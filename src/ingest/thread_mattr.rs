// Not a class-level ivar like `mattr_accessor`: two requests served at once would read each other's `Current.account`.
use crate::dialect::LibraryClass;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;

const DECLS: &[&str] = &[
    "thread_mattr_accessor",
    "thread_mattr_reader",
    "thread_mattr_writer",
    "thread_cattr_accessor",
    "thread_cattr_reader",
    "thread_cattr_writer",
];

struct Decl {
    name: Symbol,
    reader: bool,
    writer: bool,
    /// `default:`, answered by a read in a thread that never wrote the
    /// attribute (Rails 8.2 `thread_mattr_reader`).
    default: Option<Expr>,
}

/// Stands for the `default:` expression in the synthesized reader until
/// the ingested body gets the authored expression.
const DEFAULT_MARK: &str = "__thread_mattr_default__";

pub fn lower_thread_mattr(app: &mut crate::App) {
    let mut states: Vec<LibraryClass> = Vec::new();
    for lc in app.library_classes.iter_mut() {
        let decls = take_decls(lc);
        if decls.is_empty() {
            continue;
        }
        let owner = lc.name.0.as_str().to_string();
        let state = format!("{}ThreadState", owner.replace("::", ""));
        let (state_src, owner_src) = synthesized_source(&owner, &state, &decls);
        match crate::ingest::ingest_library_classes(state_src.as_bytes(), "<thread_mattr>") {
            Ok(mut classes) => {
                for c in classes.iter_mut() {
                    for m in c.methods.iter_mut() {
                        if let Some(d) = decls.iter().find(|d| d.name == m.name) {
                            if let Some(default) = &d.default {
                                put_default(&mut m.body, default);
                            }
                        }
                    }
                }
                states.extend(classes)
            }
            Err(err) => {
                super::survey::record(&err);
                continue;
            }
        }
        match crate::ingest::ingest_library_classes(owner_src.as_bytes(), "<thread_mattr>") {
            Ok(classes) => {
                for c in classes {
                    lc.methods.extend(c.methods);
                }
            }
            Err(err) => super::survey::record(&err),
        }
    }
    app.library_classes.extend(states);
}

// Options are taken as Rails 8.2 reads them: `default:` answers a read
// in a thread that never wrote the attribute, and `instance_accessor:
// false` (or `instance_reader:`/`instance_writer:` false) adds no
// instance accessor, which is all the lowering writes. Not taken when
// an instance accessor is asked for, an option is not a literal flag,
// or the default could differ evaluated per read (a mutable literal,
// a block, a call on the implicit self), which Rails evaluates once.
fn take_decls(lc: &mut LibraryClass) -> Vec<Decl> {
    let mut out = Vec::new();
    lc.unknown_calls.retain(|call| {
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*call.node else { return true };
        let kw = method.as_str();
        if !DECLS.contains(&kw) {
            return true;
        }
        let mut names = Vec::new();
        let mut options: Option<&Vec<(Expr, Expr)>> = None;
        for (i, a) in args.iter().enumerate() {
            match &*a.node {
                ExprNode::Lit { value: Literal::Sym { value } } => names.push(value.clone()),
                ExprNode::Hash { entries, kwargs: true } if i + 1 == args.len() => options = Some(entries),
                _ => return true,
            }
        }
        let mut default = None;
        if let Some(entries) = options {
            let (mut accessor, mut reader, mut writer) = (true, true, true);
            for (k, v) in entries {
                let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else { return true };
                let flag = match &*v.node {
                    ExprNode::Lit { value: Literal::Bool { value } } => Some(*value),
                    _ => None,
                };
                match (key.as_str(), flag) {
                    ("default", _) if constant_default(v) => default = Some(v.clone()),
                    ("instance_accessor", Some(f)) => accessor = f,
                    ("instance_reader", Some(f)) => reader = f,
                    ("instance_writer", Some(f)) => writer = f,
                    _ => return true,
                }
            }
            let instance_reader = accessor && reader && !kw.ends_with("_writer");
            let instance_writer = accessor && writer && !kw.ends_with("_reader");
            if instance_reader || instance_writer {
                return true;
            }
        }
        for name in names {
            out.push(Decl {
                name,
                reader: !kw.ends_with("_writer"),
                writer: !kw.ends_with("_reader"),
                default: default.clone(),
            });
        }
        false
    });
    out
}

/// A default whose every evaluation answers what one does: literals
/// that are not mutable containers, constants, and calls without a
/// block on such receivers (`!Rails.env.test?`).
fn constant_default(e: &Expr) -> bool {
    match &*e.node {
        ExprNode::Lit { .. } | ExprNode::Const { .. } => true,
        ExprNode::Send { recv: Some(recv), args, block: None, .. } => {
            constant_default(recv) && args.iter().all(constant_default)
        }
        _ => false,
    }
}

fn put_default(body: &mut Expr, default: &Expr) {
    if matches!(&*body.node, ExprNode::Send { recv: None, method, .. } if method.as_str() == DEFAULT_MARK) {
        *body = default.clone();
        return;
    }
    body.node.for_each_child_mut(&mut |child| put_default(child, default));
}

fn synthesized_source(owner: &str, state: &str, decls: &[Decl]) -> (String, String) {
    let mut state_src = format!("class {state}\n");
    for d in decls {
        let n = d.name.as_str();
        if d.default.is_some() {
            state_src.push_str(&format!(
                "  def {n}\n    return @{n} if @{n}_written\n\n    {DEFAULT_MARK}\n  end\n\n  def {n}=(value)\n    @{n}_written = true\n    @{n} = value\n  end\n\n"
            ));
        } else {
            state_src.push_str(&format!("  def {n}\n    @{n}\n  end\n\n  def {n}=(value)\n    @{n} = value\n  end\n\n"));
        }
    }
    state_src.push_str("end\n");

    let key = format!("__thread_mattr_{}", owner.replace("::", "_"));
    let mut owner_src = format!("module {owner}\n");
    // Not `nil?`-guarded: `Thread#[]` answers untyped, and only an `is_a?` test narrows the slot to the state class.
    owner_src.push_str(&format!(
        "  def self.__thread_state\n    s = Thread.current[:{key}]\n    return s if s.is_a?({state})\n    s = {state}.new\n    Thread.current[:{key}] = s\n    s\n  end\n\n"
    ));
    for d in decls {
        let n = d.name.as_str();
        if d.reader {
            owner_src.push_str(&format!("  def self.{n}\n    __thread_state.{n}\n  end\n\n"));
        }
        if d.writer {
            owner_src.push_str(&format!("  def self.{n}=(value)\n    __thread_state.{n} = value\n  end\n\n"));
        }
    }
    owner_src.push_str("end\n");
    (state_src, owner_src)
}
