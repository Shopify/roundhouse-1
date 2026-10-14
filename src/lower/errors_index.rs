//! `errors[:field]` grounding: `<recv>.errors[:field]` →
//! `<recv>.error_messages_for("field")`.
//!
//! The shared runtime's `errors` is a plain `Array[String]` of FULL
//! messages, and `error_details` holds the `ActiveRecord::ErrorDetail`
//! each was added with — [`super::errors_add`] and the `validates`
//! lowering push both. Rails' `errors[:field]` is the messages on one
//! attribute WITHOUT its humanized name (`[ "is not public" ]`, not
//! `[ "Url is not public" ]`), which `Base#error_messages_for` answers
//! from the details, under any `errors.format` and for `:base` alike.
//!
//! Left alone the site is not merely unsupported, it is silently wrong
//! on CRuby too: `Array#[]` with a Symbol raises `TypeError: no implicit
//! conversion of Symbol into Integer`, which is what campfire's
//! `Opengraph::LocationTest` reported for three tests.
//!
//! Purely shape-directed, like its two siblings: the receiver of `[]`
//! must itself be a zero-arg `errors` send, which is what makes the
//! site our accumulator rather than some other object's `[]`.
//!
//! `errors.to_hash` is the same projection for every attribute at once:
//! `ActiveSupport.errors_to_hash(<recv>.error_details)` groups the
//! details' messages by attribute, in the order Rails does. A
//! `messages` the per-field rewrites leave is that Hash too, through
//! `errors_messages`, and `attribute_names` is
//! `errors_attribute_names`. `include?` / `key?` / `has_key?` ask
//! `error_messages_for` like `errors[:field]` does.
//!
//! Views take the same rewrite: the scaffold's form partial reads
//! `<record>.errors[:field].none?` / `.any?` inside a `class:` array,
//! which renders through `class_names` at request time.

use crate::app::App;
use crate::diagnostic::Diagnostic;
use super::active_model_model::Reading;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;
use crate::ty::Ty;

/// Rewrite `errors[:field]` reads across every hook, test and view body.
/// Returns the residue ledger:
/// recognizable `errors[…]` sites left dynamic, with the reason.
pub fn apply_errors_index_lowering(app: &mut App) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    // Every tree keeps each message's attribute in an `error_details` list
    // on the record, which the reads below ask; the CRuby and JRuby trees
    // also record it in the runtime's `ActiveModel::ErrorAttributes`, which
    // answers the reads this pass leaves (`to_hash(true)`, `as_json(options)`).
    let port = super::active_model_model::ValidationsPort::of(app);
    super::for_each_owned_hook_body(app, &mut |owner, body| {
        let locals = port.locals(body);
        let site = (&port, owner.cloned(), &locals);
        rewrite(body, &site, &mut diags);
        rewrite_whole_messages(body, &site);
    });
    super::for_each_test_body(app, &mut |body| {
        let locals = port.locals(body);
        let site = (&port, None, &locals);
        rewrite(body, &site, &mut diags);
        rewrite_whole_messages(body, &site);
    });
    diags.extend(port.take_notes());
    for view in &mut app.views {
        let locals = port.locals(&view.body);
        let site = (&port, None, &locals);
        rewrite(&mut view.body, &site, &mut diags);
        rewrite_whole_messages(&mut view.body, &site);
    }
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

// Not shape alone, as `[]` is: an app's own `errors` Array answers `include?` and `attribute_names` itself.
fn is_model_errors_read(e: &Expr) -> bool {
    is_errors_read(e) && matches!(&e.ty, Some(Ty::Class { id, .. }) if id.0.as_str() == "ActiveModel::Errors")
}

fn is_messages_read(e: &Expr) -> bool {
    matches!(&*e.node,
        ExprNode::Send { recv: Some(r), method, args, block: None, .. }
            if method.as_str() == "messages" && args.is_empty() && is_errors_read(r))
}

fn sym_lit(e: &Expr) -> Option<&Symbol> {
    match &*e.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value),
        _ => None,
    }
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
    // A field that is not a Symbol literal reads the whole Hash instead, which `rewrite_whole_messages` builds.
    let replacement = match (method.as_str(), args.as_slice()) {
        ("[]", [field]) if sym_lit(field).is_some() => {
            *r = accumulator;
            None
        }
        ("key?" | "has_key?" | "include?" | "member?", [field]) if sym_lit(field).is_some() => {
            let field = field.clone();
            let mut index = send(span, accumulator, "[]", vec![field], Ty::Array { elem: std::sync::Arc::new(Ty::Str) });
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
type Site<'a> = (
    &'a super::active_model_model::ValidationsPort,
    Option<crate::ident::ClassId>,
    &'a super::active_model_model::PortLocals,
);

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
    rewrite_include(expr, diags);
    rewrite_to_hash(expr);
    rewrite_render_json(expr);
}

// Not left to the accumulator: `Array#include?(:title)` answers false for an Array of full messages.
fn rewrite_include(expr: &mut Expr, diags: &mut Vec<Diagnostic>) {
    let ExprNode::Send { recv: Some(reader), method, args, block: None, .. } = &*expr.node else { return };
    if !matches!(method.as_str(), "include?" | "key?" | "has_key?") || args.len() != 1 || !is_model_errors_read(reader) {
        return;
    }
    let field = match &*args[0].node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
        ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
        _ => None,
    };
    let Some(field) = field else {
        let mut d = crate::lower::residue_diagnostic(
            "errors_index",
            "errors.include?",
            expr.span,
            "dynamic field",
            format!(
                "`errors.{method}` with a field that is not a literal is not grounded; the error \
                 accumulator's own `{method}` would ask its full messages"
            ),
        );
        d.severity = crate::diagnostic::Severity::Error;
        diags.push(d);
        return;
    };
    let ExprNode::Send { recv: owner, .. } = &*reader.node else { unreachable!("an errors reader") };
    *expr = has_errors_on(owner.clone(), &field, expr.span);
}

/// What the per-field rewrites leave of `errors.messages` is the Hash itself.
fn rewrite_whole_messages(expr: &mut Expr, site: &Site<'_>) {
    expr.node.for_each_child_mut(&mut |c| rewrite_whole_messages(c, site));
    if !is_messages_read(expr) || on_port(expr, site) {
        return;
    }
    let ExprNode::Send { recv: Some(reader), .. } = &*expr.node else { unreachable!("a messages read") };
    *expr = details_call(expr.span, reader, "errors_messages", messages_ty());
}

/// `<owner>.errors.to_hash` and `.as_json` → `ActiveSupport.errors_to_hash(<owner>.error_details)`,
/// `.attribute_names` → `ActiveSupport.errors_attribute_names(…)`, typed.
fn rewrite_to_hash(expr: &mut Expr) {
    let ExprNode::Send { recv: Some(reader), method, args, block: None, .. } = &*expr.node else { return };
    if !args.is_empty() || !is_errors_read(reader) {
        return;
    }
    let (helper, ty) = match method.as_str() {
        "to_hash" | "as_json" => ("errors_to_hash", messages_ty()),
        "attribute_names" if is_model_errors_read(reader) => ("errors_attribute_names", Ty::Array { elem: std::sync::Arc::new(Ty::Sym) }),
        _ => return,
    };
    *expr = details_call(expr.span, reader, helper, ty);
}

// Not the Array of full messages, nor `errors_to_hash`: Rails' `render json:` writes `errors.as_json` keyed by attribute, and `Base#errors_json` reaches every target with String keys, which JSON writes the same.
fn rewrite_render_json(expr: &mut Expr) {
    let ExprNode::Send { recv: None, method, args, .. } = &mut *expr.node else { return };
    if method.as_str() != "render" {
        return;
    }
    for arg in args.iter_mut() {
        let ExprNode::Hash { entries, .. } = &mut *arg.node else { continue };
        for (key, value) in entries.iter_mut() {
            if matches!(&*key.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "json") {
                json_value(value);
            }
        }
    }
}

fn json_value(value: &mut Expr) {
    let reader = match &*value.node {
        _ if is_errors_read(value) => Some(&*value),
        ExprNode::Send { recv: Some(r), .. } if is_messages_read(value) => Some(r),
        _ => None,
    };
    if let Some(reader) = reader {
        let ExprNode::Send { recv: owner, .. } = &*reader.node else { unreachable!("an errors reader") };
        let mut json = Expr::new(
            value.span,
            ExprNode::Send { recv: owner.clone(), method: Symbol::from("errors_json"), args: Vec::new(), block: None, parenthesized: false },
        );
        json.ty = Some(Ty::Hash {
            key: std::sync::Arc::new(Ty::Str),
            value: std::sync::Arc::new(Ty::Array { elem: std::sync::Arc::new(Ty::Str) }),
        });
        *value = json;
        return;
    }
    match &mut *value.node {
        ExprNode::Hash { entries, .. } => entries.iter_mut().for_each(|(_, v)| json_value(v)),
        ExprNode::Array { elements, .. } => elements.iter_mut().for_each(json_value),
        _ => {}
    }
}

fn messages_ty() -> Ty {
    Ty::Hash {
        key: std::sync::Arc::new(Ty::Sym),
        value: std::sync::Arc::new(Ty::Array { elem: std::sync::Arc::new(Ty::Str) }),
    }
}

/// `ActiveSupport.<helper>(<owner>.error_details)`, typed.
fn details_call(span: crate::span::Span, reader: &Expr, helper: &str, ty: Ty) -> Expr {
    let ExprNode::Send { recv: owner, .. } = &*reader.node else { unreachable!("an errors reader") };
    let mut details = Expr::new(
        span,
        ExprNode::Send { recv: owner.clone(), method: Symbol::from("error_details"), args: Vec::new(), block: None, parenthesized: false },
    );
    details.ty = Some(super::errors_add::error_details_ty());
    let mut support = Expr::new(span, ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] });
    support.ty = Some(Ty::Class { id: crate::ident::ClassId(Symbol::from("ActiveSupport")), args: vec![].into() });
    let mut call = Expr::new(
        span,
        ExprNode::Send { recv: Some(support), method: Symbol::from(helper), args: vec![details], block: None, parenthesized: true },
    );
    call.ty = Some(ty);
    call
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
    let span = expr.span;
    let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
    let ExprNode::Send { recv, .. } = node else { unreachable!() };
    let ExprNode::Send { recv: owner, .. } = *recv.expect("checked Some above").node else { unreachable!() };
    *expr = messages_on(owner, field.as_str(), span);
}

/// `<owner>.error_messages_for("<field>")`, typed.
pub(crate) fn messages_on(owner: Option<Expr>, field: &str, span: crate::span::Span) -> Expr {
    let mut lit = Expr::new(span, ExprNode::Lit { value: Literal::Str { value: field.to_string() } });
    lit.ty = Some(Ty::Str);
    let mut e = Expr::new(
        span,
        ExprNode::Send { recv: owner, method: Symbol::from("error_messages_for"), args: vec![lit], block: None, parenthesized: true },
    );
    // Stamp the result: callers run *after* analyze, and the post-lowering
    // `diagnose` walk reads stamped types without re-dispatching.
    e.ty = Some(Ty::Array { elem: std::sync::Arc::new(Ty::Str) });
    e
}

/// `<owner>.error_messages_for("<field>").any?`, typed.
pub(crate) fn has_errors_on(owner: Option<Expr>, field: &str, span: crate::span::Span) -> Expr {
    send(span, messages_on(owner, field, span), "any?", vec![], Ty::Bool)
}
