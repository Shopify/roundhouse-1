// Not a class-level ivar like `mattr_accessor`: two requests served at once would read each other's `Current.account`.
use crate::dialect::LibraryClass;
use crate::expr::{ExprNode, Literal};
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
    instance_reader: bool,
    instance_writer: bool,
}

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
            Ok(classes) => states.extend(classes),
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

// Not taken when `default:` or any other option is passed: `default:` changes what a read answers.
// `instance_accessor:`/`instance_reader:`/`instance_writer:` `false` only drop the instance methods.
fn take_decls(lc: &mut LibraryClass) -> Vec<Decl> {
    let mut out = Vec::new();
    let defined: Vec<Symbol> = lc
        .methods
        .iter()
        .filter(|m| matches!(m.receiver, crate::dialect::MethodReceiver::Instance))
        .map(|m| m.name.clone())
        .collect();
    lc.unknown_calls.retain(|call| {
        let ExprNode::Send { recv: None, method, args, .. } = &*call.node else { return true };
        let kw = method.as_str();
        if !DECLS.contains(&kw) {
            return true;
        }
        let mut names = Vec::new();
        let (mut instance_reader, mut instance_writer) = (true, true);
        for (i, a) in args.iter().enumerate() {
            match &*a.node {
                ExprNode::Lit { value: Literal::Sym { value } } => names.push(value.clone()),
                ExprNode::Hash { entries, .. } if i + 1 == args.len() => {
                    for (k, v) in entries {
                        let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else { return true };
                        let ExprNode::Lit { value: Literal::Bool { value: false } } = &*v.node else { return true };
                        match key.as_str() {
                            "instance_accessor" => (instance_reader, instance_writer) = (false, false),
                            "instance_reader" => instance_reader = false,
                            "instance_writer" => instance_writer = false,
                            _ => return true,
                        }
                    }
                }
                _ => return true,
            }
        }
        for name in names {
            let reader = !kw.ends_with("_writer");
            let writer = !kw.ends_with("_reader");
            let setter = Symbol::from(format!("{}=", name.as_str()));
            out.push(Decl {
                instance_reader: reader && instance_reader && !defined.contains(&name),
                instance_writer: writer && instance_writer && !defined.contains(&setter),
                name,
                reader,
                writer,
            });
        }
        false
    });
    out
}

fn synthesized_source(owner: &str, state: &str, decls: &[Decl]) -> (String, String) {
    let mut state_src = format!("class {state}\n");
    for d in decls {
        let n = d.name.as_str();
        state_src.push_str(&format!("  def {n}\n    @{n}\n  end\n\n  def {n}=(value)\n    @{n} = value\n  end\n\n"));
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
        // Rails' instance accessors read the same per-thread slot as the owner's.
        if d.instance_reader {
            owner_src.push_str(&format!("  def {n}\n    ::{owner}.{n}\n  end\n\n"));
        }
        if d.instance_writer {
            owner_src.push_str(&format!("  def {n}=(value)\n    ::{owner}.{n} = value\n  end\n\n"));
        }
    }
    owner_src.push_str("end\n");
    (state_src, owner_src)
}
