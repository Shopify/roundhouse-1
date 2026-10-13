//! `return @x if defined?(@x)` — Rails' memoisation guard for a value
//! that may be nil — lowered to a presence flag the strict targets can
//! carry.
//!
//! CRuby answers `defined?(@x)` from the object: nil until the ivar is
//! first assigned. On a struct-backed target every ivar exists from
//! allocation, so there is nothing to consult — spinel folds the test
//! at compile time (docs/limitations.md, "`defined?(@ivar)` is answered
//! at compile time") and the guard reads the unassigned slot forever:
//! campfire's `Opengraph::Location#parsed_url` answered nil for every
//! URL, so every location was "invalid" and nothing was ever unfurled.
//! The other strict emitters have no `defined?` at all.
//!
//! The rewrite is the one that document prescribes, done once here
//! instead of by hand in each app: `defined?(@x)` becomes a read of
//! `@x_defined`, and every assignment to `@x` in the app sets the
//! flag after the value lands —
//!
//! ```ruby
//! return @x if defined?(@x)      # → return @x if @x_defined
//! @x = compute                   # → @x = compute; @x_defined = true; @x
//! ```
//!
//! The flag is a bool ivar (false — or nil, on the ruby family — until
//! set; either is falsy), so the transformed program is the same
//! program on CRuby and the intended one everywhere else. The value's
//! assignment keeps its position, and the flag is set AFTER it, so a
//! `compute` that raises leaves the object as CRuby would: unassigned.
//!
//! Scope and refusal. The guard is rewritten only when EVERY assignment
//! to that ivar anywhere in the app sits in statement position (a `Seq`
//! element or a body's sole expression), where the three-statement
//! replacement is an ordinary sequence. An assignment nested inside an
//! expression the pass cannot expand leaves the `defined?` untouched:
//! a guard that stays `defined?` fails the way it does today, which is
//! honest, where a flag one writer never sets would answer wrong. So
//! does any other writer of the ivar: a compound assignment
//! (`@x ||= {}`), a multiple assignment, `instance_variable_set` or
//! `remove_instance_variable` naming it. frozen_record's
//! `TestHelper.load_fixture` sets `@cache ||= {}`; `unload_fixtures`'
//! `return unless defined?(@cache) && @cache` read a flag nothing set,
//! so it never restored a model's `base_path`, and Core's ApiChange
//! kept the 29 test fixtures after `reset_api_versioning_test_setup`.

use std::collections::BTreeSet;

use crate::app::App;
use crate::dialect::{ControllerBodyItem, ModelBodyItem};
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::Symbol;
use crate::ty::Ty;

pub fn apply_defined_ivar_memo_lowering(app: &mut App) {
    // One pool for the whole app, not one per class: an ivar is the
    // receiver's, and the receiver's writers need not live in the class
    // that guards it. A module's `defined?(@x)` reads whatever the host
    // assigned — Core's `GraphApi::HasNestedBreadthResolver#breadth_resolver`
    // guards `@breadth_resolver`, which the extending class's inherited
    // `Resolver.breadth_resolver=` sets — and a superclass or subclass
    // writer is just as invisible to a per-class scan. Pooled, every
    // writer of the name sets the flag (a flag on an unrelated class is
    // an inert extra ivar) and any one writer the flag cannot follow
    // keeps every guard of that name a real `defined?`.
    let mut bodies: Vec<&mut Expr> = Vec::new();
    for model in &mut app.models {
        for item in &mut model.body {
            match item {
                ModelBodyItem::Method { method, .. } => bodies.push(&mut method.body),
                ModelBodyItem::Scope { scope, .. } => bodies.push(&mut scope.body),
                _ => {}
            }
        }
    }
    for lc in &mut app.library_classes {
        bodies.extend(lc.methods.iter_mut().map(|m| &mut m.body));
    }
    if let Some(lc) = &mut app.rails_application {
        bodies.extend(lc.methods.iter_mut().map(|m| &mut m.body));
    }
    for controller in &mut app.controllers {
        for item in &mut controller.body {
            if let ControllerBodyItem::Action { action, .. } = item {
                bodies.push(&mut action.body);
            }
        }
    }
    rewrite_class(bodies);
}

fn rewrite_class(mut bodies: Vec<&mut Expr>) {
    let mut guarded: BTreeSet<Symbol> = BTreeSet::new();
    for body in bodies.iter() {
        collect_guarded(body, &mut guarded);
    }
    if guarded.is_empty() {
        return;
    }
    // An ivar whose every assignment can take the flag.
    let mut blocked: BTreeSet<Symbol> = BTreeSet::new();
    for body in bodies.iter() {
        collect_blocked(body, true, &mut blocked);
    }
    let rewritable: BTreeSet<Symbol> = guarded.difference(&blocked).cloned().collect();
    if rewritable.is_empty() {
        return;
    }
    for body in bodies.iter_mut() {
        rewrite_guards(body, &rewritable);
        rewrite_assignments(body, &rewritable, true, true);
    }
}

fn flag_name(name: &Symbol) -> Symbol {
    Symbol::from(format!("{}_defined", name.as_str()).as_str())
}

fn collect_guarded(e: &Expr, out: &mut BTreeSet<Symbol>) {
    if let ExprNode::Send { recv: None, method, args, .. } = &*e.node {
        if method.as_str() == "defined?" && args.len() == 1 {
            if let ExprNode::Ivar { name } = &*args[0].node {
                out.insert(name.clone());
            }
        }
    }
    e.node.for_each_child(&mut |c| collect_guarded(c, out));
}

/// Every ivar some `@name = …` in `e` assigns outside statement
/// position — `statement` says whether `e` itself is one. A `Seq`'s
/// elements are; so is a body's sole expression; a branch of an `If`
/// is; an assignment anywhere else is a value some emitter would have
/// to expand in place. Compound, multiple and reflective writers block
/// their ivar wherever they sit.
fn collect_blocked(e: &Expr, statement: bool, out: &mut BTreeSet<Symbol>) {
    match &*e.node {
        ExprNode::Assign { target: LValue::Ivar { name }, value } => {
            if !statement {
                out.insert(name.clone());
            }
            collect_blocked(value, false, out);
            return;
        }
        ExprNode::OpAssign { target: LValue::Ivar { name }, .. } => {
            out.insert(name.clone());
        }
        ExprNode::MultiAssign { targets, .. } => {
            for t in targets {
                if let LValue::Ivar { name } = t {
                    out.insert(name.clone());
                }
            }
        }
        ExprNode::Send { method, args, .. }
            if matches!(method.as_str(), "instance_variable_set" | "remove_instance_variable") =>
        {
            if let Some(name) = args.first().and_then(literal_ivar_name) {
                out.insert(name);
            }
        }
        ExprNode::Seq { exprs } => {
            for x in exprs {
                collect_blocked(x, true, out);
            }
            return;
        }
        ExprNode::If { cond, then_branch, else_branch } => {
            collect_blocked(cond, false, out);
            collect_blocked(then_branch, true, out);
            collect_blocked(else_branch, true, out);
            return;
        }
        _ => {}
    }
    e.node.for_each_child(&mut |c| collect_blocked(c, false, out));
}

/// `arg` is an ivar's name as a literal (`:@x`, `"@x"`), returned
/// without the sigil as ivars are stored.
fn literal_ivar_name(arg: &Expr) -> Option<Symbol> {
    let text = match &*arg.node {
        ExprNode::Lit { value: Literal::Sym { value } } => value.as_str(),
        ExprNode::Lit { value: Literal::Str { value } } => value.as_str(),
        _ => return None,
    };
    text.strip_prefix('@').map(Symbol::from)
}

fn rewrite_guards(e: &mut Expr, names: &BTreeSet<Symbol>) {
    e.node.for_each_child_mut(&mut |c| rewrite_guards(c, names));
    let span = e.span;
    let replacement = match &*e.node {
        ExprNode::Send { recv: None, method, args, .. }
            if method.as_str() == "defined?" && args.len() == 1 =>
        {
            match &*args[0].node {
                ExprNode::Ivar { name } if names.contains(name) => Some(flag_name(name)),
                _ => None,
            }
        }
        _ => None,
    };
    if let Some(flag) = replacement {
        let mut read = Expr::new(span, ExprNode::Ivar { name: flag });
        read.ty = Some(Ty::Union { variants: vec![Ty::Bool, Ty::Nil].into() });
        *e = read;
    }
}

/// `used`: the statement's value is read (a body's or branch's last
/// expression). Where it is not, the rewrite ends at the flag: a bare
/// trailing `@x` in void context is a CRuby warning ("possibly useless
/// use of a variable"), which Core's StrictWarning raises.
fn rewrite_assignments(e: &mut Expr, names: &BTreeSet<Symbol>, statement: bool, used: bool) {
    match &mut *e.node {
        ExprNode::Seq { exprs } => {
            let last = exprs.len().saturating_sub(1);
            for (i, x) in exprs.iter_mut().enumerate() {
                rewrite_assignments(x, names, true, used && i == last);
            }
        }
        ExprNode::If { cond, then_branch, else_branch } => {
            rewrite_assignments(cond, names, false, true);
            rewrite_assignments(then_branch, names, true, used);
            rewrite_assignments(else_branch, names, true, used);
        }
        ExprNode::Assign { target: LValue::Ivar { name }, value } if statement && names.contains(name) => {
            rewrite_assignments(value, names, false, true);
            let span = e.span;
            let name = name.clone();
            let ty = e.ty.clone();
            let assign = std::mem::replace(e, Expr::new(span, ExprNode::Lit { value: Literal::Nil }));
            let mut flag_true = Expr::new(span, ExprNode::Lit { value: Literal::Bool { value: true } });
            flag_true.ty = Some(Ty::Bool);
            let mut set_flag = Expr::new(
                span,
                ExprNode::Assign { target: LValue::Ivar { name: flag_name(&name) }, value: flag_true },
            );
            set_flag.ty = Some(Ty::Bool);
            let mut seq = if used {
                let mut read = Expr::new(span, ExprNode::Ivar { name });
                read.ty = ty.clone();
                Expr::new(span, ExprNode::Seq { exprs: vec![assign, set_flag, read] })
            } else {
                Expr::new(span, ExprNode::Seq { exprs: vec![assign, set_flag] })
            };
            seq.ty = if used { ty } else { Some(Ty::Bool) };
            *e = seq;
        }
        _ => {
            e.node.for_each_child_mut(&mut |c| rewrite_assignments(c, names, false, true));
        }
    }
}
