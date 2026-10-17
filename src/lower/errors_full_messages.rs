//! `record.errors.full_messages` → `record.errors`.
//!
//! The shared framework runtime's `errors` is already the array of
//! full-message strings — `validates` bakes humanized text at lower
//! time ("Short can't be blank") and hand-written `errors.add` calls
//! ground into the same shape (see [`super::errors_add`], which owns
//! that half and documents the accumulator invariant). Rails' extra
//! `.full_messages` hop is therefore an identity on our runtime, and
//! leaving it in place is not harmless: `Array[String]` has no
//! `full_messages`, so every strict target gets a hard unresolved-method
//! error at a site whose meaning we already know.
//!
//! Scope is `for_each_hook_body`, view bodies, and TEST bodies — the
//! construct has real presence in all three (lobsters reads it from
//! controllers, from models, and from an `application_helper`
//! error-list builder that views call). That matches
//! [`super::exclude_predicate`], the other total rewrite with a view
//! presence.
//!
//! Tests were the last to join, and asking for them by name is the
//! convention [`super::for_each_test_body`] documents. The site is
//! campfire's `assert_equal [ "Title can't be blank", … ],
//! opengraph.errors.full_messages` — an assertion reading the
//! accumulator directly, which is exactly the shape this pass folds
//! everywhere else. Left out, it read as a missing method on `Array`
//! and the identity we already knew went unapplied purely because of
//! which walk the pass had asked for.
//!
//! An equivalent rewrite already existed as `rewrite_errors_full_messages`
//! in the Roda ingest, applied to view bodies only. That copy is left in
//! place deliberately: it runs at INGEST, this runs post-analyze, and
//! the fold is idempotent (after either pass no `full_messages` send
//! survives for the other to match), so the lanes can overlap without
//! interfering. Retiring it is a separate cleanup with its own gate.

use crate::app::App;
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};

pub fn apply_errors_full_messages_lowering(app: &mut App) {
    if app_defines_full_messages(app) {
        return;
    }
    let port = super::active_model_model::ValidationsPort::of(app);
    super::for_each_owned_hook_body(app, &mut |owner, body| {
        let locals = port.locals(body);
        rewrite(body, &(&port, owner, &locals))
    });
    super::for_each_test_body(app, &mut |body| {
        let locals = port.locals(body);
        rewrite(body, &(&port, None, &locals))
    });
    for view in &mut app.views {
        let locals = port.locals(&view.body);
        rewrite(&mut view.body, &(&port, None, &locals));
    }
}

/// Does any app class define its own `full_messages`? Then the name
/// doesn't mean Rails' and the pass stands down, rather than folding
/// away a real call. Mirrors `exclude_predicate`'s guard.
pub(crate) fn app_defines_full_messages(app: &App) -> bool {
    let is_fm = |n: &Symbol| n.as_str() == "full_messages";
    app.models.iter().any(|m| {
        m.body.iter().any(|item| {
            matches!(item, crate::dialect::ModelBodyItem::Method { method, .. }
                if is_fm(&method.name))
        })
    }) || app
        .library_classes
        .iter()
        .any(|lc| lc.methods.iter().any(|m| is_fm(&m.name)))
}

/// The ActiveModel::Validations port's classes, the body's owner and
/// the body's locals that may hold a port record.
pub(crate) type Site<'a> =
    (&'a super::active_model_model::ValidationsPort, Option<&'a ClassId>, &'a super::active_model_model::PortLocals);

fn rewrite(expr: &mut Expr, site: &Site<'_>) {
    expr.node.for_each_child_mut(&mut |c| rewrite(c, site));
    rewrite_node(expr, site);
}

/// `owner` is the class whose body this is; an `errors` that `port`
/// answers (a class over the ActiveModel::Validations port, reading
/// its own or another such record's) keeps Rails' `full_messages`.
pub(crate) fn rewrite_node(expr: &mut Expr, &(port, owner, locals): &Site<'_>) {
    // `<recv>.errors.full_messages` — zero-arg, block-free on both hops.
    // The receiver being an `errors` reader is what makes this our
    // accumulator rather than some other object's method of the same
    // name; the receiver of `errors` itself is unconstrained, so both
    // `errors.full_messages` (a model validating itself) and
    // `record.errors.full_messages` (a controller reading another
    // object's) fold, and the surviving expression keeps that receiver.
    let replacement = match &*expr.node {
        ExprNode::Send { recv: Some(r), method, args, block: None, .. }
            if method.as_str() == "full_messages"
                && args.is_empty()
                && matches!(
                    &*r.node,
                    ExprNode::Send { method: em, args: ea, block: None, .. }
                        if em.as_str() == "errors" && ea.is_empty()
                )
                && !port.answers(r, owner) =>
        {
            match port.classify(r, owner, locals) {
                super::active_model_model::Reading::Refused => {
                    port.refuse(expr, "errors.full_messages");
                    return;
                }
                super::active_model_model::Reading::Untyped => {
                    port.note_untyped("errors_full_messages", "errors.full_messages", expr.span)
                }
                _ => {}
            }
            Some((*r.node).clone())
        }
        _ => None,
    };
    if let Some(node) = replacement {
        expr.node = Box::new(node);
        return;
    }
    fold_safe_nav(expr);
}

/// `x.errors&.full_messages`, which `&.` binds as
/// `(__safe_nav1 = x.errors) && __safe_nav1.full_messages`, and
/// `x&.errors&.full_messages`, where the bound value is itself
/// `x && x.errors`: the hop is the same identity, and it is nil-safe on
/// its own (nil `errors` answers nil either way), so the binding goes
/// with it. `e.record&.errors&.full_messages&.first` reads the messages
/// whatever `e.record`'s type, as the `.` spelling does.
fn fold_safe_nav(expr: &mut Expr) {
    use crate::expr::{BoolOpKind, LValue};
    let is_errors = |e: &Expr| {
        matches!(&*e.node, ExprNode::Send { method, args, block: None, .. }
            if method.as_str() == "errors" && args.is_empty())
    };
    let ExprNode::BoolOp { op: BoolOpKind::And, left, right, .. } = &*expr.node else { return };
    let ExprNode::Assign { target: LValue::Var { name: temp, .. }, value } = &*left.node else { return };
    let ExprNode::Send { recv: Some(read), method, args, block: None, .. } = &*right.node else { return };
    if method.as_str() != "full_messages"
        || !args.is_empty()
        || !matches!(&*read.node, ExprNode::Var { name, .. } if name == temp)
    {
        return;
    }
    let folds = is_errors(value)
        || matches!(&*value.node, ExprNode::BoolOp { op: BoolOpKind::And, right: inner, .. } if is_errors(inner));
    if folds {
        let value = value.clone();
        *expr = value;
    }
}
