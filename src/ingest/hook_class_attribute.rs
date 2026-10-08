//! A `class_attribute` that a module's `self.included(base)` hook
//! declares on the class including it.
//!
//! activemerchant 1.137.0's `PostsData`:
//!
//! ```ruby
//! def self.included(base)
//!   base.class_attribute :ssl_strict
//!   base.ssl_strict = true
//!   ...
//! ```
//!
//! Ruby runs the hook at `include PostsData`. There ActiveSupport's
//! `Class#class_attribute` defines the accessors on `base` and sets
//! `@__class_attr_ssl_strict` on it to the default. Ingest expands a
//! class-body `class_attribute` into plain defs
//! (`class_attribute::methods`). The emitted tree defines no
//! `Class#class_attribute`, so the hook raised NoMethodError at the
//! include, and `ActiveMerchant::Billing::Gateway` never loaded.
//!
//! Each app class that includes the module directly gets the defs a
//! class-body declaration gives it, ahead of its other methods. The Ruby
//! family writes the include after the class methods its hook calls on
//! `base` (`emit::ruby::library::hooked_include_position`), so the hook
//! finds the writer it calls next. The hook's declaration statement
//! becomes the rest of what `class_attribute` does at that point: the
//! seed on `base`, `base.instance_variable_set(:@__class_attr_name,
//! default)`. Later names of a multi-name declaration with a default
//! share its one value, as ActiveSupport stores it. The hook's other
//! statements run as written.
//!
//! A hook may ask first: activemerchant's `PaypalExpressCommon` runs
//! `if base.respond_to?(:class_attribute)` with its declarations, and
//! else `base.class_inheritable_accessor` (Rails 2). Natively that is
//! true, because ActiveSupport's `class/attribute` is loaded, and this
//! expansion is what answers `class_attribute` here. So a top-level `if`
//! with that condition whose branch holds only declarations is that
//! branch.
//!
//! The pass is all or nothing per module. A hook keeps its text when it
//! has a `class_attribute` call this does not read, or when no app class
//! includes the module. An unread call is one that is not a top-level
//! statement on `base`, or has computed names, a computed flag, a block
//! or an unknown option.

use crate::App;
use crate::dialect::{MethodDef, MethodReceiver};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;

use super::class_attribute::{Decl, storage_ivar};

pub(super) fn expand(app: &mut App) {
    let mut planned: Vec<(usize, usize, Vec<(usize, Declaration)>)> = Vec::new();
    for (mi, m) in app.library_classes.iter().enumerate() {
        if !m.is_module || m.origin.is_some() {
            continue;
        }
        let Some(hi) =
            m.methods.iter().position(|d| d.receiver == MethodReceiver::Class && d.name.as_str() == "included")
        else {
            continue;
        };
        if let Some(decls) = hook_declarations(&m.methods[hi])
            && !decls.is_empty()
        {
            planned.push((mi, hi, decls));
        }
    }
    for (mi, hi, decls) in planned {
        let module = app.library_classes[mi].name.clone();
        let includers: Vec<usize> = app
            .library_classes
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.is_module && c.origin.is_none())
            .filter(|(_, c)| {
                c.includes.iter().any(|inc| {
                    super::class_eval_macro::resolve_in_ancestry(c, inc.0.as_str(), &app.library_classes)
                        .is_some_and(|found| found.name == module)
                })
            })
            .map(|(i, _)| i)
            .collect();
        if includers.is_empty() {
            continue;
        }
        for i in includers {
            let owner = app.library_classes[i].name.clone();
            let mut added: Vec<MethodDef> = Vec::new();
            for decl in decls.iter().flat_map(|(_, d)| d.decls()) {
                for method in super::class_attribute::methods(&owner, decl) {
                    let own = &app.library_classes[i].methods;
                    if own.iter().chain(added.iter()).any(|d| d.name == method.name && d.receiver == method.receiver) {
                        continue;
                    }
                    added.push(method);
                }
            }
            app.library_classes[i].methods.splice(0..0, added);
        }
        let hook = &mut app.library_classes[mi].methods[hi];
        let mut statements = match &*hook.body.node {
            ExprNode::Seq { exprs } => exprs.clone(),
            _ => vec![hook.body.clone()],
        };
        let base = Expr::new(
            hook.body.span,
            ExprNode::Var { id: crate::ident::VarId(0), name: hook.params[0].name.clone() },
        );
        for (at, declaration) in decls.iter().rev() {
            let seeds: Vec<Expr> = declaration.decls().iter().flat_map(|decl| seeds(&base, decl)).collect();
            statements.splice(*at..*at + 1, seeds);
        }
        let span = hook.body.span;
        hook.body = Expr::new(span, ExprNode::Seq { exprs: statements });
    }
}

/// The hook's top-level `base.class_attribute` declarations, by statement
/// index. `None` when the hook has a `class_attribute` call that is not
/// one of them, or a parameter list other than one plain `base`.
fn hook_declarations(hook: &MethodDef) -> Option<Vec<(usize, Declaration)>> {
    let [base] = hook.params.as_slice() else { return None };
    if base.keyword || base.rest || base.forwarding || base.default.is_some() {
        return None;
    }
    let statements: Vec<&Expr> = match &*hook.body.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![&hook.body],
    };
    let mut out = Vec::new();
    for (i, statement) in statements.iter().enumerate() {
        if let Some(decl) = declaration(statement, &base.name) {
            out.push((i, Declaration::One(decl)));
        } else if let Some(decls) = guarded_declarations(statement, &base.name) {
            out.push((i, Declaration::Guarded(decls)));
        }
    }
    fn count(e: &Expr, n: &mut usize) {
        if let ExprNode::Send { method, .. } = &*e.node
            && method.as_str() == "class_attribute"
        {
            *n += 1;
        }
        e.node.for_each_child(&mut |c| count(c, n));
    }
    let mut calls = 0;
    count(&hook.body, &mut calls);
    let read: usize = out.iter().map(|(_, d)| d.decls().len()).sum();
    let unread_guard = out.iter().filter_map(|(at, d)| matches!(d, Declaration::Guarded(_)).then_some(*at)).map(|at| {
        let ExprNode::If { else_branch, .. } = &*statements[at].node else { return 0 };
        let mut n = 0;
        count(else_branch, &mut n);
        n
    });
    (calls == read + unread_guard.sum::<usize>()).then_some(out)
}

/// A hook statement this pass rewrites: one declaration, or a guard
/// on `base.respond_to?(:class_attribute)` whose branch is declarations.
enum Declaration {
    One(Decl),
    Guarded(Vec<Decl>),
}

impl Declaration {
    fn decls(&self) -> &[Decl] {
        match self {
            Declaration::One(decl) => std::slice::from_ref(decl),
            Declaration::Guarded(decls) => decls,
        }
    }
}

/// `if base.respond_to?(:class_attribute) then <declarations> else … end`:
/// the then-branch's declarations.
fn guarded_declarations(statement: &Expr, base: &Symbol) -> Option<Vec<Decl>> {
    let ExprNode::If { cond, then_branch, .. } = &*statement.node else { return None };
    let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*cond.node else { return None };
    if method.as_str() != "respond_to?"
        || !matches!(&*recv.node, ExprNode::Var { name, .. } if name == base)
        || !matches!(args.as_slice(), [arg] if matches!(&*arg.node,
            ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "class_attribute"))
    {
        return None;
    }
    let branch: Vec<&Expr> = match &*then_branch.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![then_branch],
    };
    let decls: Vec<Decl> = branch.iter().map(|s| declaration(s, base)).collect::<Option<_>>()?;
    (!decls.is_empty()).then_some(decls)
}

/// `base.class_attribute(:a, :b, instance_reader: false, default: x)`,
/// with literal names and flags, as a [`Decl`].
fn declaration(statement: &Expr, base: &Symbol) -> Option<Decl> {
    let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*statement.node else {
        return None;
    };
    if method.as_str() != "class_attribute" || !matches!(&*recv.node, ExprNode::Var { name, .. } if name == base) {
        return None;
    }
    let mut names: Vec<Symbol> = Vec::new();
    let mut instance_accessor: Option<bool> = None;
    let mut instance_reader: Option<bool> = None;
    let mut instance_writer: Option<bool> = None;
    let mut instance_predicate: Option<bool> = None;
    let mut default: Option<Expr> = None;
    for (i, arg) in args.iter().enumerate() {
        match &*arg.node {
            ExprNode::Lit { value: Literal::Sym { value } } => names.push(value.clone()),
            ExprNode::Lit { value: Literal::Str { value } } => names.push(Symbol::from(value.as_str())),
            ExprNode::Hash { entries, .. } if i + 1 == args.len() => {
                for (key, value) in entries {
                    let ExprNode::Lit { value: Literal::Sym { value: key } } = &*key.node else { return None };
                    let slot = match key.as_str() {
                        "instance_accessor" => &mut instance_accessor,
                        "instance_reader" => &mut instance_reader,
                        "instance_writer" => &mut instance_writer,
                        "instance_predicate" => &mut instance_predicate,
                        "default" if default.is_none() => {
                            default = Some(value.clone());
                            continue;
                        }
                        _ => return None,
                    };
                    let flag = match &*value.node {
                        ExprNode::Lit { value: Literal::Bool { value } } => *value,
                        ExprNode::Lit { value: Literal::Nil } => false,
                        _ => return None,
                    };
                    if slot.replace(flag).is_some() {
                        return None;
                    }
                }
            }
            _ => return None,
        }
    }
    if names.is_empty() || names.iter().any(|n| !super::class_attribute::is_method_name(n.as_str())) {
        return None;
    }
    let accessor = instance_accessor.unwrap_or(true);
    Some(Decl {
        names,
        instance_reader: instance_reader.unwrap_or(accessor),
        instance_writer: instance_writer.unwrap_or(accessor),
        instance_predicate: instance_predicate.unwrap_or(true),
        default,
        span: statement.span,
    })
}

/// `base.instance_variable_set(:@__class_attr_<name>, value)` per name.
/// The first name takes the default (nil without one). Each later name
/// reads the first's ivar back, so a default is evaluated once and its
/// one object is shared, as ActiveSupport stores it.
fn seeds(base: &Expr, decl: &Decl) -> Vec<Expr> {
    let span = decl.span;
    let send = |method: &str, args: Vec<Expr>| {
        Expr::new(
            span,
            ExprNode::Send {
                recv: Some(base.clone()),
                method: Symbol::from(method),
                args,
                block: None,
                parenthesized: true,
            },
        )
    };
    let ivar = |name: &Symbol| {
        Expr::new(
            span,
            ExprNode::Lit { value: Literal::Sym { value: Symbol::from(format!("@{}", storage_ivar(name).as_str())) } },
        )
    };
    let first = &decl.names[0];
    decl.names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let value = match &decl.default {
                Some(_) if i > 0 => send("instance_variable_get", vec![ivar(first)]),
                Some(default) => default.clone(),
                None => Expr::new(span, ExprNode::Lit { value: Literal::Nil }),
            };
            send("instance_variable_set", vec![ivar(name), value])
        })
        .collect()
}
