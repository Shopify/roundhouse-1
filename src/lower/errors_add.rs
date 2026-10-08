//! `errors.add` grounding: `errors.add(:field, "msg")` →
//! `errors << "Field msg"`.
//!
//! The shared runtime's error accumulator is a plain `Array[String]` —
//! the validates lowering bakes humanized full messages at lower time
//! ("Short can't be blank") — so hand-written `add` calls ground into
//! the same shape. `:base` contributes the bare message (Rails
//! semantics); a dynamic message interpolates after the humanized
//! field; a missing message defaults to Rails' "is invalid".
//!
//! Any receiver spelling grounds: `errors` as a zero-arg send on
//! `self` (a model adding to its own errors during validation) or on
//! another expression (`record.errors.add(...)` from a controller —
//! lobsters' duplicate-comment guard). The rewrite keeps the receiver,
//! so both land on the same accumulator. A local or parameter field
//! (an EachValidator's `record.errors.add(attribute, "msg")`) grounds
//! to `attribute.to_s == "base" ? "msg" : "#{ActiveSupport.humanize(
//! attribute.to_s)} msg"`, the same text evaluated at run time. A
//! Symbol message is an error type: the count-free entries of Rails'
//! default en.yml (`:blank` → "can't be blank", `:taken`, ...) bake to
//! their text. Any other field expression, error type or option set
//! still joins the residue ledger — the accumulator is an
//! `Array[String]`, which has no `add`, so on strict targets each such
//! site is a named per-target gap rather than a silent compile error
//! (`lower_residue` warning, pass `errors_add`). Adjacent
//! string-literal concats in the message (`"a " << "b"`, lobsters'
//! line-wrap idiom) fold to one literal first — a runtime `<<` on a
//! frozen literal is a hazard the bake sidesteps.
//!
//! Purely shape-directed; runs on the post-analyze hook
//! (`apply_post_analyze_lowerings`) with its siblings so every target
//! consumes the grounded form. Scope is `for_each_hook_body` (views
//! excluded like every hook pass — the construct has no view presence).

use crate::app::App;
use crate::diagnostic::Diagnostic;
use super::active_model_model::Reading;
use crate::expr::{Expr, ExprNode, InterpPart, Literal};
use crate::ident::Symbol;
use crate::ty::Ty;

/// Rewrite self-receiver `errors.add` sends across every hook body.
/// Returns the residue ledger: recognizable `errors.add` sites left
/// dynamic, with the reason.
pub fn apply_errors_add_lowering(app: &mut App) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let localized = app.locale_files.first().map(|f| (f.clone(), app.locale_files.len()));
    let port = super::active_model_model::ValidationsPort::of(app);
    super::for_each_owned_hook_body(app, &mut |owner, body| {
        let owner = owner.cloned();
        let locals = port.locals(body);
        let site = Site { localized: localized.as_ref(), port: &port, owner: owner.as_ref(), locals: &locals };
        rewrite_errors_add(body, &site, &mut diags)
    });
    diags.extend(port.take_notes());
    diags
}

fn residue(expr: &Expr, reason: &str) -> Diagnostic {
    crate::lower::residue_diagnostic(
        "errors_add",
        "errors.add",
        expr.span,
        reason,
        format!(
            "`errors.add` left as dynamic dispatch ({reason}) — the error \
             accumulator is an Array[String] with no `add`; ground by hand \
             or extend the errors_add lowering"
        ),
    )
}

/// Fold `"a" << "b"` / `"a" + "b"` chains of string literals into one
/// literal (left-recursively, so multi-line wraps fold whole). Any
/// non-literal operand leaves the expression untouched.
fn fold_str_concat(e: Expr) -> Expr {
    let ExprNode::Send { recv: Some(l), method, args, block: None, .. } = &*e.node else {
        return e;
    };
    if !(method.as_str() == "<<" || method.as_str() == "+") || args.len() != 1 {
        return e;
    }
    let left = fold_str_concat(l.clone());
    let (ExprNode::Lit { value: Literal::Str { value: lv } },
         ExprNode::Lit { value: Literal::Str { value: rv } }) = (&*left.node, &*args[0].node)
    else {
        return e;
    };
    Expr::new(e.span, ExprNode::Lit { value: Literal::Str { value: format!("{lv}{rv}") } })
}

/// Where a body sits: the app's locale files, and which classes keep
/// ActiveModel::Errors (the port) with the body's owner.
struct Site<'a> {
    localized: Option<&'a (String, usize)>,
    port: &'a super::active_model_model::ValidationsPort,
    owner: Option<&'a crate::ident::ClassId>,
    locals: &'a super::active_model_model::PortLocals,
}

fn rewrite_errors_add(expr: &mut Expr, site: &Site<'_>, diags: &mut Vec<Diagnostic>) {
    let localized = site.localized;
    expr.node
        .for_each_child_mut(&mut |c| rewrite_errors_add(c, site, diags));
    // Any `add` on an `errors` reader is this pass's construct; decide
    // ground-vs-residue below.
    let recognized = matches!(
        &*expr.node,
        ExprNode::Send { recv: Some(r), method, block: None, .. }
            if method.as_str() == "add"
                && matches!(&*r.node, ExprNode::Send { method: em, args: ea, .. }
                    if em.as_str() == "errors" && ea.is_empty())
    );
    if !recognized {
        return;
    }
    // The rewrite targets the shared runtime's `Array[String]`
    // accumulator. An `errors` reader the analyzer typed as a class
    // (an app's own error collection, e.g. CustomerFoundations::Errors
    // with `add(attribute, code)`) is not that accumulator: its `add`
    // is an ordinary method call and stays as written. The framework's
    // `ActiveModel::Errors` is the accumulator and grounds.
    let ExprNode::Send { recv: Some(r), .. } = &*expr.node else { unreachable!() };
    if r.ty.as_ref().is_some_and(names_own_class) {
        return;
    }
    // A class over the ActiveModel::Validations port (and a module only
    // those include) holds the real ActiveModel::Errors: `add` is its
    // own, with Rails' arguments and messages.
    match site.port.classify(r, site.owner, site.locals) {
        Reading::Port => return,
        Reading::Refused => {
            site.port.refuse(expr, "errors.add");
            return;
        }
        Reading::Untyped => site.port.note_untyped("errors_add", "errors.add", expr.span),
        Reading::Accumulator => {}
    }
    // The field: a Symbol literal is baked at lower time; a local or
    // parameter (`errors.add(attribute, …)` in an EachValidator's
    // `validate_each`) is humanized at run time. Anything else stays
    // residue: the field would be evaluated twice.
    let field_kind = match &*expr.node {
        ExprNode::Send { args, .. } if !args.is_empty() && args.len() <= 2 => match &*args[0].node {
            ExprNode::Lit { value: Literal::Sym { .. } } => Some(true),
            ExprNode::Var { .. } => Some(false),
            _ => None,
        },
        _ => None,
    };
    let Some(static_field) = field_kind else {
        let reason = match &*expr.node {
            ExprNode::Send { args, .. }
                if args.first().is_some_and(
                    |a| !matches!(&*a.node, ExprNode::Lit { value: Literal::Sym { .. } })) =>
            {
                "dynamic field"
            }
            _ => "unrecognized arg shape",
        };
        diags.push(residue(expr, reason));
        return;
    };
    let span = expr.span;
    let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
    let ExprNode::Send { recv, args, .. } = node else { unreachable!() };
    let mut args = args.into_iter();
    let field_expr = args.next().expect("checked non-empty");
    let mut msg = args.next().map(fold_str_concat);
    // Rails' options spelling: `errors.add(:field, message: "…")`.
    // The kwargs hash IS the message carrier — unwrap a sole
    // `message:` entry; any other option set stays residue (put the
    // Send back — it was already moved out).
    let sole_message = matches!(
        msg.as_ref().map(|m| &*m.node),
        Some(ExprNode::Hash { entries, .. })
            if entries.len() == 1
                && matches!(&*entries[0].0.node,
                    ExprNode::Lit { value: Literal::Sym { value } }
                        if value.as_str() == "message")
    );
    if sole_message {
        let Some(ExprNode::Hash { entries, .. }) = msg.take().map(|m| *m.node) else {
            unreachable!()
        };
        msg = Some(fold_str_concat(entries.into_iter().next().unwrap().1));
    }
    // A Symbol message is an error TYPE, which Rails translates
    // (`errors.add(attr, :blank)` reads "can't be blank"). The
    // count-free entries of activemodel's and activerecord's default
    // en.yml bake to their text; any other type stays residue.
    let symbol_type = match msg.as_ref().map(|m| &*m.node) {
        Some(ExprNode::Lit { value: Literal::Sym { value } }) => Some(value.clone()),
        _ => None,
    };
    // Rails looks the type up through the app's locale data first
    // (`activerecord.errors.models.<model>.attributes.<attr>.<type>`,
    // the model's, its ancestors', `activerecord.errors.messages`,
    // `errors.attributes.<attr>`, `errors.messages`), and the record's
    // class is not known here: with locale files present the default
    // text is not the answer, so the site declines by name.
    let translated = if localized.is_some() {
        None
    } else {
        symbol_type.as_ref().and_then(|t| default_error_message(t.as_str()))
    };
    let unrecognized = matches!(msg.as_ref().map(|m| &*m.node), Some(ExprNode::Hash { .. }))
        || (symbol_type.is_some() && translated.is_none());
    if unrecognized {
        let reason = match (&symbol_type, localized) {
            (Some(t), Some((first, n))) => format!(
                "error type :{} resolves through the app's locale data ({first}{})",
                t.as_str(),
                if *n > 1 { format!(" and {} more", n - 1) } else { String::new() }
            ),
            (Some(t), None) => format!("error type :{} has no count-free default message", t.as_str()),
            (None, _) => "unrecognized arg shape".to_string(),
        };
        diags.push(residue(&Expr::new(span, ExprNode::Seq { exprs: vec![] }), &reason));
        *expr.node = ExprNode::Send {
            recv,
            method: Symbol::from("add"),
            args: vec![field_expr, msg.unwrap()],
            block: None,
            parenthesized: true,
        };
        return;
    }
    // Rails' default error type, with no options: `errors.add(:lines)`
    // and `errors.add(:lines, :invalid)` are the same Error as the one a
    // has_many adds for an invalid record (`emit::ruby::
    // collection_autosave`), which `errors.uniq!` keeps once.
    let invalid_type = static_field
        && !sole_message
        && (msg.is_none() || symbol_type.as_ref().is_some_and(|t| t.as_str() == "invalid"));
    if let Some(text) = translated {
        msg = Some(str_lit(span, text));
    }
    let mut message: Expr = if static_field {
        let ExprNode::Lit { value: Literal::Sym { value: field } } = &*field_expr.node else {
            unreachable!()
        };
        let humanized = super::model_to_library::validations::humanize(field.as_str());
        if field.as_str() == "base" {
            // :base attaches the message to the record, not a field.
            msg.unwrap_or_else(|| str_lit(span, "is invalid"))
        } else {
            match msg {
                Some(m) => match &*m.node {
                    ExprNode::Lit { value: Literal::Str { value } } => {
                        str_lit(span, &format!("{humanized} {value}"))
                    }
                    _ => Expr::new(
                        span,
                        ExprNode::StringInterp {
                            parts: vec![
                                InterpPart::Text { value: format!("{humanized} ") },
                                InterpPart::Expr { expr: m },
                            ],
                        },
                    ),
                },
                None => str_lit(span, &format!("{humanized} is invalid")),
            }
        }
    } else {
        // `attribute.to_s == "base" ? msg : "#{ActiveSupport.humanize(
        // attribute.to_s)} #{msg}"`: Rails' full message is the bare
        // message on :base and "<human attribute name> <message>"
        // otherwise. The field is a local, so reading it twice is free.
        let msg = msg.unwrap_or_else(|| str_lit(span, "is invalid"));
        let field_s = typed(
            span,
            ExprNode::Send { recv: Some(field_expr.clone()), method: Symbol::from("to_s"), args: vec![], block: None, parenthesized: false },
            Ty::Str,
        );
        let is_base = typed(
            span,
            ExprNode::Send { recv: Some(field_s.clone()), method: Symbol::from("=="), args: vec![str_lit(span, "base")], block: None, parenthesized: false },
            Ty::Bool,
        );
        let human = typed(
            span,
            ExprNode::Send {
                recv: Some(typed(span, ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] }, Ty::Class { id: crate::ident::ClassId(Symbol::from("ActiveSupport")), args: vec![] })),
                method: Symbol::from("humanize"),
                args: vec![field_s],
                block: None,
                parenthesized: true,
            },
            Ty::Str,
        );
        let tail = match &*msg.node {
            ExprNode::Lit { value: Literal::Str { value } } => InterpPart::Text { value: format!(" {value}") },
            _ => InterpPart::Expr { expr: msg.clone() },
        };
        let mut parts = vec![InterpPart::Expr { expr: human }];
        if matches!(tail, InterpPart::Expr { .. }) {
            parts.push(InterpPart::Text { value: " ".into() });
        }
        parts.push(tail);
        let full = typed(span, ExprNode::StringInterp { parts }, Ty::Str);
        typed(span, ExprNode::If { cond: is_base, then_branch: msg, else_branch: full }, Ty::Str)
    };
    // `<<` returns the accumulator it appends to (Array#<< / the errors
    // collection), so the synthesized send's result type is the
    // receiver's. Stamp it: this pass runs *after* analyze, and the
    // post-lowering `diagnose` walk reads stamped types without re-
    // dispatching — leaving `ty = None` here makes it false-positive a
    // `send_dispatch_failed` on a receiver it can see the type of.
    let recv_ty = recv.as_ref().and_then(|r| r.ty.clone());
    // The attribute the message is for, which the accumulator drops: the
    // CRuby and JRuby trees record it for `errors.attribute_names`.
    let attribute = match &*field_expr.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(crate::expr::IrHint::ErrorAttribute(value.clone())),
        ExprNode::Var { name, .. } => Some(crate::expr::IrHint::ErrorAttributeLocal(name.clone())),
        _ => None,
    };
    expr.hint = attribute;
    if invalid_type {
        message.hint = Some(crate::expr::IrHint::InvalidErrorType);
    }
    *expr.node = ExprNode::Send {
        recv,
        method: Symbol::from("<<"),
        args: vec![message],
        block: None,
        parenthesized: false,
    };
    expr.ty = recv_ty;
}

/// A class type (or a nilable one), i.e. not the runtime accumulator.
/// `ActiveModel::Errors` is the framework's collection, which the
/// analyzer types `errors` as on AR records and on includers of
/// `ActiveModel::Validations`/`Model`: the shared runtime implements it
/// as that very accumulator, so it grounds like an untyped receiver.
fn names_own_class(ty: &Ty) -> bool {
    let app_class = |v: &Ty| matches!(v, Ty::Class { id, .. } if id.0.as_str() != "ActiveModel::Errors");
    match ty {
        Ty::Class { .. } => app_class(ty),
        Ty::Union { variants } => {
            variants.iter().any(app_class)
                && variants.iter().all(|v| app_class(v) || matches!(v, Ty::Nil))
        }
        _ => false,
    }
}

fn str_lit(span: crate::span::Span, value: &str) -> Expr {
    typed(span, ExprNode::Lit { value: Literal::Str { value: value.to_string() } }, Ty::Str)
}

fn typed(span: crate::span::Span, node: ExprNode, ty: Ty) -> Expr {
    let mut e = Expr::new(span, node);
    e.ty = Some(ty);
    e
}

/// The count-free messages of activemodel 8.2.0.alpha's
/// lib/active_model/locale/en.yml and activerecord's
/// lib/active_record/locale/en.yml (`required`, `taken`), by error type.
/// `confirmation` interpolates the attribute and the length/number
/// types a count, so they are not here.
fn default_error_message(error_type: &str) -> Option<&'static str> {
    Some(match error_type {
        "inclusion" => "is not included in the list",
        "exclusion" => "is reserved",
        "invalid" => "is invalid",
        "accepted" => "must be accepted",
        "empty" => "can't be empty",
        "blank" => "can't be blank",
        "present" => "must be blank",
        "password_too_long" => "is too long",
        "not_a_number" => "is not a number",
        "not_an_integer" => "must be an integer",
        "odd" => "must be odd",
        "even" => "must be even",
        "required" => "must exist",
        "taken" => "has already been taken",
        _ => return None,
    })
}
