//! `errors[:field]` grounding: `<recv>.errors[:field]` →
//! `ActiveSupport.errors_for(<recv>.errors, "Field ")`.
//!
//! The shared runtime's error accumulator is a plain `Array[String]` of
//! FULL messages — [`super::errors_add`] owns that invariant and bakes
//! `"#{humanized_field} #{message}"` at lower time, and
//! [`super::errors_full_messages`] folds Rails' `full_messages` hop
//! away because on our shape it is the identity. Rails' `errors[:field]`
//! is the other projection of the same data: the messages for one
//! attribute, WITHOUT the humanized prefix (`[ "is not public" ]`, not
//! `[ "Url is not public" ]`).
//!
//! Left alone the site is not merely unsupported, it is silently wrong
//! on CRuby too: `Array#[]` with a Symbol raises `TypeError: no implicit
//! conversion of Symbol into Integer`, which is what campfire's
//! `Opengraph::LocationTest` reported for three tests. So the pass
//! rewrites to a helper that re-derives the projection from the baked
//! prefix — the same "one shared `runtime/ruby` body prices every
//! target" placement [`super::presence_in`] documents, in the same
//! `ActiveSupport` module.
//!
//! WHAT THE PREFIX RE-DERIVATION CANNOT SEE. The accumulator keeps no
//! attribute column, so the helper matches on text. If one attribute's
//! humanized name is a PREFIX of another's — `url` and `url_host`
//! humanize to `"Url"` and `"Url host"` — then `errors[:url]` also
//! matches `"Url host can't be blank"` and answers `"host can't be
//! blank"`. That is a real divergence from Rails and it is recorded in
//! `docs/pipeline/runtime.md`; the fix is an attribute column on the
//! accumulator, which changes `@errors`' type in every strict target
//! and is not worth it until a corpus site needs it.
//!
//! `:base` DECLINES. Rails attaches `:base` messages to the record
//! rather than a field, and `errors_add` accordingly bakes them with NO
//! prefix — there is no text for the helper to match on, and every
//! prefixed message would be a false negative. Those sites join the
//! residue ledger instead.
//!
//! Purely shape-directed, like its two siblings: the receiver of `[]`
//! must itself be a zero-arg `errors` send, which is what makes the
//! site our accumulator rather than some other object's `[]`.
//!
//! VIEWS ARE DELIBERATELY OUT OF SCOPE, unlike
//! [`super::errors_full_messages`]. The view lowering already owns this
//! exact shape: `classify_errors_field_predicate` recognizes
//! `<record>.errors[:field].none? / .any?` — the scaffold's form
//! partial styles every field from it — and grounds the whole
//! predicate, `[]` included. Rewriting the inner `[]` here runs FIRST
//! and leaves that classifier a shape it no longer matches, which takes
//! the blog's `_form.html.erb` from four grounded predicates to four
//! `unresolved_type` diagnostics. Scope is therefore hook bodies and
//! TEST bodies; a view read outside the predicate shape stays exactly
//! as unsupported as it was, which is honest, and the view lowering is
//! where it would be claimed.

use crate::app::App;
use crate::diagnostic::Diagnostic;
use super::active_model_model::Reading;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;
use crate::ty::Ty;

/// Rewrite `errors[:field]` reads across every hook body and test body
/// (NOT views — see the module docs). Returns the residue ledger:
/// recognizable `errors[…]` sites left dynamic, with the reason.
pub fn apply_errors_index_lowering(app: &mut App) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let port = super::active_model_model::ValidationsPort::of(app);
    super::for_each_owned_hook_body(app, &mut |owner, body| {
        let locals = port.locals(body);
        let site = (&port, owner.cloned(), &locals);
        rewrite(body, &site, &mut diags);
        ledger_messages(body, &site, &mut diags);
    });
    super::for_each_test_body(app, &mut |body| {
        let locals = port.locals(body);
        let site = (&port, None, &locals);
        rewrite(body, &site, &mut diags);
        ledger_messages(body, &site, &mut diags);
    });
    diags.extend(port.take_notes());
    diags
}

fn residue(expr: &Expr, reason: &str) -> Diagnostic {
    crate::lower::residue_diagnostic(
        "errors_index",
        "errors[]",
        expr.span,
        reason,
        format!(
            "`errors[:field]` left as dynamic dispatch ({reason}) — the error \
             accumulator is an Array[String] whose `[]` takes an Integer; \
             ground by hand or extend the errors_index lowering"
        ),
    )
}

/// Is this expression a zero-arg, block-free `errors` send — i.e. our
/// accumulator? The receiver of `errors` itself is unconstrained, so
/// both `errors[:x]` (a model reading its own) and `record.errors[:x]`
/// (a test reading another object's) match.
fn is_errors_read(e: &Expr) -> bool {
    matches!(
        &*e.node,
        ExprNode::Send { method, args, block: None, .. }
            if method.as_str() == "errors" && args.is_empty()
    )
}

fn is_messages_read(e: &Expr) -> bool {
    matches!(&*e.node,
        ExprNode::Send { recv: Some(r), method, args, block: None, .. }
            if method.as_str() == "messages" && args.is_empty() && is_errors_read(r))
}

fn send(span: crate::span::Span, recv: Expr, method: &str, args: Vec<Expr>, ty: Ty) -> Expr {
    let mut e = Expr::new(
        span,
        ExprNode::Send { recv: Some(recv), method: Symbol::from(method), args, block: None, parenthesized: false },
    );
    e.ty = Some(ty);
    e
}

// Not a Hash: the accumulator is the Array of full messages, so each `messages` question is asked of it directly.
fn rewrite_messages(expr: &mut Expr, diags: &mut Vec<Diagnostic>) {
    let span = expr.span;
    let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &mut *expr.node else { return };
    if !is_messages_read(r) {
        return;
    }
    let ExprNode::Send { recv: Some(errors), .. } = &*r.node else { return };
    let accumulator = errors.clone();
    let replacement = match (method.as_str(), args.as_slice()) {
        ("[]", [_]) => {
            *r = accumulator;
            None
        }
        ("key?" | "has_key?" | "include?" | "member?", [field]) => {
            let field = field.clone();
            let mut index = send(span, accumulator, "[]", vec![field], Ty::Array { elem: Box::new(Ty::Str) });
            rewrite_index(&mut index, diags);
            Some(send(span, index, "any?", vec![], Ty::Bool))
        }
        ("blank?" | "empty?", []) => Some(send(span, accumulator, "empty?", vec![], Ty::Bool)),
        ("present?" | "any?", []) => Some(send(span, accumulator, "any?", vec![], Ty::Bool)),
        _ => None,
    };
    if let Some(new) = replacement {
        *expr = new;
    }
}

/// The ActiveModel::Validations port's classes, and the body's owner.
type Site<'a> = (&'a super::active_model_model::ValidationsPort, Option<crate::ident::ClassId>, &'a super::active_model_model::PortLocals);

/// Whether the `errors` this site reads (`errors[…]`, `errors.messages`
/// or a question asked of it) is the port's ActiveModel::Errors, whose
/// `[]` and `messages` are Rails' own and stay as written.
fn on_port(expr: &Expr, site: &Site<'_>) -> bool {
    let ExprNode::Send { recv: Some(r), .. } = &*expr.node else { return false };
    let errors = if is_errors_read(r) {
        r
    } else if is_messages_read(r) {
        let ExprNode::Send { recv: Some(e), .. } = &*r.node else { return false };
        e
    } else {
        return false;
    };
    site.0.answers(errors, site.1.as_ref())
}

/// The `errors` a site `rewrite` would ground reads (`errors[x]`, or a
/// `messages` question it answers), with the site's spelling.
fn grounded_errors(expr: &Expr) -> Option<(&Expr, &'static str)> {
    let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &*expr.node else { return None };
    if method.as_str() == "[]" && args.len() == 1 && is_errors_read(r) {
        return Some((r, "errors[]"));
    }
    let known = matches!(
        (method.as_str(), args.len()),
        ("[]" | "key?" | "has_key?" | "include?" | "member?", 1) | ("blank?" | "empty?" | "present?" | "any?", 0)
    );
    if !known || !is_messages_read(r) {
        return None;
    }
    let ExprNode::Send { recv: Some(errors), .. } = &*r.node else { return None };
    Some((errors, "errors.messages"))
}

fn rewrite(expr: &mut Expr, site: &Site<'_>, diags: &mut Vec<Diagnostic>) {
    expr.node.for_each_child_mut(&mut |c| rewrite(c, site, diags));
    if on_port(expr, site) {
        return;
    }
    if let Some((errors, spelling)) = grounded_errors(expr) {
        match site.0.classify(errors, site.1.as_ref(), site.2) {
            Reading::Refused => {
                site.0.refuse(expr, spelling);
                return;
            }
            Reading::Untyped => site.0.note_untyped("errors_index", spelling, expr.span),
            Reading::Port | Reading::Accumulator => {}
        }
    }
    rewrite_messages(expr, diags);
    rewrite_index(expr, diags);
}

fn ledger_messages(expr: &mut Expr, site: &Site<'_>, diags: &mut Vec<Diagnostic>) {
    if is_messages_read(expr) && !on_port(expr, site) {
        diags.push(crate::lower::residue_diagnostic(
            "errors_index",
            "errors.messages",
            expr.span,
            "whole_hash",
            "`errors.messages` grounds only as `[]`, `key?`, `blank?`/`empty?` or `present?`/`any?`; the error \
             accumulator keeps no attribute column to build the Hash from, which the CRuby and JRuby trees \
             record (`ActiveModel::ErrorAttributes.messages`) and every other tree refuses"
                .to_string(),
        ));
    }
    expr.node.for_each_child_mut(&mut |c| ledger_messages(c, site, diags));
}

fn rewrite_index(expr: &mut Expr, diags: &mut Vec<Diagnostic>) {
    let matches_shape = matches!(
        &*expr.node,
        ExprNode::Send { recv: Some(r), method, args, block: None, .. }
            if method.as_str() == "[]" && args.len() == 1 && is_errors_read(r)
    );
    if !matches_shape {
        return;
    }
    let field = match &*expr.node {
        ExprNode::Send { args, .. } => match &*args[0].node {
            ExprNode::Lit { value: Literal::Sym { value } } => Some(value.clone()),
            _ => None,
        },
        _ => unreachable!(),
    };
    let Some(field) = field else {
        diags.push(residue(expr, "dynamic field"));
        return;
    };
    if field.as_str() == "base" {
        diags.push(residue(expr, ":base carries no humanized prefix"));
        return;
    }
    let humanized = super::model_to_library::validations::humanize(field.as_str());
    let span = expr.span;
    let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
    let ExprNode::Send { recv, .. } = node else { unreachable!() };
    let accumulator = recv.expect("checked Some above");
    let prefix = Expr::new(
        span,
        ExprNode::Lit { value: Literal::Str { value: format!("{humanized} ") } },
    );
    // Stamp the result: this pass runs *after* analyze, and the
    // post-lowering `diagnose` walk reads stamped types without
    // re-dispatching, so an unstamped synthesis false-positives a
    // `send_dispatch_failed`.
    expr.ty = Some(Ty::Array { elem: Box::new(Ty::Str) });
    *expr.node = ExprNode::Send {
        recv: Some(Expr::new(
            span,
            ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] },
        )),
        method: Symbol::from("errors_for"),
        args: vec![accumulator, prefix],
        block: None,
        parenthesized: true,
    };
}
