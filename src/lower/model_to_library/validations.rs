//! Validations: lower `validates :attr, presence: true, length: { ... }` into
//! a single `def validate` body. Each rule expands to inline IR (`if cond
//! then errors << "msg" end`) rather than a helper-call into the runtime
//! Validations module — the Phase 2.5(a) lowerer per docs/archive/rust-migration-plan.md.
//!
//! Inline expansion wins three ways: (1) error messages are string-literal
//! constants, no runtime interpolation, (2) typed targets (Rust, Crystal,
//! strict TS) avoid the `untyped value` channel that the Validations module
//! forces, (3) every target gets the same expansion — no per-target adapter
//! for the Validations module's dispatch shape.
//!
//! One top-level `def validate` per model; rules across multiple attrs each
//! append their own stmt block to the body.

use crate::dialect::{
    AccessorKind, Association, MethodDef, MethodReceiver, Model, NumericCheck, NumericOp, RuleMessage,
    RuleOptions, ValidationRule,
};
use crate::effect::EffectSet;
use crate::expr::{ArrayStyle, BoolOpKind, BoolOpSurface, Expr, ExprNode, InterpPart, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;
use crate::ty::Ty;

use super::{fn_sig, seq};

pub(super) fn push_validate_method(methods: &mut Vec<MethodDef>, model: &Model) {
    let mut stmts: Vec<Expr> = Vec::new();
    // `model_name.human`, which a `message:` reads as `%{model}`.
    let model_human = humanize(&crate::naming::underscore(crate::naming::demodulize(model.name.0.as_str())));

    for (span, v) in model.spanned_validations() {
        // Column type from the model's attributes row (when present).
        // Lets the per-rule generator skip dead `is_a?(Array)` branches
        // for fields the schema declares as `Str` — tsc narrows
        // `Array.isArray(stringField)` to `never` and rejects the
        // subsequent `.length` access.
        let attr_ty = model.attributes.fields.get(&v.attribute);
        // `validates :user, presence: true` on a `belongs_to :user` (or
        // `has_one`) names the ASSOCIATION, and Rails asks its reader —
        // `user.blank?`, nil for a record. There is no `@user` slot: the
        // writer sets `@user_id` and caches the record, so the column
        // check below read an ivar nothing writes and rejected every
        // record. The 2023 lobsters snapshot's ReadRibbon validates this
        // way, and its story page (reached once `around_action` ran)
        // could never save a ribbon.
        let is_assoc = model.associations().any(|a| {
            matches!(a, Association::BelongsTo { name, .. } | Association::HasOne { name, .. }
                if name == &v.attribute)
        });
        for rule in &v.rules {
            // `allow_nil:` / `allow_blank:` / `message:` / `if:` /
            // `unless:` wrap the rule they apply to.
            let (rule, options) = match rule {
                ValidationRule::Guarded { rule, options } => (&**rule, Some(options)),
                other => (other, None),
            };
            // The Sequel front-end's `message:` rides on the Length rule.
            let own_message = match rule {
                ValidationRule::Length { message: Some(text), .. } => Some(RuleMessage::Text { text: text.clone() }),
                _ => None,
            };
            let text = ErrorText {
                attr: &v.attribute,
                model_human: &model_human,
                message: options.and_then(|o| o.message.as_ref()).or(own_message.as_ref()),
            };
            let reader = Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: None,
                    method: v.attribute.clone(),
                    args: vec![],
                    block: None,
                    parenthesized: false,
                },
            );
            let (checks, value) = if is_assoc && matches!(rule, ValidationRule::Presence) {
                let check = if_with_nil_else(
                    send(reader.clone(), "nil?", vec![]),
                    text.push("can't be blank", None, || reader.clone()),
                );
                (vec![check], reader)
            } else {
                (validation_rule_to_calls(&v.attribute, rule, attr_ty, &text), ivar(&v.attribute))
            };
            let checks = match options {
                Some(o) => guard_rule(value, checks, o),
                None => checks,
            };
            for mut check in checks {
                // Each expanded check attributes to its `validates` line,
                // and each message it adds to the validated attribute.
                check.inherit_span(span);
                tag_error_attribute(&mut check, &v.attribute);
                stmts.push(check);
            }
        }
    }

    // Rails 5+ default: every `belongs_to` requires the associated
    // record to exist before save. Emit `validates_belongs_to(:assoc,
    // @<fk>, <Target>)` per non-optional belongs_to. The runtime
    // helper short-circuits when the FK is unset (nil/0) and queries
    // `<Target>.exists?(fk_value)` otherwise.
    for (span, assoc) in model.spanned_associations() {
        if let Association::BelongsTo {
            name, target, foreign_key, optional: false, polymorphic, ..
        } = assoc
        {
            let mut check =
                inline_belongs_to_check(name, foreign_key, target, *polymorphic);
            check.inherit_span(span);
            // Rails adds `:required` to the association's name.
            tag_error_attribute(&mut check, name);
            stmts.push(check);
        }
    }

    stmts.extend(secure_password_checks(model));

    if stmts.is_empty() {
        return;
    }

    methods.push(MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("validate"),
        receiver: MethodReceiver::Instance,
        params: Vec::new(),
        body: seq(stmts),
        signature: Some(fn_sig(vec![], Ty::Nil)),
        effects: EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    });

    // A validating model OUTSIDE the ActiveRecord hierarchy (`class
    // Search` + `include ActiveModel::Validations`, no superclass)
    // can't inherit `valid?`/`errors` from the runtime Base —
    // synthesize the same pair here. Skipped when the source defines
    // either name itself.
    if model.parent.is_none() {
        push_active_model_validation_surface(methods, model);
    }
}

/// The validations `has_secure_password` adds unless it is written
/// with `validations: false` — the ones that make a password reset with
/// a mismatched confirmation FAIL, as the Rails 8 authentication
/// generator's PasswordsController and its test rely on:
///
///   errors << "Password can't be blank" if @password_digest blank
///   errors << "Password is too long" if @password.bytesize > 72
///   errors << "Password confirmation doesn't match Password"
///     if @password_confirmation given and != @password (allow_blank)
///
/// The plaintext ivars are the ones `lower::secure_password` writes.
fn secure_password_checks(model: &Model) -> Vec<Expr> {
    if !secure_password_validates(model) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for attr in crate::lower::secure_password::secure_password_attrs(&model.body) {
        let human = humanize(attr.as_str());
        let plain = ivar(&attr);
        let digest = ivar(&Symbol::from(format!("{}_digest", attr.as_str())));
        let confirmation = ivar(&Symbol::from(format!("{}_confirmation", attr.as_str())));
        let blank = |e: Expr| bool_op(BoolOpKind::Or, send(e.clone(), "nil?", vec![]), send(e, "empty?", vec![]));
        let not = |e: Expr| Expr::new(
            Span::synthetic(),
            ExprNode::Send { recv: Some(e), method: Symbol::from("!"), args: vec![], block: None, parenthesized: false },
        );
        let present = |e: Expr| not(blank(e));
        // Rails adds the blank and too-long errors to the attribute and
        // the mismatch to `<attr>_confirmation` (validates_confirmation_of).
        let confirmation_attr = Symbol::from(format!("{}_confirmation", attr.as_str()));
        let start = out.len();
        out.push(if_with_nil_else(blank(digest), errors_push(format!("{human} can't be blank"))));
        out.push(if_with_nil_else(
            bool_op(
                BoolOpKind::And,
                present(plain.clone()),
                send(send(plain.clone(), "bytesize", vec![]), ">", vec![Expr::new(
                    Span::synthetic(),
                    ExprNode::Lit { value: Literal::Int { value: 72 } },
                )]),
            ),
            errors_push(format!("{human} is too long")),
        ));
        out.push(if_with_nil_else(
            bool_op(
                BoolOpKind::And,
                bool_op(BoolOpKind::And, present(plain.clone()), not(send(confirmation.clone(), "nil?", vec![]))),
                send(confirmation, "!=", vec![plain]),
            ),
            errors_push(format!("{human} confirmation doesn't match {human}")),
        ));
        for check in &mut out[start..start + 2] {
            tag_error_attribute(check, &attr);
        }
        tag_error_attribute(&mut out[start + 2], &confirmation_attr);
    }
    out
}

/// Tag each `errors << msg` push in `check` that carries no attribute yet
/// with the one it is for (`IrHint::ErrorAttribute`): the accumulator
/// keeps only the message, and `errors.attribute_names` needs the
/// attribute on the trees that record it.
pub(crate) fn tag_error_attribute(check: &mut Expr, attr: &Symbol) {
    let is_push = matches!(
        &*check.node,
        ExprNode::Send { recv: Some(r), method, args, .. }
            if method.as_str() == "<<" && args.len() == 1
                && matches!(&*r.node, ExprNode::Send { recv: None, method: m, args: a, .. } if m.as_str() == "errors" && a.is_empty())
    );
    if is_push && check.hint.is_none() {
        check.hint = Some(crate::expr::IrHint::ErrorAttribute(attr.clone()));
    }
    check.node.for_each_child_mut(&mut |c| tag_error_attribute(c, attr));
}

/// `has_secure_password` without `validations: false`.
fn secure_password_validates(model: &Model) -> bool {
    use crate::dialect::ModelBodyItem;
    let mut declared = false;
    for item in &model.body {
        let ModelBodyItem::Unknown { expr, .. } = item else { continue };
        let ExprNode::Send { recv: None, method, args, .. } = &*expr.node else { continue };
        if method.as_str() != "has_secure_password" {
            continue;
        }
        declared = true;
        let off = args.iter().any(|a| matches!(&*a.node, ExprNode::Hash { entries, .. }
            if entries.iter().any(|(k, v)|
                matches!(&*k.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "validations")
                    && matches!(&*v.node, ExprNode::Lit { value: Literal::Bool { value: false } }))));
        if off {
            return false;
        }
    }
    declared
}

/// `ActiveModel::Model`'s attribute-hash constructor —
/// `Metadata.new(title: …, url: …)`, which the module supplies and
/// `ActiveModel::Validations` alone does NOT. campfire's
/// `Opengraph::Metadata` is built exactly once, by
/// `new attributes.merge(…)` in its own `from_url`, and without this
/// the call was "wrong number of arguments (given 1, expected 0)" —
/// the class had only Object's zero-arg `initialize`.
///
/// Assigns the attributes the model declares through `attr_*`, which is
/// what the module does: unknown keys raise `UnknownAttributeError` in
/// Rails, and dropping them here is the same shape as the permitted-
/// field ledger elsewhere rather than a silent write to nowhere.
///
/// Skipped when the source writes its own `initialize` — campfire's
/// `Opengraph::Location` does, and its one positional arg is the whole
/// point of that class.
pub(super) fn push_active_model_constructor(methods: &mut Vec<MethodDef>, model: &Model) {
    if !includes_active_model_model(model) {
        return;
    }
    let defines_initialize = model.body.iter().any(|item| matches!(
        item,
        crate::dialect::ModelBodyItem::Method { method, .. }
            if method.name.as_str() == "initialize"
    )) || methods.iter().any(|m| m.name.as_str() == "initialize");
    if defines_initialize {
        return;
    }
    let names = super::markers::declared_attr_names(model);
    if names.is_empty() {
        return;
    }
    // ONE definition of what the module's constructor is — the
    // library-class twin (`lower::active_model_model`) covers the
    // classes ingest cannot classify as tableless models (campfire's
    // `ActionText::Attachment::OpengraphEmbed`, which lives in `lib/`),
    // and the two must not drift into two constructors.
    methods.push(crate::lower::active_model_model::attributes_initialize(
        &model.name,
        model.span,
        &names,
    ));
}

/// `include ActiveModel::Model` in the class body. The narrower
/// `ActiveModel::Validations` brings `valid?`/`errors` and no
/// constructor, so the two are not interchangeable here.
fn includes_active_model_model(model: &Model) -> bool {
    model.body.iter().any(|item| {
        let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item else { return false };
        let ExprNode::Send { recv: None, method, args, .. } = &*expr.node else {
            return false;
        };
        method.as_str() == "include"
            && args.iter().any(|a| matches!(&*a.node,
                ExprNode::Const { path } if path.len() == 2
                    && path[0].as_str() == "ActiveModel"
                    && path[1].as_str() == "Model"))
    })
}

/// `before_validation :sanitize_fields` in the model body. Only the
/// method-name form — a block callback (`before_validation do … end`)
/// is synthesized under the same method name, so it is included too;
/// anything `push_callback_methods` declines to synthesize would leave
/// this call dangling, which is why the two read the same declaration.
fn declares_before_validation(model: &Model) -> bool {
    model.body.iter().any(|item| {
        matches!(item, crate::dialect::ModelBodyItem::Callback { callback, .. }
            if callback.hook == crate::dialect::CallbackHook::BeforeValidation)
    })
}

/// `valid?` + `errors` mirroring the runtime Base implementations,
/// for `include ActiveModel::Validations` classes with no superclass.
fn push_active_model_validation_surface(methods: &mut Vec<MethodDef>, model: &Model) {
    let defines = |name: &str| {
        model.body.iter().any(|item| matches!(
            item,
            crate::dialect::ModelBodyItem::Method { method, .. } if method.name.as_str() == name
        ))
    };
    let span = model.span;
    let errors_read = || Expr::new(span, ExprNode::Ivar { name: Symbol::from("errors") });
    if !defines("valid?") {
        // `before_validation :sanitize_fields` runs INSIDE `valid?` for
        // an ActiveModel class (`ActiveModel::Validations::Callbacks`),
        // not on save — there is no save. The callback method is
        // synthesized by `push_callback_methods`, and nothing called
        // it: campfire's `Opengraph::Metadata` sanitizes its title and
        // description there, so the raw `<script>` survived into the
        // record and three tests read it back.
        //
        // Ordered first, which is what the name says and what the
        // sanitize case needs — the presence checks below must see the
        // sanitized values.
        let mut body_stmts: Vec<Expr> = Vec::new();
        // Asked of the model BODY, not of `methods`: this pass runs
        // before `push_callback_methods` synthesizes the callback (847
        // vs 890 in mod.rs), so the method does not exist yet — and
        // reordering the two to suit this read would be the tail
        // wagging the dog.
        if declares_before_validation(model) {
            body_stmts.push(Expr::new(
                span,
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from("before_validation"),
                    args: Vec::new(),
                    block: None,
                    parenthesized: false,
                },
            ));
        }
        body_stmts.extend(vec![
            Expr::new(
                span,
                ExprNode::Assign {
                    target: crate::expr::LValue::Ivar { name: Symbol::from("errors") },
                    value: Expr::new(
                        span,
                        ExprNode::Array { elements: vec![], style: ArrayStyle::default() },
                    ),
                },
            ),
            Expr::new(
                span,
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from("validate"),
                    args: vec![],
                    block: None,
                    parenthesized: false,
                },
            ),
            Expr::new(
                span,
                ExprNode::Send {
                    recv: Some(errors_read()),
                    method: Symbol::from("empty?"),
                    args: vec![],
                    block: None,
                    parenthesized: false,
                },
            ),
        ]);
        let body = seq(body_stmts);
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("valid?"),
            receiver: MethodReceiver::Instance,
            params: Vec::new(),
            body,
            signature: Some(fn_sig(vec![], Ty::Bool)),
            effects: EffectSet::default(),
            enclosing_class: Some(model.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: true,
            block_param: None,
        });
    }
    if !defines("errors") {
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("errors"),
            receiver: MethodReceiver::Instance,
            params: Vec::new(),
            body: errors_read(),
            signature: Some(fn_sig(vec![], Ty::Array { elem: Box::new(Ty::Str) })),
            effects: EffectSet::default(),
            enclosing_class: Some(model.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }
}

/// Produce the list of helper-call expressions for one `ValidationRule` on
/// `attr`. Each helper is `<helper>(:attr, @attr [, kwargs])` — the value
/// is passed positionally so the runtime helper sees a concretely-typed
/// `value` parameter (no block-yield, no `instance_variable_get`).
fn validation_rule_to_calls(
    attr: &Symbol,
    rule: &ValidationRule,
    attr_ty: Option<&Ty>,
    text: &ErrorText<'_>,
) -> Vec<Expr> {
    match rule {
        ValidationRule::Presence => vec![inline_presence_check(attr, attr_ty, text)],
        ValidationRule::Absence => vec![inline_absence_check(attr, text)],
        ValidationRule::Length { min, max, .. } => inline_length_check(
            attr,
            min.map(|n| n as usize),
            max.map(|n| n as usize),
            text,
            attr_ty,
        ),
        ValidationRule::Format { pattern, flags, without } => {
            vec![inline_format_check(attr, pattern, flags, *without, text)]
        }
        ValidationRule::Numericality { only_integer, checks } => {
            inline_numericality_check(attr, *only_integer, checks, text)
        }
        ValidationRule::Inclusion { list, exclusion, cover } => {
            vec![inline_inclusion_check(attr, list, *exclusion, *cover, text)]
        }
        // Reported by `roundhouse check` (`analyze::ingest_coverage`);
        // a weaker check here would pass records Rails rejects.
        ValidationRule::Unsupported { .. } => Vec::new(),
        // `push_validate_method` unwraps the options before asking.
        ValidationRule::Guarded { rule, .. } => validation_rule_to_calls(attr, rule, attr_ty, text),
        // `validate :validate_url` — a method the model defines, which
        // adds to `errors` itself. The synthesized `validate` just
        // calls it; everything else about the shape (when it runs, what
        // `valid?` does with `errors` afterwards) is already the same
        // for a rule-derived check.
        ValidationRule::Custom { method, if_method, unless_method } => {
            let bare = |m: &Symbol| {
                Expr::new(
                    Span::synthetic(),
                    ExprNode::Send {
                        recv: None,
                        method: m.clone(),
                        args: Vec::new(),
                        block: None,
                        parenthesized: false,
                    },
                )
            };
            let call = bare(method);
            let nil = || Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil });
            // `if: :pred` / `unless: :pred` guard the call, as Rails'
            // callback conditions do.
            // Both given: Rails runs the check only when `if:` holds AND
            // `unless:` does not — the `unless` guard nests inside the `if`.
            let guarded = match unless_method {
                Some(c) => Expr::new(
                    Span::synthetic(),
                    ExprNode::If { cond: bare(c), then_branch: nil(), else_branch: call },
                ),
                None => call,
            };
            let guarded = match if_method {
                Some(c) => Expr::new(
                    Span::synthetic(),
                    ExprNode::If { cond: bare(c), then_branch: guarded, else_branch: nil() },
                ),
                None => guarded,
            };
            vec![guarded]
        }
        // `validates_with K, opts` → `K.new(opts).validate(self)`, under
        // the same Symbol `if:` / `unless:` guards as `validate :m`.
        // Rails builds the instance once, when the class body runs; here
        // it is built per validation run, so check_validity! raises at
        // the first `valid?` rather than at load.
        ValidationRule::With { validator, options, if_method, unless_method } => {
            let s = Span::synthetic();
            let bare = |m: &Symbol| {
                Expr::new(
                    s,
                    ExprNode::Send { recv: None, method: m.clone(), args: Vec::new(), block: None, parenthesized: false },
                )
            };
            let options = options.clone().unwrap_or_else(|| {
                Expr::new(s, ExprNode::Hash { entries: Vec::new(), kwargs: false })
            });
            let instance = Expr::new(
                s,
                ExprNode::Send {
                    recv: Some(validator.clone()),
                    method: Symbol::from("new"),
                    args: vec![options],
                    block: None,
                    parenthesized: true,
                },
            );
            let call = Expr::new(
                s,
                ExprNode::Send {
                    recv: Some(instance),
                    method: Symbol::from("validate"),
                    args: vec![Expr::new(s, ExprNode::SelfRef)],
                    block: None,
                    parenthesized: true,
                },
            );
            let nil = || Expr::new(s, ExprNode::Lit { value: Literal::Nil });
            let guarded = match unless_method {
                Some(c) => Expr::new(s, ExprNode::If { cond: bare(c), then_branch: nil(), else_branch: call }),
                None => call,
            };
            let guarded = match if_method {
                Some(c) => Expr::new(s, ExprNode::If { cond: bare(c), then_branch: guarded, else_branch: nil() }),
                None => guarded,
            };
            vec![guarded]
        }
        ValidationRule::Uniqueness { .. } => {
            // Not yet exercised by real-blog; lands when a fixture forces the issue.
            Vec::new()
        }
    }
}

/// Inline `belongs_to` presence check (Rails 5+ default — every
/// non-optional `belongs_to` requires the associated record to
/// exist). Generates the IR equivalent of:
///   if @article_id.nil? || @article_id == 0 || !Article.exists?(@article_id)
///     errors << "article must exist"
///   end
/// Mirrors `runtime/ruby/active_record/validations.rb::validates_belongs_to`
/// but flattens the early-return + post-check sequence to a single
/// composite condition.
///
/// A POLYMORPHIC association keeps only the first two terms. The class
/// to query is in the row's `<assoc>_type` COLUMN, so there is no
/// constant to write here — and `target` for a polymorphic belongs_to
/// is the assoc-name phantom (`Record` for `belongs_to :record`), a
/// class that does not exist. Emitting `Record.exists?(@record_id)`
/// was an uninitialized-constant NameError on the first save of any
/// polymorphic child; `ActionText::RichText` is the first model in the
/// corpus to reach it. Dropping the existence half is a strict SUBSET
/// of Rails' check — it can fail a save Rails would fail, never one
/// Rails would pass — and the id-presence half is the part that
/// catches the real error (an unset owner).
fn inline_belongs_to_check(
    assoc_name: &Symbol,
    foreign_key: &Symbol,
    target: &ClassId,
    polymorphic: bool,
) -> Expr {
    let fk_ivar = ivar(foreign_key);
    // `@fk.nil?`
    let nil_check = send(fk_ivar.clone(), "nil?", vec![]);
    // `@fk == 0`
    let zero_check = send(
        fk_ivar.clone(),
        "==",
        vec![Expr::new(
            Span::synthetic(),
            ExprNode::Lit { value: Literal::Int { value: 0 } },
        )],
    );
    let push_err = errors_push(format!("{} must exist", humanize(assoc_name.as_str())));
    if polymorphic {
        return if_with_nil_else(bool_op(BoolOpKind::Or, nil_check, zero_check), push_err);
    }
    // `!Target.exists?(@fk)`
    let target_const = Expr::new(
        Span::synthetic(),
        ExprNode::Const { path: vec![target.0.clone()] },
    );
    let exists_call = send(target_const, "exists?", vec![fk_ivar]);
    let not_exists = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(exists_call),
            method: Symbol::from("!"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    let cond = bool_op(
        BoolOpKind::Or,
        bool_op(BoolOpKind::Or, nil_check, zero_check),
        not_exists,
    );
    if_with_nil_else(cond, push_err)
}

/// `@<attr>` — direct ivar read passed as the `value` positional arg
/// to every validates_* helper.
fn ivar(attr: &Symbol) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Ivar { name: attr.clone() })
}

/// Inline `validates :attr, presence: true` expansion.
/// Generates the IR equivalent of:
///   if @attr.nil? || (@attr.is_a?(String) && @attr.empty?) || (@attr.is_a?(Array) && @attr.empty?)
///     errors << "attr can't be blank"
///   end
///
/// Mirrors `runtime/ruby/active_record/validations.rb::validates_presence_of`
/// exactly; no behavior change vs. the helper-call form, but the
/// expansion lives in the IR so every target emits inline checks
/// instead of routing through the Validations module at runtime.
/// When `attr_ty` is `Some(Ty::Str)` the `is_a?(Array)` arm drops out
/// (typed targets like TS narrow `Array.isArray(stringField)` to
/// `never` and reject the subsequent property access). Same logic for
/// `Some(Ty::Array { .. })` — the String arm drops. `None` keeps the
/// generic three-way form so untyped/dynamic-shape attrs still work.
fn inline_presence_check(attr: &Symbol, attr_ty: Option<&Ty>, text: &ErrorText<'_>) -> Expr {
    // Temporal columns (schema-typed `Time`) store ISO-8601 text in
    // `@<attr>_raw`; `@<attr>` is never the storage slot (the ruby
    // tree's parse memo is `@__t_<attr>`), so a check against it fired
    // unconditionally — lobsters' `validates :created_at, presence:
    // true` on Username rejected every record. Blank on the stored
    // form is nil-or-empty text.
    if matches!(attr_ty, Some(Ty::Time)) {
        let raw_ivar = ivar(&Symbol::from(format!("{}_raw", attr.as_str())));
        let cond = bool_op(
            BoolOpKind::Or,
            send(raw_ivar.clone(), "nil?", vec![]),
            send(raw_ivar, "empty?", vec![]),
        );
        let push_err = text.push("can't be blank", None, || ivar(attr));
        return if_with_nil_else(cond, push_err);
    }
    let attr_ivar = ivar(attr);
    // `@attr.nil?`
    let nil_check = send(attr_ivar.clone(), "nil?", vec![]);
    // Nullable column: the `nil?` disjunct already covers nil, so the
    // emptiness test reads the narrowed value — `@attr.empty?` on an
    // Option doesn't compile on a strict target.
    let (attr_ty, nilable) = peel_nilable(attr_ty);
    let attr_ivar = if nilable {
        match attr_ty {
            Some(inner) => narrowed_ivar(attr, inner),
            None => attr_ivar,
        }
    } else {
        attr_ivar
    };
    let cond = match attr_ty {
        Some(Ty::Str) => {
            // Skip is_a?(Array) — `body : String` can never be an array.
            bool_op(
                BoolOpKind::Or,
                nil_check,
                send(attr_ivar, "empty?", vec![]),
            )
        }
        Some(Ty::Array { .. }) => {
            // Skip is_a?(String) — symmetric.
            bool_op(
                BoolOpKind::Or,
                nil_check,
                send(attr_ivar, "empty?", vec![]),
            )
        }
        _ => {
            // Generic: `@attr.nil? || (@attr.is_a?(String) && @attr.empty?) ||
            //          (@attr.is_a?(Array) && @attr.empty?)`
            let string_blank = bool_op(
                BoolOpKind::And,
                is_a_check(&attr_ivar, "String"),
                send(attr_ivar.clone(), "empty?", vec![]),
            );
            let array_blank = bool_op(
                BoolOpKind::And,
                is_a_check(&attr_ivar, "Array"),
                send(attr_ivar, "empty?", vec![]),
            );
            bool_op(
                BoolOpKind::Or,
                bool_op(BoolOpKind::Or, nil_check, string_blank),
                array_blank,
            )
        }
    };
    // `errors << "attr can't be blank"`
    let push_err = text.push("can't be blank", None, || ivar(attr));
    // The wrapping `if cond then push_err end` (Nil else).
    if_with_nil_else(cond, push_err)
}

/// Inline `validates :attr, absence: true` — the negation of presence.
///   if !(@attr.nil? || (@attr.is_a?(String) && @attr.empty?) || (@attr.is_a?(Array) && @attr.empty?))
///     errors << "attr must be blank"
///   end
/// Reuses the presence condition tree and wraps with unary `!`.
fn inline_absence_check(attr: &Symbol, text: &ErrorText<'_>) -> Expr {
    // Re-derive the blank-condition (matches inline_presence_check's tree).
    let attr_ivar = ivar(attr);
    let nil_check = send(attr_ivar.clone(), "nil?", vec![]);
    let string_blank = bool_op(
        BoolOpKind::And,
        is_a_check(&attr_ivar, "String"),
        send(attr_ivar.clone(), "empty?", vec![]),
    );
    let array_blank = bool_op(
        BoolOpKind::And,
        is_a_check(&attr_ivar, "Array"),
        send(attr_ivar, "empty?", vec![]),
    );
    let blank_cond = bool_op(
        BoolOpKind::Or,
        bool_op(BoolOpKind::Or, nil_check, string_blank),
        array_blank,
    );
    let not_blank = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(blank_cond),
            method: Symbol::from("!"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    let push_err = text.push("must be blank", None, || ivar(attr));
    if_with_nil_else(not_blank, push_err)
}

/// Inline `validates :attr, inclusion: { in: LIST }` (`exclusion:` with
/// `exclusion`), asking the list as Rails does:
///   errors << "Attr is not included in the list" unless LIST.include?(@attr)
///   errors << "Attr is reserved" if LIST.include?(@attr)
/// `cover` (a Range literal over numbers) asks `cover?`. The list is the
/// expression as written, so a constant resolves where the class body
/// would resolve it.
fn inline_inclusion_check(
    attr: &Symbol,
    list: &Expr,
    exclusion: bool,
    cover: bool,
    text: &ErrorText<'_>,
) -> Expr {
    let member = send(list.clone(), if cover { "cover?" } else { "include?" }, vec![ivar(attr)]);
    if exclusion {
        return if_with_nil_else(member, text.push("is reserved", None, || ivar(attr)));
    }
    if_with_nil_else(not(member), text.push("is not included in the list", None, || ivar(attr)))
}

/// Inline `validates :attr, format: { with: /re/ }` (`without:` with
/// `without`). Rails matches `value.to_s`, so nil reads as "":
///   errors << "Attr is invalid" unless /re/.match?(@attr.to_s)
fn inline_format_check(attr: &Symbol, pattern: &str, flags: &str, without: bool, text: &ErrorText<'_>) -> Expr {
    let regex_lit = Expr::new(
        Span::synthetic(),
        ExprNode::Lit { value: Literal::Regex { pattern: pattern.to_string(), flags: flags.to_string() } },
    );
    let matched = send(regex_lit, "match?", vec![send(ivar(attr), "to_s", vec![])]);
    let invalid = if without { matched } else { not(matched) };
    if_with_nil_else(invalid, text.push("is invalid", None, || ivar(attr)))
}

/// Inline `validates :attr, length: { minimum: M, maximum: N, is: K }`.
/// Produces a single outer `unless @attr.nil?` guard wrapping a Seq:
///   unless @attr.nil?
///     len = if @attr.is_a?(String) then @attr.length elsif @attr.is_a?(Array) then @attr.length else 0 end
///     errors << "Attr is too short (minimum is M characters)" if len < M  # if min set
///     errors << "Attr is too long (maximum is N characters)"  if len > N  # if max set
///     errors << "attr is the wrong length (should be K)" if len != K  # if is set
///   end
/// The `is` (exact length) option isn't in the current ValidationRule
/// shape; left for when the IR adds it.
fn inline_length_check(
    attr: &Symbol,
    min: Option<usize>,
    max: Option<usize>,
    text: &ErrorText<'_>,
    attr_ty: Option<&Ty>,
) -> Vec<Expr> {
    let (attr_ty, nilable) = peel_nilable(attr_ty);
    // Same narrowing as the presence check: the caller guards the
    // length computation with `!@attr.nil?`, so the read inside it is
    // the nil-excluded value.
    let attr_ivar = match (nilable, attr_ty) {
        (true, Some(inner)) => narrowed_ivar(attr, inner),
        _ => ivar(attr),
    };
    // Compute `len`. When the attr's column type is statically known
    // (Str / Array), drop the `is_a?` discrimination — `body : String`
    // can never be an Array, and tsc narrows the dead branch to
    // `never` and rejects the subsequent `.length`. Generic three-way
    // form retained for unknown/dynamic attrs.
    let length_send = send(attr_ivar.clone(), "length", vec![]);
    let len_expr = match attr_ty {
        Some(Ty::Str) | Some(Ty::Array { .. }) => length_send,
        _ => {
            let zero_lit = Expr::new(
                Span::synthetic(),
                ExprNode::Lit { value: Literal::Int { value: 0 } },
            );
            let inner_else = Expr::new(
                Span::synthetic(),
                ExprNode::If {
                    cond: is_a_check(&attr_ivar, "Array"),
                    then_branch: length_send.clone(),
                    else_branch: zero_lit,
                },
            );
            Expr::new(
                Span::synthetic(),
                ExprNode::If {
                    cond: is_a_check(&attr_ivar, "String"),
                    then_branch: length_send,
                    else_branch: inner_else,
                },
            )
        }
    };
    let len_var = Symbol::from("len");
    let len_assign = Expr::new(
        Span::synthetic(),
        ExprNode::Assign {
            target: crate::expr::LValue::Var {
                id: crate::ident::VarId(0),
                name: len_var.clone(),
            },
            value: len_expr,
        },
    );
    let len_read = Expr::new(
        Span::synthetic(),
        ExprNode::Var { id: crate::ident::VarId(0), name: len_var },
    );

    let mut inner_stmts: Vec<Expr> = vec![len_assign];
    if let Some(n) = min {
        let lt = send(
            len_read.clone(),
            "<",
            vec![Expr::new(
                Span::synthetic(),
                ExprNode::Lit { value: Literal::Int { value: n as i64 } },
            )],
        );
        let count = Literal::Int { value: n as i64 };
        let msg = text.push("is too short (minimum is %{count} characters)", Some(&count), || ivar(attr));
        inner_stmts.push(if_with_nil_else(lt, msg));
    }
    if let Some(n) = max {
        let gt = send(
            len_read.clone(),
            ">",
            vec![Expr::new(
                Span::synthetic(),
                ExprNode::Lit { value: Literal::Int { value: n as i64 } },
            )],
        );
        let count = Literal::Int { value: n as i64 };
        let msg = text.push("is too long (maximum is %{count} characters)", Some(&count), || ivar(attr));
        inner_stmts.push(if_with_nil_else(gt, msg));
    }
    let body_seq = seq(inner_stmts);

    // A nil value has no length to read. LengthValidator still fails it
    // on `minimum:` (nil is shorter than any minimum) and passes it on
    // `maximum:` (`skip_nil_check?`), so:
    //   if @attr.nil? then <too short, when min is set> else body end
    // Tests the RAW ivar — `attr_ivar` above may be the narrowed `Cast`
    // read, which is exactly what this guard exists to make safe.
    let nil_check = send(ivar(attr), "nil?", vec![]);
    let nil_branch = match min {
        Some(n) => {
            let count = Literal::Int { value: n as i64 };
            text.push("is too short (minimum is %{count} characters)", Some(&count), || ivar(attr))
        }
        None => Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
    };
    vec![Expr::new(
        Span::synthetic(),
        ExprNode::If { cond: nil_check, then_branch: nil_branch, else_branch: body_seq },
    )]
}

/// Inline `validates :attr, numericality: { ... }`, NumericalityValidator's
/// order: the value parsed the way `parse_as_number` does (a Numeric as
/// is, an integer String with `to_i`, any other non-hexadecimal String
/// with `Kernel.Float`), then
///   number = <parsed or nil>
///   if number.nil?
///     errors << "Attr is not a number"
///   elsif <only_integer> && !/\A[+-]?\d+\z/.match?(@attr.to_s)
///     errors << "Attr must be an integer"
///   else
///     errors << "Attr must be greater than 0" unless number > 0
///     ...                                      # each option, in order
///   end
/// Every failing comparison adds its own error, as natively.
fn inline_numericality_check(
    attr: &Symbol,
    only_integer: bool,
    checks: &[NumericCheck],
    text: &ErrorText<'_>,
) -> Vec<Expr> {
    let s = Span::synthetic();
    let raw = || ivar(attr);
    let regex = |pattern: &str| {
        Expr::new(s, ExprNode::Lit { value: Literal::Regex { pattern: pattern.into(), flags: String::new() } })
    };
    let integer_re = r"\A[+-]?\d+\z";
    let nil = || Expr::new(s, ExprNode::Lit { value: Literal::Nil });
    let is_string = || is_a_check(&raw(), "String");
    let float_parse = Expr::new(
        s,
        ExprNode::Send {
            recv: None,
            method: Symbol::from("Float"),
            args: vec![
                raw(),
                Expr::new(
                    s,
                    ExprNode::Hash {
                        entries: vec![(
                            Expr::new(s, ExprNode::Lit { value: Literal::Sym { value: Symbol::from("exception") } }),
                            Expr::new(s, ExprNode::Lit { value: Literal::Bool { value: false } }),
                        )],
                        kwargs: true,
                    },
                ),
            ],
            block: None,
            parenthesized: true,
        },
    );
    let parsed = Expr::new(
        s,
        ExprNode::If {
            cond: is_a_check(&raw(), "Numeric"),
            then_branch: raw(),
            else_branch: Expr::new(
                s,
                ExprNode::If {
                    cond: bool_op(BoolOpKind::And, is_string(), send(regex(integer_re), "match?", vec![raw()])),
                    then_branch: send(raw(), "to_i", vec![]),
                    else_branch: Expr::new(
                        s,
                        ExprNode::If {
                            cond: bool_op(
                                BoolOpKind::And,
                                is_string(),
                                not(send(regex(r"\A[+-]?0[xX]"), "match?", vec![raw()])),
                            ),
                            then_branch: float_parse,
                            else_branch: nil(),
                        },
                    ),
                },
            ),
        },
    );
    let name = Symbol::from(format!("{}_number", attr.as_str()));
    let number = || Expr::new(s, ExprNode::Var { id: crate::ident::VarId(0), name: name.clone() });
    let assign = Expr::new(
        s,
        ExprNode::Assign {
            target: crate::expr::LValue::Var { id: crate::ident::VarId(0), name: name.clone() },
            value: parsed,
        },
    );
    let mut compares: Vec<Expr> = Vec::new();
    for check in checks {
        let op = match check.op {
            NumericOp::GreaterThan => ">",
            NumericOp::GreaterThanOrEqualTo => ">=",
            NumericOp::EqualTo => "==",
            NumericOp::LessThan => "<",
            NumericOp::LessThanOrEqualTo => "<=",
            NumericOp::OtherThan => "!=",
            NumericOp::Odd | NumericOp::Even => {
                let parity = if check.op == NumericOp::Odd { "odd?" } else { "even?" };
                let holds = send(send(number(), "to_i", vec![]), parity, vec![]);
                let default = if check.op == NumericOp::Odd { "must be odd" } else { "must be even" };
                compares.push(if_with_nil_else(not(holds), text.push(default, None, number)));
                continue;
            }
        };
        let Some(bound) = &check.bound else { continue };
        let holds = send(number(), op, vec![Expr::new(s, ExprNode::Lit { value: bound.clone() })]);
        let default = match check.op {
            NumericOp::GreaterThan => "must be greater than %{count}",
            NumericOp::GreaterThanOrEqualTo => "must be greater than or equal to %{count}",
            NumericOp::EqualTo => "must be equal to %{count}",
            NumericOp::LessThan => "must be less than %{count}",
            NumericOp::LessThanOrEqualTo => "must be less than or equal to %{count}",
            _ => "must be other than %{count}",
        };
        compares.push(if_with_nil_else(not(holds), text.push(default, Some(bound), number)));
    }
    let tail = if compares.is_empty() { nil() } else { seq(compares) };
    let tail = if only_integer {
        Expr::new(
            s,
            ExprNode::If {
                cond: not(send(regex(integer_re), "match?", vec![send(raw(), "to_s", vec![])])),
                then_branch: text.push("must be an integer", None, raw),
                else_branch: tail,
            },
        )
    } else {
        tail
    };
    let check = Expr::new(
        s,
        ExprNode::If {
            cond: send(number(), "nil?", vec![]),
            then_branch: text.push("is not a number", None, raw),
            else_branch: tail,
        },
    );
    vec![assign, check]
}

/// A numeric bound as ActiveModel interpolates it into `%{count}`: an
/// Integer as written, a Float through `BigDecimal(f, Float::DIG)#to_s`
/// (`1.5` → "0.15e1", `100.0` → "0.1e3").
fn format_count(count: &Literal) -> String {
    match count {
        Literal::Int { value } => value.to_string(),
        Literal::Float { value } => bigdecimal_to_s(*value),
        Literal::Str { value } => value.clone(),
        other => format!("{other:?}"),
    }
}

fn bigdecimal_to_s(f: f64) -> String {
    if f == 0.0 {
        return "0.0".into();
    }
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    // 15 significant digits, as `BigDecimal(f, Float::DIG)` keeps.
    let sci = format!("{:.14e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("exponent");
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    format!("{}0.{digits}e{}", if f < 0.0 { "-" } else { "" }, exp + 1)
}

/// How a rule's errors read: ActiveModel's default text, or the
/// validator's `message:`. Either way the full message is the humanized
/// attribute, a space, and the message (`"%{attribute} %{message}"`).
pub(super) struct ErrorText<'a> {
    attr: &'a Symbol,
    model_human: &'a str,
    message: Option<&'a RuleMessage>,
}

impl ErrorText<'_> {
    /// `errors << "<Attr> <message>"` for an error whose default text is
    /// `default` (its `%{count}` the formatted `count`) and whose value
    /// (what `%{value}` and a callable message see) is `value`.
    fn push(&self, default: &str, count: Option<&Literal>, value: impl Fn() -> Expr) -> Expr {
        let human = humanize(self.attr.as_str());
        let count_text = count.map(format_count).unwrap_or_default();
        match self.message {
            None => errors_push(format!("{human} {}", default.replace("%{count}", &count_text))),
            Some(RuleMessage::Text { text }) => {
                // I18n interpolation of the keys ActiveModel supplies;
                // ingest admits no others.
                let mut parts: Vec<InterpPart> = vec![InterpPart::Text { value: format!("{human} ") }];
                let mut rest = text.as_str();
                let push_text = |parts: &mut Vec<InterpPart>, t: &str| match parts.last_mut() {
                    Some(InterpPart::Text { value }) => value.push_str(t),
                    _ => parts.push(InterpPart::Text { value: t.to_string() }),
                };
                while let Some(start) = rest.find("%{") {
                    let tail = &rest[start + 2..];
                    let Some(end) = tail.find('}') else { break };
                    push_text(&mut parts, &rest[..start]);
                    match &tail[..end] {
                        "attribute" => push_text(&mut parts, &human),
                        "model" => push_text(&mut parts, self.model_human),
                        "count" => push_text(&mut parts, &count_text),
                        "value" => parts.push(InterpPart::Expr { expr: value() }),
                        other => push_text(&mut parts, &format!("%{{{other}}}")),
                    }
                    rest = &tail[end + 1..];
                }
                push_text(&mut parts, rest);
                match parts.as_slice() {
                    [InterpPart::Text { value }] => errors_push(value.clone()),
                    _ => errors_push_expr(Expr::new(Span::synthetic(), ExprNode::StringInterp { parts })),
                }
            }
            Some(RuleMessage::Call { callable }) => {
                // `message.call(record, model:, attribute:, value:[, count:])`.
                let s = Span::synthetic();
                let sym = |n: &str| Expr::new(s, ExprNode::Lit { value: Literal::Sym { value: Symbol::from(n) } });
                let str_lit = |t: &str| Expr::new(s, ExprNode::Lit { value: Literal::Str { value: t.to_string() } });
                let mut entries = vec![
                    (sym("model"), str_lit(self.model_human)),
                    (sym("attribute"), str_lit(&human)),
                    (sym("value"), value()),
                ];
                if let Some(c) = count {
                    entries.push((sym("count"), Expr::new(s, ExprNode::Lit { value: c.clone() })));
                }
                let call = Expr::new(
                    s,
                    ExprNode::Send {
                        recv: Some(callable.clone()),
                        method: Symbol::from("call"),
                        args: vec![Expr::new(s, ExprNode::SelfRef), Expr::new(s, ExprNode::Hash { entries, kwargs: true })],
                        block: None,
                        parenthesized: true,
                    },
                );
                errors_push_expr(Expr::new(
                    s,
                    ExprNode::StringInterp {
                        parts: vec![InterpPart::Text { value: format!("{human} ") }, InterpPart::Expr { expr: call }],
                    },
                ))
            }
        }
    }
}

/// A rule's checks under its `allow_nil:` / `allow_blank:` (EachValidator
/// skips such a value) and Symbol `if:` / `unless:` (both given: `if`
/// holds and `unless` does not).
fn guard_rule(value: Expr, checks: Vec<Expr>, o: &RuleOptions) -> Vec<Expr> {
    if checks.is_empty() {
        return checks;
    }
    let s = Span::synthetic();
    let nil = || Expr::new(s, ExprNode::Lit { value: Literal::Nil });
    let mut body = if checks.len() == 1 { checks.into_iter().next().unwrap() } else { seq(checks) };
    let skip_nil = o.allow_nil.then(|| send(value.clone(), "nil?", vec![]));
    let skip_blank = o.allow_blank.then(|| {
        send(Expr::new(s, ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] }), "blank?", vec![value.clone()])
    });
    let skip = match (skip_nil, skip_blank) {
        (Some(a), Some(b)) => Some(bool_op(BoolOpKind::Or, a, b)),
        (a, b) => a.or(b),
    };
    if let Some(cond) = skip {
        body = Expr::new(s, ExprNode::If { cond, then_branch: nil(), else_branch: body });
    }
    let bare = |m: &Symbol| {
        Expr::new(s, ExprNode::Send { recv: None, method: m.clone(), args: Vec::new(), block: None, parenthesized: false })
    };
    if let Some(c) = &o.unless_method {
        body = Expr::new(s, ExprNode::If { cond: bare(c), then_branch: nil(), else_branch: body });
    }
    if let Some(c) = &o.if_method {
        body = Expr::new(s, ExprNode::If { cond: bare(c), then_branch: body, else_branch: nil() });
    }
    vec![body]
}

fn not(e: Expr) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send { recv: Some(e), method: Symbol::from("!"), args: vec![], block: None, parenthesized: false },
    )
}

/// Humanize an attribute name for an error message, matching Rails'
/// `errors.full_messages` (`String#humanize` + the `"%{attribute}
/// %{message}"` format): drop a trailing `_id`, turn `_` into spaces,
/// and upcase the first letter. So `body` → `Body`, `author_id` →
/// `Author`, `first_name` → `First name`. The full message is baked
/// here (this lowerer inlines literal strings, not runtime
/// interpolation — see the module header), so the humanization has to
/// happen at lower time rather than in an errors object.
pub(crate) fn humanize(attr: &str) -> String {
    let trimmed = attr.strip_suffix("_id").unwrap_or(attr);
    let spaced = trimmed.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

// ── IR helpers ────────────────────────────────────────────────

fn send(recv: Expr, method: &str, args: Vec<Expr>) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from(method),
            args,
            block: None,
            parenthesized: true,
        },
    )
}

fn bool_op(op: BoolOpKind, left: Expr, right: Expr) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::BoolOp { op, surface: BoolOpSurface::default(), left, right },
    )
}

fn is_a_check(value: &Expr, class_name: &str) -> Expr {
    let class_const = Expr::new(
        Span::synthetic(),
        ExprNode::Const { path: vec![Symbol::from(class_name)] },
    );
    send(value.clone(), "is_a?", vec![class_const])
}

/// `errors << <msg_expr>` — pushes a String literal onto the `errors`
/// collection. `errors` is reached via implicit-self Send (the same
/// shape every existing validates_*_of helper produces inside the
/// Validations module).
fn errors_push(msg: String) -> Expr {
    errors_push_expr(Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Str { value: msg } }))
}

/// `errors << <msg>` for a message built at run time.
fn errors_push_expr(msg_expr: Expr) -> Expr {
    let errors_call = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: None,
            method: Symbol::from("errors"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    send(errors_call, "<<", vec![msg_expr])
}

fn if_with_nil_else(cond: Expr, then_branch: Expr) -> Expr {
    let nil_lit = Expr::new(
        Span::synthetic(),
        ExprNode::Lit { value: Literal::Nil },
    );
    Expr::new(
        Span::synthetic(),
        ExprNode::If { cond, then_branch, else_branch: nil_lit },
    )
}

#[cfg(test)]
mod tests {
    //! Unit tests per ValidationRule arm. Replaces the deleted
    //! `runtime/ruby/test/active_record/validations_test.rb` framework
    //! test, which tested the runtime `validates_*_of` helper methods —
    //! a surface no Group 1 target dispatches into any more (Phase 2.5(a)
    //! inlines every validates declaration here).
    //!
    //! Each test calls the inline helper directly and asserts on the
    //! error-message strings the lowered IR emits, since the error text
    //! is the user-visible contract. Structural shape is covered
    //! implicitly: the message-collection walker only reaches strings
    //! that sit at `errors << "..."` positions.
    use super::*;
    use crate::ident::ClassId;

    /// Walk an `Expr` tree and collect every string literal that's the
    /// RHS of an `errors << "..."` send (the canonical error-push shape
    /// emitted by `errors_push`). Used to assert which error messages
    /// the lowerer arms produce.
    fn collect_error_messages(expr: &Expr) -> Vec<String> {
        let mut out = Vec::new();
        walk(expr, &mut out);
        out
    }

    fn walk(expr: &Expr, out: &mut Vec<String>) {
        match expr.node.as_ref() {
            ExprNode::Send { recv, method, args, .. } => {
                if method.as_str() == "<<" {
                    if let Some(r) = recv.as_ref() {
                        if let ExprNode::Send { method: m, recv: None, .. } = r.node.as_ref() {
                            if m.as_str() == "errors" {
                                if let Some(arg) = args.first() {
                                    if let ExprNode::Lit { value: Literal::Str { value } } =
                                        arg.node.as_ref()
                                    {
                                        out.push(value.clone());
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(r) = recv {
                    walk(r, out);
                }
                for a in args {
                    walk(a, out);
                }
            }
            ExprNode::BoolOp { left, right, .. } => {
                walk(left, out);
                walk(right, out);
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                walk(cond, out);
                walk(then_branch, out);
                walk(else_branch, out);
            }
            ExprNode::Seq { exprs } => {
                for e in exprs {
                    walk(e, out);
                }
            }
            ExprNode::Assign { value, .. } => walk(value, out),
            _ => {}
        }
    }

    fn attr() -> Symbol {
        Symbol::from("title")
    }

    fn plain(attr: &Symbol) -> ErrorText<'_> {
        ErrorText { attr, model_human: "Post", message: None }
    }

    #[test]
    fn presence_emits_blank_error() {
        let a = attr();
        let expr = inline_presence_check(&a, None, &plain(&a));
        assert_eq!(collect_error_messages(&expr), vec!["Title can't be blank"]);
    }

    #[test]
    fn presence_str_typed_attr_drops_is_a_array_branch() {
        // Statically-typed string attr: the `is_a?(Array) && ...` arm
        // must drop out, otherwise tsc narrows the dead branch to
        // `never` and rejects the subsequent `.length` access.
        let a = attr();
        let expr = inline_presence_check(&a, Some(&Ty::Str), &plain(&a));
        let dbg = format!("{:?}", expr);
        assert!(
            !dbg.contains("\"Array\""),
            "Str-typed attr should drop is_a?(Array); tree: {dbg}",
        );
    }

    #[test]
    fn presence_array_typed_attr_drops_is_a_string_branch() {
        let a = attr();
        let expr = inline_presence_check(&a, Some(&Ty::Array { elem: Box::new(Ty::Str) }), &plain(&a));
        let dbg = format!("{:?}", expr);
        assert!(
            !dbg.contains("\"String\""),
            "Array-typed attr should drop is_a?(String); tree: {dbg}",
        );
    }

    #[test]
    fn presence_message_replaces_the_default_text() {
        let a = attr();
        let message = RuleMessage::Text { text: "must be given".into() };
        let text = ErrorText { attr: &a, model_human: "Post", message: Some(&message) };
        let expr = inline_presence_check(&a, None, &text);
        assert_eq!(collect_error_messages(&expr), vec!["Title must be given"]);
    }

    #[test]
    fn absence_emits_must_be_blank_error() {
        let a = attr();
        let expr = inline_absence_check(&a, &plain(&a));
        assert_eq!(collect_error_messages(&expr), vec!["Title must be blank"]);
    }

    #[test]
    fn length_min_only_emits_too_short() {
        // Twice: a nil value is too short as well (LengthValidator).
        let a = attr();
        let exprs = inline_length_check(&a, Some(5), None, &plain(&a), None);
        assert_eq!(exprs.len(), 1, "length lowers to one outer expression");
        let msgs: Vec<String> = exprs.iter().flat_map(collect_error_messages).collect();
        assert_eq!(
            msgs,
            vec!["Title is too short (minimum is 5 characters)", "Title is too short (minimum is 5 characters)"]
        );
    }

    #[test]
    fn length_max_only_emits_too_long() {
        let a = attr();
        let exprs = inline_length_check(&a, None, Some(100), &plain(&a), None);
        let msgs: Vec<String> = exprs.iter().flat_map(collect_error_messages).collect();
        assert_eq!(msgs, vec!["Title is too long (maximum is 100 characters)"]);
    }

    #[test]
    fn length_min_and_max_emits_both_in_order() {
        let a = attr();
        let exprs = inline_length_check(&a, Some(5), Some(100), &plain(&a), None);
        let msgs: Vec<String> = exprs.iter().flat_map(collect_error_messages).collect();
        assert_eq!(
            msgs,
            vec![
                "Title is too short (minimum is 5 characters)",
                "Title is too short (minimum is 5 characters)",
                "Title is too long (maximum is 100 characters)",
            ],
        );
    }

    #[test]
    fn length_message_interpolates_count() {
        let a = attr();
        let message = RuleMessage::Text { text: "needs %{count} for %{model}".into() };
        let text = ErrorText { attr: &a, model_human: "Post", message: Some(&message) };
        let exprs = inline_length_check(&a, None, Some(4), &text, None);
        let msgs: Vec<String> = exprs.iter().flat_map(collect_error_messages).collect();
        assert_eq!(msgs, vec!["Title needs 4 for Post"]);
    }

    #[test]
    fn format_emits_invalid_error() {
        let a = attr();
        let expr = inline_format_check(&a, "[A-Z]+", "", false, &plain(&a));
        assert_eq!(collect_error_messages(&expr), vec!["Title is invalid"]);
    }

    #[test]
    fn inclusion_and_exclusion_emit_their_errors() {
        let a = attr();
        let list = Expr::new(
            Span::synthetic(),
            ExprNode::Array {
                elements: ["a", "b"]
                    .iter()
                    .map(|v| Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Str { value: (*v).into() } }))
                    .collect(),
                style: ArrayStyle::Brackets,
            },
        );
        let expr = inline_inclusion_check(&a, &list, false, false, &plain(&a));
        assert_eq!(collect_error_messages(&expr), vec!["Title is not included in the list"]);
        let expr = inline_inclusion_check(&a, &list, true, false, &plain(&a));
        assert_eq!(collect_error_messages(&expr), vec!["Title is reserved"]);
    }

    #[test]
    fn numericality_bare_emits_nan_only() {
        let a = attr();
        let exprs = inline_numericality_check(&a, false, &[], &plain(&a));
        let msgs: Vec<String> = exprs.iter().flat_map(collect_error_messages).collect();
        assert_eq!(msgs, vec!["Title is not a number"]);
    }

    #[test]
    fn numericality_emits_each_option_in_activemodel_order() {
        let a = attr();
        let check = |op, bound| NumericCheck { op, bound };
        let checks = [
            check(NumericOp::GreaterThan, Some(Literal::Int { value: 0 })),
            check(NumericOp::LessThanOrEqualTo, Some(Literal::Int { value: 100 })),
            check(NumericOp::Odd, None),
        ];
        let exprs = inline_numericality_check(&a, true, &checks, &plain(&a));
        let msgs: Vec<String> = exprs.iter().flat_map(collect_error_messages).collect();
        assert_eq!(
            msgs,
            vec![
                "Title is not a number",
                "Title must be an integer",
                "Title must be greater than 0",
                "Title must be less than or equal to 100",
                "Title must be odd",
            ],
        );
    }

    #[test]
    fn numericality_float_bounds_read_as_bigdecimal() {
        // ActiveModel parses a Float option through BigDecimal(f,
        // Float::DIG) and interpolates its `to_s`.
        let a = attr();
        let checks = [NumericCheck { op: NumericOp::GreaterThan, bound: Some(Literal::Float { value: 0.5 }) }];
        let exprs = inline_numericality_check(&a, false, &checks, &plain(&a));
        let msgs: Vec<String> = exprs.iter().flat_map(collect_error_messages).collect();
        assert!(msgs.iter().any(|m| m == "Title must be greater than 0.5e0"), "got {msgs:?}");
        for (f, s) in [(1.5, "0.15e1"), (100.0, "0.1e3"), (-2.25, "-0.225e1"), (0.001, "0.1e-2"), (0.0, "0.0")] {
            assert_eq!(bigdecimal_to_s(f), s);
        }
    }

    #[test]
    fn belongs_to_inline_emits_must_exist_error() {
        let assoc_name = Symbol::from("article");
        let foreign_key = Symbol::from("article_id");
        let target = ClassId(Symbol::from("Article"));
        let expr = inline_belongs_to_check(&assoc_name, &foreign_key, &target, false);
        assert_eq!(collect_error_messages(&expr), vec!["Article must exist"]);
    }

    #[test]
    fn polymorphic_belongs_to_checks_the_id_but_names_no_class() {
        // The class to query lives in the row's `<assoc>_type` column,
        // and `target` is the assoc-name phantom — writing it out was an
        // uninitialized-constant NameError on the first save.
        let assoc_name = Symbol::from("record");
        let foreign_key = Symbol::from("record_id");
        let target = ClassId(Symbol::from("Record"));
        let expr = inline_belongs_to_check(&assoc_name, &foreign_key, &target, true);
        assert_eq!(collect_error_messages(&expr), vec!["Record must exist"]);
        let rendered = format!("{expr:?}");
        assert!(
            !rendered.contains("exists?"),
            "a polymorphic check must not query a class: {rendered}"
        );
    }
}

/// `Union{[T, Nil]}` → `T`, for the nullable-column slot types the
/// attributes row now carries. The checks below discriminate on the
/// underlying type and narrow the read with `Cast` — the IR's
/// nilable→scalar bridge — under a `nil?` guard that short-circuits in
/// every target.
fn peel_nilable(ty: Option<&Ty>) -> (Option<&Ty>, bool) {
    match ty {
        Some(Ty::Union { variants }) if variants.len() == 2 => {
            let inner = variants.iter().find(|v| !matches!(v, Ty::Nil));
            let has_nil = variants.iter().any(|v| matches!(v, Ty::Nil));
            match (inner, has_nil) {
                (Some(i), true) => (Some(i), true),
                _ => (ty, false),
            }
        }
        other => (other, false),
    }
}

/// `Cast(@attr, T)` — the nil-excluded read. Callers place it only
/// where a preceding `nil?` disjunct/conjunct guarantees the value.
fn narrowed_ivar(attr: &Symbol, inner: &Ty) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Cast { value: ivar(attr), target_ty: inner.clone() },
    )
}
