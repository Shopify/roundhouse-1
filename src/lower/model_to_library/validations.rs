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
    AccessorKind, Association, LengthCheck, LengthOp, MethodDef, MethodReceiver, Model, NumericCheck, NumericOp,
    RuleMessage, RuleOptions, RuleValue, ValidationRule,
};
use crate::effect::EffectSet;
use crate::expr::{ArrayStyle, BoolOpKind, BoolOpSurface, Expr, ExprNode, InterpPart, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;
use crate::ty::Ty;

use super::{fn_sig, seq};

/// The class whose class body declares a set of validations, as the
/// checks lowered from them see it: `model_name` for `%{model}` and the
/// I18n lookup, and the class a `uniqueness:` check queries.
pub(crate) struct Declarer {
    /// The declaring class (a model, or an STI subclass of one).
    pub name: ClassId,
    /// `lookup_ancestors`' `model_name.i18n_key`s, the declaring class
    /// first: ActiveRecord walks up to the `base_class`, so an STI
    /// subclass's missing translation names its own keys, then its
    /// parents' up to the base.
    pub i18n_keys: Vec<String>,
    /// The class `uniqueness:` asks (`find_finder_class_for`).
    pub finder: Finder,
}

/// The relation a `uniqueness:` check starts from. Rails walks from the
/// record's class up to the class that declared the validation, and the
/// last non-abstract one found is the finder; its `unscoped` relation
/// keeps an STI subclass's type condition.
pub(crate) enum Finder {
    /// An abstract declaring class: the record's own class answers.
    RecordClass,
    /// The declaring class, with `type IN (...)` when it is an STI
    /// subclass (itself and every subclass below it).
    Declaring { class: ClassId, type_names: Vec<String> },
}

impl Declarer {
    /// A model declaring its own validations.
    pub(crate) fn model(model: &Model) -> Self {
        Declarer {
            name: model.name.clone(),
            i18n_keys: vec![crate::naming::underscore(model.name.0.as_str())],
            finder: if super::markers::is_abstract_class(model) {
                Finder::RecordClass
            } else {
                Finder::Declaring { class: model.name.clone(), type_names: Vec::new() }
            },
        }
    }
}

pub(super) fn push_validate_method(methods: &mut Vec<MethodDef>, model: &Model) {
    let declarer = Declarer::model(model);
    let mut stmts = validation_statements(model, model.spanned_validations(), &declarer);

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
            stmts.push(check);
        }
    }

    stmts.extend(secure_password_checks(model));

    if stmts.is_empty() {
        // A tableless model (it includes ActiveModel::Validations or
        // ::Model) answers `valid?` / `errors` with no rules of its own,
        // and its subclasses' `validate` extends this one.
        let defines_validate = model.body.iter().any(|item| matches!(
            item,
            crate::dialect::ModelBodyItem::Method { method, .. }
                if method.name.as_str() == "validate" && method.receiver == MethodReceiver::Instance
        ));
        if model.parent.is_none() && !defines_validate {
            let nil = Expr::new(model.span, ExprNode::Lit { value: Literal::Nil });
            methods.push(validate_method_def(&model.name, vec![nil]));
            push_active_model_validation_surface(methods, model);
        }
        return;
    }

    methods.push(validate_method_def(&model.name, stmts));

    // A validating model OUTSIDE the ActiveRecord hierarchy (`class
    // Search` + `include ActiveModel::Validations`, no superclass)
    // can't inherit `valid?`/`errors` from the runtime Base —
    // synthesize the same pair here. Skipped when the source defines
    // either name itself.
    if model.parent.is_none() {
        push_active_model_validation_surface(methods, model);
    }
}

/// `def validate; <stmts>; end` on `owner`.
pub(crate) fn validate_method_def(owner: &ClassId, stmts: Vec<Expr>) -> MethodDef {
    MethodDef {
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
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

/// The checks `validations` (each with its declaration's span) lower
/// to, against `model`'s attributes and associations and as `declarer`
/// declares them, `prepend: true` ones first.
pub(crate) fn validation_statements<'v>(
    model: &Model,
    validations: impl Iterator<Item = (Span, &'v crate::dialect::Validation)>,
    declarer: &Declarer,
) -> Vec<Expr> {
    let (mut front, rest) = validation_statements_split(model, validations, declarer);
    front.extend(rest);
    front
}

/// [`validation_statements`] as (`prepend: true` checks, the rest). A
/// prepended callback goes to the front of the chain when it is
/// declared, ahead of every one before it (an STI subclass's ahead of
/// the ones it inherits), so a later prepend runs before an earlier one.
/// An STI subclass passes its base model and its own declarations.
pub(crate) fn validation_statements_split<'v>(
    model: &Model,
    validations: impl Iterator<Item = (Span, &'v crate::dialect::Validation)>,
    declarer: &Declarer,
) -> (Vec<Expr>, Vec<Expr>) {
    let mut front: Vec<Expr> = Vec::new();
    let mut stmts: Vec<Expr> = Vec::new();
    // `model_name.human`, which a `message:` reads as `%{model}`.
    let model_human = humanize(&crate::naming::underscore(crate::naming::demodulize(declarer.name.0.as_str())));
    // The I18n scope, which a Symbol `message:` with no English
    // translation names.
    let i18n_scope = if model.parent.is_some() { "activerecord" } else { "activemodel" };

    for (span, v) in validations {
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
        // Any association (a `has_many` too) is read through its reader,
        // as `read_attribute_for_validation` reads it.
        let any_assoc = model.associations().any(|a| a.name() == &v.attribute);
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
                i18n_keys: &declarer.i18n_keys,
                i18n_scope,
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
                    text.push("blank", None, || reader.clone()),
                );
                (vec![check], reader)
            } else {
                let value = if any_assoc { reader } else { ivar(&v.attribute) };
                (validation_rule_to_calls(model, &declarer.finder, &v.attribute, rule, attr_ty, &value, &text), value)
            };
            let prepend = options.is_some_and(|o| o.prepend);
            let checks = match options {
                Some(o) => guard_rule(value, checks, o),
                None => checks,
            };
            let mut checks: Vec<Expr> = checks
                .into_iter()
                .map(|mut check| {
                    // Each expanded check attributes to its `validates` line.
                    check.inherit_span(span);
                    check
                })
                .collect();
            if prepend {
                checks.append(&mut front);
                front = checks;
            } else {
                stmts.extend(checks);
            }
        }
    }
    (front, stmts)
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
    }
    out
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
pub(crate) fn push_active_model_validation_surface(methods: &mut Vec<MethodDef>, model: &Model) {
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
        // A rule's `on:` reads `validation_context`: ActiveModel's
        // `valid?(context = nil)` sets it for the run (no default — the
        // `:create` / `:update` default is ActiveRecord's) and restores
        // the previous one after.
        let contexts = uses_validation_context(model);
        let mut params = Vec::new();
        if contexts {
            let context = Symbol::from("validation_context");
            let previous = Symbol::from("previous_validation_context");
            let ctx_ivar = || Expr::new(span, ExprNode::Ivar { name: context.clone() });
            let ok = Symbol::from("validation_ok");
            let check = body_stmts.pop().expect("the empty? check");
            body_stmts.insert(0, assign_local(&previous, ctx_ivar()));
            body_stmts.insert(
                1,
                Expr::new(
                    span,
                    ExprNode::Assign {
                        target: crate::expr::LValue::Ivar { name: context.clone() },
                        value: local_read(&Symbol::from("context")),
                    },
                ),
            );
            body_stmts.push(assign_local(&ok, check));
            body_stmts.push(Expr::new(
                span,
                ExprNode::Assign {
                    target: crate::expr::LValue::Ivar { name: context.clone() },
                    value: local_read(&previous),
                },
            ));
            body_stmts.push(local_read(&ok));
            params.push(crate::dialect::Param {
                name: Symbol::from("context"),
                default: Some(Expr::new(span, ExprNode::Lit { value: Literal::Nil })),
                keyword: false,
                rest: false,
                forwarding: false,
                from_keyword: false,
                from_kwrest: false,
            });
        }
        let body = seq(body_stmts);
        let mut signature = fn_sig(vec![], Ty::Bool);
        if let (true, Ty::Fn { params, .. }) = (contexts, &mut signature) {
            params.push(crate::ty::Param {
                name: Symbol::from("context"),
                ty: Ty::Union { variants: vec![Ty::Sym, Ty::Nil] },
                kind: crate::ty::ParamKind::Optional,
            });
        }
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("valid?"),
            receiver: MethodReceiver::Instance,
            params,
            body,
            signature: Some(signature),
            effects: EffectSet::default(),
            enclosing_class: Some(model.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: true,
            block_param: None,
        });
    }
    if uses_validation_context(model) && !defines("validation_context") {
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("validation_context"),
            receiver: MethodReceiver::Instance,
            params: Vec::new(),
            body: Expr::new(span, ExprNode::Ivar { name: Symbol::from("validation_context") }),
            signature: Some(fn_sig(vec![], Ty::Union { variants: vec![Ty::Sym, Ty::Nil] })),
            effects: EffectSet::default(),
            enclosing_class: Some(model.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
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

/// Whether any rule runs `on:` a validation context.
fn uses_validation_context(model: &Model) -> bool {
    model.validations().any(|v| {
        v.rules.iter().any(|r| {
            matches!(r, ValidationRule::Guarded { options, .. } if !options.on.is_empty() || !options.except_on.is_empty())
        })
    })
}

/// Produce the list of helper-call expressions for one `ValidationRule` on
/// `attr`. Each helper is `<helper>(:attr, @attr [, kwargs])` — the value
/// is passed positionally so the runtime helper sees a concretely-typed
/// `value` parameter (no block-yield, no `instance_variable_get`).
fn validation_rule_to_calls(
    model: &Model,
    finder: &Finder,
    attr: &Symbol,
    rule: &ValidationRule,
    attr_ty: Option<&Ty>,
    value: &Expr,
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
        ValidationRule::Comparison { checks } => inline_comparison_check(attr, value, checks, text),
        // `validate do ... end`: the block, called inside the record's
        // method (its `self`, as under `instance_exec`).
        // `validate SomeValidations.new` → `<object>.validate(self)`.
        ValidationRule::Object { object } => vec![send(
            object.clone(),
            "validate",
            vec![Expr::new(Span::synthetic(), ExprNode::SelfRef)],
        )],
        ValidationRule::Block { callable, with_record } => {
            let args = if *with_record { vec![Expr::new(Span::synthetic(), ExprNode::SelfRef)] } else { Vec::new() };
            vec![Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: Some(callable.clone()),
                    method: Symbol::from("call"),
                    args,
                    block: None,
                    parenthesized: *with_record,
                },
            )]
        }
        ValidationRule::Inclusion { list_of: Some(members), exclusion, .. } => {
            vec![inline_inclusion_check(attr, &resolve(members), *exclusion, false, text)]
        }
        ValidationRule::Inclusion { list, exclusion, cover, list_of: None } => {
            vec![inline_inclusion_check(attr, list, *exclusion, *cover, text)]
        }
        ValidationRule::LengthOf { checks, nil_fails_maximum } => {
            inline_length_of_check(attr, value, attr_ty, checks, *nil_fails_maximum, text)
        }
        ValidationRule::FormatOf { regexp, without } => {
            let matched = send(resolve(regexp), "match?", vec![send(ivar(attr), "to_s", vec![])]);
            let invalid = if *without { matched } else { not(matched) };
            vec![if_with_nil_else(invalid, text.push("invalid", None, || ivar(attr)))]
        }
        // Reported by `roundhouse check` (`analyze::ingest_coverage`);
        // a weaker check here would pass records Rails rejects.
        ValidationRule::Unsupported { .. } => Vec::new(),
        // `push_validate_method` unwraps the options before asking.
        ValidationRule::Guarded { rule, .. } => {
            validation_rule_to_calls(model, finder, attr, rule, attr_ty, value, text)
        }
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
        // Reported by `roundhouse check`: there is no table to ask.
        ValidationRule::Uniqueness { .. } if model.parent.is_none() => Vec::new(),
        ValidationRule::Uniqueness { scope, case_sensitive, conditions } => {
            inline_uniqueness_check(model, finder, attr, attr_ty, scope, *case_sensitive, conditions.as_ref(), text)
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
        let push_err = text.push("blank", None, || ivar(attr));
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
    let push_err = text.push("blank", None, || ivar(attr));
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
    let push_err = text.push("present", None, || ivar(attr));
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
        return if_with_nil_else(member, text.push("exclusion", None, || ivar(attr)));
    }
    if_with_nil_else(not(member), text.push("inclusion", None, || ivar(attr)))
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
    if_with_nil_else(invalid, text.push("invalid", None, || ivar(attr)))
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
            // Rails reads anything else's `to_s.length`.
            let to_s_length = send(send(attr_ivar.clone(), "to_s", vec![]), "length", vec![]);
            let inner_else = Expr::new(
                Span::synthetic(),
                ExprNode::If {
                    cond: is_a_check(&attr_ivar, "Array"),
                    then_branch: length_send.clone(),
                    else_branch: to_s_length,
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
        let count = Count::Static(Literal::Int { value: n as i64 });
        let msg = text.push("too_short", Some(&count), || ivar(attr));
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
        let count = Count::Static(Literal::Int { value: n as i64 });
        let msg = text.push("too_long", Some(&count), || ivar(attr));
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
            let count = Count::Static(Literal::Int { value: n as i64 });
            text.push("too_short", Some(&count), || ivar(attr))
        }
        None => Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
    };
    vec![Expr::new(
        Span::synthetic(),
        ExprNode::If { cond: nil_check, then_branch: nil_branch, else_branch: body_seq },
    )]
}

/// `validates :attr, length: { ... }` in LengthValidator's general form
/// — `is:` / `minimum:` / `maximum:` in that order, each bound resolved
/// per run, each error with its own default message:
///   <attr>_minimum = <bound>               # for a non-literal bound
///   if value.nil?
///     errors << "Attr is too short (...)"  # is / minimum always fail nil
///   else
///     len = value.respond_to?(:length) ? value.length : value.to_s.length
///     errors << "Attr is too short (minimum is 3 characters)" unless len >= <attr>_minimum
///   end
/// A nil value fails `maximum:` too when `allow_nil:` / `allow_blank:`
/// is given (`skip_nil_check?`); otherwise nil's length, 0, passes it.
fn inline_length_of_check(
    attr: &Symbol,
    value: &Expr,
    attr_ty: Option<&Ty>,
    checks: &[LengthCheck],
    nil_fails_maximum: bool,
    text: &ErrorText<'_>,
) -> Vec<Expr> {
    let s = Span::synthetic();
    let mut out = Vec::new();
    let mut bounds: Vec<(Expr, Count)> = Vec::new();
    for check in checks {
        let key = match check.op {
            LengthOp::Is => "is",
            LengthOp::Minimum => "minimum",
            LengthOp::Maximum => "maximum",
        };
        bounds.push(match &check.bound {
            RuleValue::Literal { value } => {
                (Expr::new(s, ExprNode::Lit { value: value.clone() }), Count::Static(value.clone()))
            }
            other => {
                let local = Symbol::from(format!("{}_length_{key}", attr.as_str()));
                out.push(assign_local(&local, resolve(other)));
                (local_read(&local), Count::Dynamic(local_read(&local)))
            }
        });
    }
    let error = |check: &LengthCheck, count: &Count| {
        let kind = match check.op {
            LengthOp::Is => "wrong_length",
            LengthOp::Minimum => "too_short",
            LengthOp::Maximum => "too_long",
        };
        // A rule-level `message:` wins over `too_short:` and the like.
        let own = ErrorText { message: text.message.or(check.message.as_ref()), ..*text };
        own.push(kind, Some(count), || value.clone())
    };
    let (inner, _) = peel_nilable(attr_ty);
    let length = match inner {
        Some(Ty::Str) | Some(Ty::Array { .. }) => send(value.clone(), "length", vec![]),
        _ => Expr::new(
            s,
            ExprNode::If {
                cond: send(
                    value.clone(),
                    "respond_to?",
                    vec![Expr::new(s, ExprNode::Lit { value: Literal::Sym { value: Symbol::from("length") } })],
                ),
                then_branch: send(value.clone(), "length", vec![]),
                else_branch: send(send(value.clone(), "to_s", vec![]), "length", vec![]),
            },
        ),
    };
    let len = Symbol::from(format!("{}_length", attr.as_str()));
    let mut present = vec![assign_local(&len, length)];
    let mut absent = Vec::new();
    for (check, (bound, count)) in checks.iter().zip(&bounds) {
        let op = match check.op {
            LengthOp::Is => "==",
            LengthOp::Minimum => ">=",
            LengthOp::Maximum => "<=",
        };
        present.push(if_with_nil_else(not(send(local_read(&len), op, vec![bound.clone()])), error(check, count)));
        match check.op {
            LengthOp::Maximum if !nil_fails_maximum => {
                // nil's length is 0.
                let zero = Expr::new(s, ExprNode::Lit { value: Literal::Int { value: 0 } });
                absent.push(if_with_nil_else(not(send(zero, "<=", vec![bound.clone()])), error(check, count)));
            }
            _ => absent.push(error(check, count)),
        }
    }
    out.push(Expr::new(
        s,
        ExprNode::If { cond: send(value.clone(), "nil?", vec![]), then_branch: seq(absent), else_branch: seq(present) },
    ));
    out
}

/// `validates :attr, uniqueness: { ... }` — UniquenessValidator's query
/// against the record's own class, unscoped:
///   <attr>_relation = ActiveRecord::Relation.new(self.class).where({ attr: @attr })
///   <attr>_relation = <attr>_relation.not({ id: [id] }) if persisted?
///   <attr>_relation = <attr>_relation.where({ scope: @scope })   # each `scope:`
///   <attr>_relation = <attr>_relation.where(...)                 # `conditions:`
///   errors << "Attr has already been taken" if <attr>_relation.exists?
/// `case_sensitive: false` on a string column compares `LOWER(col) =
/// LOWER(?)`, as PostgreSQL's case-insensitive comparison does; a nil
/// value is `IS NULL` either way. A `belongs_to` names its foreign key.
fn inline_uniqueness_check(
    model: &Model,
    finder: &Finder,
    attr: &Symbol,
    attr_ty: Option<&Ty>,
    scope: &[Symbol],
    case_sensitive: bool,
    conditions: Option<&Expr>,
    text: &ErrorText<'_>,
) -> Vec<Expr> {
    let s = Span::synthetic();
    let sym = |n: &str| Expr::new(s, ExprNode::Lit { value: Literal::Sym { value: Symbol::from(n) } });
    let hash = |entries: Vec<(Expr, Expr)>| Expr::new(s, ExprNode::Hash { entries, kwargs: false });
    // A `belongs_to` reads as its foreign key column; a polymorphic one
    // as its type column too, as `where(assoc => record)` writes it.
    let columns = |name: &Symbol| -> Vec<Symbol> {
        for a in model.associations() {
            if let Association::BelongsTo { name: n, foreign_key, polymorphic, .. } = a {
                if n == name {
                    let mut cols = vec![foreign_key.clone()];
                    if *polymorphic {
                        cols.push(Symbol::from(format!("{}_type", name.as_str())));
                    }
                    return cols;
                }
            }
        }
        vec![name.clone()]
    };
    let rel = Symbol::from(format!("{}_relation", attr.as_str()));
    let model_class = match finder {
        Finder::RecordClass => send(Expr::new(s, ExprNode::SelfRef), "class", vec![]),
        Finder::Declaring { class, .. } => Expr::new(
            s,
            ExprNode::Const { path: class.0.as_str().split("::").map(Symbol::from).collect() },
        ),
    };
    let relation = Expr::new(
        s,
        ExprNode::Send {
            recv: Some(Expr::new(
                s,
                ExprNode::Const { path: vec![Symbol::from("ActiveRecord"), Symbol::from("Relation")] },
            )),
            method: Symbol::from("new"),
            args: vec![model_class],
            block: None,
            parenthesized: true,
        },
    );
    // An STI subclass's relation keeps its type condition: the rows of
    // the subclass and of every class below it.
    let base = match finder {
        Finder::Declaring { type_names, .. } if !type_names.is_empty() => {
            let names = type_names
                .iter()
                .map(|n| Expr::new(s, ExprNode::Lit { value: Literal::Str { value: n.clone() } }))
                .collect();
            let list = Expr::new(s, ExprNode::Array { elements: names, style: ArrayStyle::Brackets });
            send(relation, "where", vec![hash(vec![(sym("type"), list)])])
        }
        _ => relation,
    };
    // The attribute itself: a belongs_to binds its foreign key only.
    let own_col = columns(attr).into_iter().next().expect("one column");
    let own_value = ivar(&own_col);
    let (inner, _) = peel_nilable(attr_ty);
    let lower = !case_sensitive && own_col == *attr && matches!(inner, Some(Ty::Str));
    let eq = send(base.clone(), "where", vec![hash(vec![(sym(own_col.as_str()), own_value.clone())])]);
    let first = if lower {
        let sql = Expr::new(
            s,
            ExprNode::Lit { value: Literal::Str { value: format!("LOWER({0}) = LOWER(?)", own_col.as_str()) } },
        );
        Expr::new(
            s,
            ExprNode::If {
                cond: send(own_value.clone(), "nil?", vec![]),
                then_branch: eq,
                else_branch: send(base, "where", vec![sql, own_value.clone()]),
            },
        )
    } else {
        eq
    };
    let mut out = vec![assign_local(&rel, first)];
    let pk = model.primary_key.clone().unwrap_or_else(|| Symbol::from("id"));
    let id = Expr::new(
        s,
        ExprNode::Send { recv: None, method: Symbol::from("id"), args: Vec::new(), block: None, parenthesized: false },
    );
    let not_self = send(
        local_read(&rel),
        "not",
        vec![hash(vec![(
            sym(pk.as_str()),
            Expr::new(s, ExprNode::Array { elements: vec![id], style: ArrayStyle::Brackets }),
        )])],
    );
    let persisted = Expr::new(
        s,
        ExprNode::Send {
            recv: None,
            method: Symbol::from("persisted?"),
            args: Vec::new(),
            block: None,
            parenthesized: false,
        },
    );
    out.push(if_with_nil_else(persisted, assign_local(&rel, not_self)));
    let scope_entries: Vec<(Expr, Expr)> =
        scope.iter().flat_map(columns).map(|c| (sym(c.as_str()), ivar(&c))).collect();
    if !scope_entries.is_empty() {
        out.push(assign_local(&rel, send(local_read(&rel), "where", vec![hash(scope_entries)])));
    }
    if let Some(conditions) = conditions {
        if let Some(chain) = uniqueness_conditions_chain(conditions, &local_read(&rel)) {
            out.push(assign_local(&rel, chain));
        }
    }
    let taken = send(local_read(&rel), "exists?", vec![]);
    out.push(if_with_nil_else(taken, text.push("taken", None, || own_value.clone())));
    out
}

/// A `conditions:` lambda — a `where(...)` / `where.not(...)` chain,
/// which Rails `instance_exec`s on the relation — written onto `rel`.
/// The lambda's parameter, when it takes one, is the record. Ingest
/// admits no other shape.
fn uniqueness_conditions_chain(conditions: &Expr, rel: &Expr) -> Option<Expr> {
    let lambda = match &*conditions.node {
        ExprNode::Lambda { .. } => conditions,
        ExprNode::Send { block: Some(block), .. } => block,
        _ => return None,
    };
    let ExprNode::Lambda { params, body, .. } = &*lambda.node else { return None };
    let record = params.first().cloned();
    let body = match &*body.node {
        ExprNode::Seq { exprs } if exprs.len() == 1 => &exprs[0],
        _ => body,
    };
    fn args_on(args: &[Expr], record: &Option<Symbol>) -> Vec<Expr> {
        args.iter()
            .map(|a| {
                let mut a = a.clone();
                replace_var_with_self(&mut a, record);
                a
            })
            .collect()
    }
    fn chain(e: &Expr, rel: &Expr, record: &Option<Symbol>) -> Option<Expr> {
        let ExprNode::Send { recv, method, args, block: None, .. } = &*e.node else { return None };
        let on = |r: &Option<Expr>| match r {
            None => Some(rel.clone()),
            Some(r) => chain(r, rel, record),
        };
        match method.as_str() {
            "where" if !args.is_empty() => Some(send(on(recv)?, "where", args_on(args, record))),
            "not" if !args.is_empty() => {
                let inner = recv.as_ref()?;
                let ExprNode::Send { recv: root, method: w, args: none, block: None, .. } = &*inner.node else {
                    return None;
                };
                (w.as_str() == "where" && none.is_empty()).then_some(())?;
                Some(send(on(root)?, "not", args_on(args, record)))
            }
            _ => None,
        }
    }
    chain(body, rel, &record)
}

fn replace_var_with_self(e: &mut Expr, record: &Option<Symbol>) {
    if let (ExprNode::Var { name, .. }, Some(r)) = (&*e.node, record) {
        if name == r {
            *e.node = ExprNode::SelfRef;
            return;
        }
    }
    e.node.for_each_child_mut(&mut |c| replace_var_with_self(c, record));
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
/// ComparisonValidator: a nil or blank value is one `:blank` error;
/// otherwise each check, in the order written, compares the value with
/// its resolved bound (`value.public_send(op, bound)`) and adds the
/// option's error with the bound as `%{count}`. An ArgumentError from
/// the comparison (`comparison of Time with nil failed`) becomes the
/// error's text, as Rails rescues it per check.
fn inline_comparison_check(attr: &Symbol, value: &Expr, checks: &[NumericCheck], text: &ErrorText<'_>) -> Vec<Expr> {
    let s = Span::synthetic();
    let human = humanize(attr.as_str());
    let err = Symbol::from(format!("{}_comparison_error", attr.as_str()));
    let rescued = |body: Expr| {
        Expr::new(
            s,
            ExprNode::BeginRescue {
                body,
                rescues: vec![crate::expr::RescueClause {
                    classes: vec![Expr::new(s, ExprNode::Const { path: vec![Symbol::from("ArgumentError")] })],
                    binding: Some(err.clone()),
                    body: errors_push_expr(Expr::new(
                        s,
                        ExprNode::StringInterp {
                            parts: vec![
                                InterpPart::Text { value: format!("{human} ") },
                                InterpPart::Expr { expr: send(local_read(&err), "message", vec![]) },
                            ],
                        },
                    )),
                }],
                else_branch: None,
                ensure: None,
                implicit: false,
            },
        )
    };
    // The bound: a literal in place, anything else resolved into a local
    // the comparison and %{count} share.
    let bound = |check: &NumericCheck| -> (Vec<Expr>, Expr, Count) {
        match check.bound.as_ref() {
            Some(RuleValue::Literal { value }) => {
                (Vec::new(), Expr::new(s, ExprNode::Lit { value: value.clone() }), Count::Static(value.clone()))
            }
            Some(other) => {
                let local = Symbol::from(format!("{}_{}", attr.as_str(), check.op.key()));
                (vec![assign_local(&local, resolve(other))], local_read(&local), Count::Dynamic(local_read(&local)))
            }
            None => (Vec::new(), Expr::new(s, ExprNode::Lit { value: Literal::Nil }), Count::Static(Literal::Nil)),
        }
    };
    let Some(first) = checks.first() else { return Vec::new() };
    let blank = bool_op(
        BoolOpKind::Or,
        send(value.clone(), "nil?", vec![]),
        send(Expr::new(s, ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] }), "blank?", vec![value.clone()]),
    );
    let (mut pre, _, count) = bound(first);
    pre.push(text.push("blank", Some(&count), || value.clone()));
    let blank_branch = rescued(seq(pre));
    let mut each: Vec<Expr> = Vec::new();
    for check in checks {
        let op = match check.op {
            NumericOp::GreaterThan => ">",
            NumericOp::GreaterThanOrEqualTo => ">=",
            NumericOp::EqualTo => "==",
            NumericOp::LessThan => "<",
            NumericOp::LessThanOrEqualTo => "<=",
            NumericOp::OtherThan => "!=",
            NumericOp::Odd | NumericOp::Even | NumericOp::In => continue,
        };
        let (mut pre, bound_expr, count) = bound(check);
        let holds = send(value.clone(), op, vec![bound_expr]);
        pre.push(if_with_nil_else(not(holds), text.push(check.op.key(), Some(&count), || value.clone())));
        each.push(rescued(seq(pre)));
    }
    vec![Expr::new(s, ExprNode::If { cond: blank, then_branch: blank_branch, else_branch: seq(each) })]
}

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
                compares.push(if_with_nil_else(not(holds), text.push(check.op.key(), None, number)));
                continue;
            }
            // `value.in?(range)`: Range#include?, which for numbers is
            // `cover?`.
            NumericOp::In => "include?",
        };
        let Some(bound) = &check.bound else { continue };
        // A literal is written in place; anything else is resolved once,
        // into a local the comparison and the message's %{count} share.
        let (bound_expr, count) = match bound {
            RuleValue::Literal { value } => {
                (Expr::new(s, ExprNode::Lit { value: value.clone() }), Count::Static(value.clone()))
            }
            other => {
                let local = Symbol::from(format!("{}_{}", attr.as_str(), check.op.key()));
                compares.push(assign_local(&local, resolve(other)));
                (local_read(&local), Count::Dynamic(local_read(&local)))
            }
        };
        let holds = if check.op == NumericOp::In {
            send(bound_expr, op, vec![number()])
        } else {
            send(number(), op, vec![bound_expr])
        };
        compares.push(if_with_nil_else(not(holds), text.push(check.op.key(), Some(&count), number)));
    }
    let tail = if compares.is_empty() { nil() } else { seq(compares) };
    let tail = if only_integer {
        Expr::new(
            s,
            ExprNode::If {
                cond: not(send(regex(integer_re), "match?", vec![send(raw(), "to_s", vec![])])),
                then_branch: text.push("not_an_integer", None, raw),
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
            then_branch: text.push("not_a_number", None, raw),
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

/// What a message's `%{count}` reads: a bound known when lowering, or
/// one resolved on the run.
pub(super) enum Count {
    Static(Literal),
    Dynamic(Expr),
}

/// How a rule's errors read: ActiveModel's default text, or the
/// validator's `message:`. Either way the full message is the humanized
/// attribute, a space, and the message (`"%{attribute} %{message}"`).
#[derive(Clone, Copy)]
pub(super) struct ErrorText<'a> {
    attr: &'a Symbol,
    model_human: &'a str,
    i18n_keys: &'a [String],
    i18n_scope: &'a str,
    message: Option<&'a RuleMessage>,
}

impl ErrorText<'_> {
    /// `errors << "<Attr> <message>"` for an error of type `kind`
    /// (`blank`, `too_long`, ...), its `%{count}` `count`, and the value
    /// `%{value}` and a callable message see.
    fn push(&self, kind: &str, count: Option<&Count>, value: impl Fn() -> Expr) -> Expr {
        let human = humanize(self.attr.as_str());
        match self.message {
            None => self.push_type(kind, count, &value),
            Some(RuleMessage::Key { key }) => self.push_type(key.as_str(), count, &value),
            Some(RuleMessage::Text { text }) => errors_push_expr(self.interpolate(text, count, &value)),
            Some(RuleMessage::Value { expr }) => {
                // `m = <expr>`, then a Proc's call or the String with
                // the keys ActiveModel supplies interpolated.
                let s = Span::synthetic();
                let local = Symbol::from(format!("{}_message", self.attr.as_str()));
                let str_lit = |t: &str| Expr::new(s, ExprNode::Lit { value: Literal::Str { value: t.to_string() } });
                let sym = |n: &str| Expr::new(s, ExprNode::Lit { value: Literal::Sym { value: Symbol::from(n) } });
                let count_expr = match count {
                    Some(Count::Static(c)) => Some(Expr::new(s, ExprNode::Lit { value: c.clone() })),
                    Some(Count::Dynamic(e)) => Some(e.clone()),
                    None => None,
                };
                let mut entries = vec![
                    (sym("model"), str_lit(self.model_human)),
                    (sym("attribute"), str_lit(&human)),
                    (sym("value"), value()),
                ];
                if let Some(c) = &count_expr {
                    entries.push((sym("count"), c.clone()));
                }
                let call = Expr::new(
                    s,
                    ExprNode::Send {
                        recv: Some(local_read(&local)),
                        method: Symbol::from("call"),
                        args: vec![Expr::new(s, ExprNode::SelfRef), Expr::new(s, ExprNode::Hash { entries, kwargs: true })],
                        block: None,
                        parenthesized: true,
                    },
                );
                let mut text = send(local_read(&local), "to_s", vec![]);
                let mut keys = vec![("%{attribute}", str_lit(&human)), ("%{model}", str_lit(self.model_human))];
                keys.push(("%{value}", send(value(), "to_s", vec![])));
                if let Some(c) = count_expr {
                    keys.push(("%{count}", send(c, "to_s", vec![])));
                }
                for (key, with) in keys {
                    text = send(text, "gsub", vec![str_lit(key), with]);
                }
                let callable = send(local_read(&local), "respond_to?", vec![sym("call")]);
                let message = Expr::new(s, ExprNode::If { cond: callable, then_branch: call, else_branch: text });
                seq(vec![
                    assign_local(&local, expr.clone()),
                    errors_push_expr(Expr::new(
                        s,
                        ExprNode::StringInterp {
                            parts: vec![InterpPart::Text { value: format!("{human} ") }, InterpPart::Expr { expr: message }],
                        },
                    )),
                ])
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
                match count {
                    Some(Count::Static(c)) => entries.push((sym("count"), Expr::new(s, ExprNode::Lit { value: c.clone() }))),
                    Some(Count::Dynamic(e)) => entries.push((sym("count"), e.clone())),
                    None => {}
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

    /// The English message for error type `kind`, pluralized by `count`
    /// (`one` for 1), or — for a type nothing translates — the text I18n
    /// answers for a missing translation, naming the keys it tried.
    fn push_type(&self, kind: &str, count: Option<&Count>, value: &dyn Fn() -> Expr) -> Expr {
        let Some((one, other)) = crate::dialect::active_model_en_message(kind) else {
            let attr = self.attr.as_str();
            let scope = self.i18n_scope;
            let mut missing = String::from("Translation missing. Options considered were:");
            for model in self.i18n_keys {
                missing.push_str(&format!(
                    "\n- en.{scope}.errors.models.{model}.attributes.{attr}.{kind}\
                     \n- en.{scope}.errors.models.{model}.{kind}"
                ));
            }
            missing.push_str(&format!(
                "\n- en.{scope}.errors.messages.{kind}\
                 \n- en.errors.attributes.{attr}.{kind}\
                 \n- en.errors.messages.{kind}"
            ));
            return errors_push(format!("{} {missing}", humanize(attr)));
        };
        match (one, count) {
            (Some(one), Some(Count::Static(c))) => {
                let is_one = matches!(c, Literal::Int { value: 1 }) || matches!(c, Literal::Float { value } if *value == 1.0);
                errors_push_expr(self.interpolate(if is_one { one } else { other }, count, value))
            }
            (Some(one), Some(Count::Dynamic(e))) => {
                let s = Span::synthetic();
                let is_one = send(e.clone(), "==", vec![Expr::new(s, ExprNode::Lit { value: Literal::Int { value: 1 } })]);
                errors_push_expr(Expr::new(
                    s,
                    ExprNode::If {
                        cond: is_one,
                        then_branch: self.interpolate(one, count, value),
                        else_branch: self.interpolate(other, count, value),
                    },
                ))
            }
            _ => errors_push_expr(self.interpolate(other, count, value)),
        }
    }

    /// `"<Attr> <template>"` with I18n's interpolation of the keys
    /// ActiveModel supplies (ingest admits no others).
    fn interpolate(&self, template: &str, count: Option<&Count>, value: &dyn Fn() -> Expr) -> Expr {
        let human = humanize(self.attr.as_str());
        let mut parts: Vec<InterpPart> = vec![InterpPart::Text { value: format!("{human} ") }];
        let push_text = |parts: &mut Vec<InterpPart>, t: &str| match parts.last_mut() {
            Some(InterpPart::Text { value }) => value.push_str(t),
            _ => parts.push(InterpPart::Text { value: t.to_string() }),
        };
        let mut rest = template;
        while let Some(start) = rest.find("%{") {
            let tail = &rest[start + 2..];
            let Some(end) = tail.find('}') else { break };
            push_text(&mut parts, &rest[..start]);
            match (&tail[..end], count) {
                ("attribute", _) => push_text(&mut parts, &human),
                ("model", _) => push_text(&mut parts, self.model_human),
                ("count", Some(Count::Static(c))) => push_text(&mut parts, &format_count(c)),
                ("count", Some(Count::Dynamic(e))) => parts.push(InterpPart::Expr { expr: e.clone() }),
                ("value", _) => parts.push(InterpPart::Expr { expr: value() }),
                (other, _) => push_text(&mut parts, &format!("%{{{other}}}")),
            }
            rest = &tail[end + 1..];
        }
        push_text(&mut parts, rest);
        match parts.as_slice() {
            [InterpPart::Text { value }] => Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Str { value: value.clone() } }),
            _ => Expr::new(Span::synthetic(), ExprNode::StringInterp { parts }),
        }
    }
}

/// A value ActiveModel resolves per run (`resolve_value`), and a
/// callback condition's call: a literal or expression as written, a
/// method on the record, a lambda called with the record or with
/// nothing. A lambda written inline runs with the record as `self`,
/// which is what a condition's `instance_exec` sees; ingest admits a
/// resolved lambda only when it does not read `self`.
fn resolve(value: &RuleValue) -> Expr {
    let s = Span::synthetic();
    match value {
        RuleValue::Literal { value } => Expr::new(s, ExprNode::Lit { value: value.clone() }),
        RuleValue::Expr { expr } => expr.clone(),
        RuleValue::Method { name } => Expr::new(
            s,
            ExprNode::Send { recv: None, method: name.clone(), args: Vec::new(), block: None, parenthesized: false },
        ),
        RuleValue::Call { callable, with_record } => Expr::new(
            s,
            ExprNode::Send {
                recv: Some(callable.clone()),
                method: Symbol::from("call"),
                args: if *with_record { vec![Expr::new(s, ExprNode::SelfRef)] } else { Vec::new() },
                block: None,
                parenthesized: true,
            },
        ),
    }
}

fn assign_local(name: &Symbol, value: Expr) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Assign { target: crate::expr::LValue::Var { id: crate::ident::VarId(0), name: name.clone() }, value },
    )
}

fn local_read(name: &Symbol) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Var { id: crate::ident::VarId(0), name: name.clone() })
}

/// A rule's checks under its `allow_nil:` / `allow_blank:` (EachValidator
/// skips such a value), its `on:` contexts, and its `if:` / `unless:`
/// conditions (every `if` holds and no `unless` does, as callback
/// conditions combine).
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
    let mut holds: Vec<Expr> = Vec::new();
    if !o.on.is_empty() {
        holds.push(on_context_check(&o.on));
    }
    holds.extend(o.if_conds.iter().map(resolve));
    // `except_on:` is the first `unless:` condition Rails adds.
    if !o.except_on.is_empty() {
        holds.push(not(on_context_check(&o.except_on)));
    }
    holds.extend(o.unless_conds.iter().map(|c| not(resolve(c))));
    if let Some(cond) = holds.into_iter().reduce(|a, b| bool_op(BoolOpKind::And, a, b)) {
        body = if_with_nil_else(cond, body);
    }
    vec![body]
}

/// `on: [:create, ...]`: Rails' `!(on & Array(validation_context)).empty?`.
fn on_context_check(on: &[Symbol]) -> Expr {
    let s = Span::synthetic();
    let contexts = Expr::new(
        s,
        ExprNode::Array {
            elements: on.iter().map(|c| Expr::new(s, ExprNode::Lit { value: Literal::Sym { value: c.clone() } })).collect(),
            style: ArrayStyle::Brackets,
        },
    );
    let current = Expr::new(
        s,
        ExprNode::Send {
            recv: None,
            method: Symbol::from("Array"),
            args: vec![Expr::new(
                s,
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from("validation_context"),
                    args: Vec::new(),
                    block: None,
                    parenthesized: false,
                },
            )],
            block: None,
            parenthesized: true,
        },
    );
    send(contexts, "intersect?", vec![current])
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
        let keys: &'static [String] = Box::leak(vec![String::from("post")].into_boxed_slice());
        ErrorText { attr, model_human: "Post", i18n_keys: keys, i18n_scope: "activerecord", message: None }
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
        let text = ErrorText { attr: &a, model_human: "Post", i18n_keys: &[String::from("post")], i18n_scope: "activerecord", message: Some(&message) };
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
        let text = ErrorText { attr: &a, model_human: "Post", i18n_keys: &[String::from("post")], i18n_scope: "activerecord", message: Some(&message) };
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
            check(NumericOp::GreaterThan, Some(crate::dialect::RuleValue::Literal { value: Literal::Int { value: 0 } })),
            check(NumericOp::LessThanOrEqualTo, Some(crate::dialect::RuleValue::Literal { value: Literal::Int { value: 100 } })),
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
        let checks = [NumericCheck { op: NumericOp::GreaterThan, bound: Some(RuleValue::Literal { value: Literal::Float { value: 0.5 } }) }];
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
