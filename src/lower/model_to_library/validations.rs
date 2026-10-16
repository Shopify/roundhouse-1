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

use super::{fn_sig, seq, ErrorsReaders};

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
    Declaring { class: ClassId, type_names: Vec<String>, type_column: String },
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
                Finder::Declaring { class: model.name.clone(), type_names: Vec::new(), type_column: String::new() }
            },
        }
    }
}

/// `details`: record each error's `ErrorDetail` in an `error_details`
/// list beside `errors` (every target but the CRuby one — see
/// [`push_error_details`]).
pub(super) fn push_validate_method(
    methods: &mut Vec<MethodDef>,
    model: &Model,
    models: &[Model],
    errors_readers: ErrorsReaders,
    details: bool,
) {
    let declarer = Declarer::model(model);
    let mut stmts = validation_statements(model, model.spanned_validations(), &declarer);

    // Rails 5+ default: every `belongs_to` requires the associated
    // record to exist before save. Emit `validates_belongs_to(:assoc,
    // @<fk>, <Target>)` per non-optional belongs_to. The runtime
    // helper short-circuits when the FK is unset (nil/0) and queries
    // `<Target>.exists?(fk_value)` otherwise (by the association's
    // `primary_key:` when it names a column other than the target's key).
    for (span, assoc) in model.spanned_associations() {
        if let Association::BelongsTo {
            name, target, foreign_key, optional: false, polymorphic, primary_key, ..
        } = assoc
        {
            let lookup_pk = belongs_to_lookup_pk(models, target, primary_key.as_ref());
            let mut check = inline_belongs_to_check(
                Messages(Some(model)),
                name,
                foreign_key,
                target,
                lookup_pk.as_ref(),
                *polymorphic,
            );
            check.inherit_span(span);
            // Rails adds `:required` to the association's name:
            // `validates_presence_of name, message: :required`, an Error
            // of type :blank.
            tag_error_detail(&mut check, name, &typed_detail("blank", vec![]));
            // Rails runs it where `belongs_to` declared it, among the
            // `validates` / `validate` lines (errors and full messages
            // follow that order): before the first check written after
            // it, while the checks stand in source order (a `prepend:`
            // one does not).
            let in_order = stmts.windows(2).all(|w| w[0].span.file != w[1].span.file || w[0].span.start <= w[1].span.start);
            let at = in_order
                .then(|| stmts.iter().position(|c| !c.span.is_synthetic() && c.span.file == span.file && c.span.start > span.start))
                .flatten();
            match at {
                Some(at) if !span.is_synthetic() => stmts.insert(at, check),
                _ => stmts.push(check),
            }
        }
    }

    stmts.extend(secure_password_checks(model));
    if details {
        stmts.iter_mut().for_each(|s| push_error_details(s, &model.i18n));
    }

    if stmts.is_empty() {
        // A tableless model (it includes ActiveModel::Validations or
        // ::Model) answers `valid?` / `errors` with no rules of its own,
        // and its subclasses' `validate` extends this one. A source
        // `def validate` is not shadowed by an empty synthesized one
        // (`push_user_methods` keeps Method synthesizers over source
        // defs), but an ActiveModel class that writes it still gets the
        // surface below.
        let defines_validate = model.body.iter().any(|item| matches!(
            item,
            crate::dialect::ModelBodyItem::Method { method, .. }
                if method.name.as_str() == "validate" && method.receiver == MethodReceiver::Instance
        ));
        let active_model = includes_active_model_validations(model);
        if model.parent.is_none() && (!defines_validate || active_model) {
            if !defines_validate {
                let nil = Expr::new(model.span, ExprNode::Lit { value: Literal::Nil });
                methods.push(validate_method_def(&model.name, vec![nil]));
            }
            push_active_model_validation_surface_for(methods, model, errors_readers, details);
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
        push_active_model_validation_surface_for(methods, model, errors_readers, details);
    }
}

/// ActiveModel's public `validate(context = nil)` is `valid?(context)`:
/// the synthesized `validate` here runs the checks, which `valid?` calls
/// with no argument, so a call that names a context (Core's
/// `validator.validate(record.validation_context)`, `nil` included) goes
/// to `valid?` and answers its Boolean. The checks stay the no-argument
/// form subclasses extend through `super`.
fn take_explicit_validation_context(methods: &mut [MethodDef], model: &Model, span: Span) {
    let Some(validate) = methods.iter_mut().find(|m| {
        m.receiver == MethodReceiver::Instance
            && m.name.as_str() == "validate"
            && m.params.is_empty()
            && m.name_span.is_synthetic()
    }) else {
        return;
    };
    let context = Symbol::from("context");
    let empty = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(local_read(&context)),
            method: Symbol::from("empty?"),
            args: Vec::new(),
            block: None,
            parenthesized: false,
        },
    );
    let contexts = uses_validation_context(model);
    let forward = Expr::new(
        span,
        ExprNode::Send {
            recv: None,
            method: Symbol::from("valid?"),
            args: if contexts {
                vec![Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(local_read(&context)),
                        method: Symbol::from("first"),
                        args: Vec::new(),
                        block: None,
                        parenthesized: false,
                    },
                )]
            } else {
                Vec::new()
            },
            block: None,
            parenthesized: contexts,
        },
    );
    let checks = validate.body.clone();
    validate.body = Expr::new(span, ExprNode::If { cond: empty, then_branch: checks, else_branch: forward });
    validate.params = vec![crate::dialect::Param {
        name: context.clone(),
        default: None,
        keyword: false,
        rest: true,
        forwarding: false,
        from_keyword: false,
        from_kwrest: false,
    }];
    validate.signature = Some(Ty::Fn {
        params: vec![crate::ty::Param {
            name: context,
            ty: Ty::Array { elem: Ty::Untyped.into() }.into(),
            kind: crate::ty::ParamKind::Rest,
        }]
        .into(),
        block: None,
        ret: std::sync::Arc::new(Ty::Untyped),
        effects: crate::effect::EffectSet::pure(),
    });
}

/// The column a required `belongs_to`'s existence check asks the target
/// by: the declaration's `primary_key:` when it is not the target's own
/// key (`primary_key: "uuid"`), else `None` (the plain `exists?(@fk)`).
fn belongs_to_lookup_pk(models: &[Model], target: &ClassId, declared: Option<&Symbol>) -> Option<Symbol> {
    // A target keyed on a column list (`self.primary_key = [..]`) answers
    // `exists?` for a key tuple, so a declared `primary_key: :id` is a
    // column to ask by, whatever the target's `primary_key` symbol reads.
    if let Some(declared) = declared {
        let mut at = models.iter().find(|m| &m.name == target);
        let mut hops = 0;
        while let Some(m) = at.filter(|_| hops < 32) {
            if !m.composite_primary_key.is_empty() {
                return Some(declared.clone());
            }
            if m.primary_key.is_some() {
                break;
            }
            hops += 1;
            at = m.parent.as_ref().and_then(|p| models.iter().find(|x| &x.name == p));
        }
    }
    let assoc_pk = crate::lower::scope_chain::association_primary_key(models, target, declared);
    let own_pk = crate::lower::scope_chain::association_primary_key(models, target, None);
    (assoc_pk != own_pk).then_some(assoc_pk)
}

/// For a tree that keeps each error's `ErrorDetail` in an `error_details`
/// list beside `errors` (`lower::errors_add::attributed_push`) rather
/// than in the runtime's `ErrorAttributes` (the CRuby tree): turn each
/// tagged `errors << "<Attr> msg"` into that push and the detail's. The
/// detail records the type and the short message, so a push whose type
/// or short message the lowering did not record is left as it was.
pub(crate) fn push_error_details(e: &mut Expr, i18n: &crate::i18n::ModelI18n) {
    // An `errors.add` the errors-add pass lowered already pushes its detail
    // (its message push keeps the hint), so a second pass leaves it be.
    if is_attributed_pair(e) {
        return;
    }
    e.node.for_each_child_mut(&mut |c| push_error_details(c, i18n));
    let Some(crate::expr::IrHint::ErrorAttribute(tag)) = &e.hint else { return };
    if !is_errors_push(e) {
        return;
    }
    let ExprNode::Send { args, .. } = &*e.node else { return };
    // The text the model's `errors.format` puts around the message, which
    // the short message is left without.
    let (before, after) = affixes(i18n, tag.attribute.as_str());
    let Some(bare) = without_affixes(&args[0], &before, &after) else { return };
    let span = e.span;
    // Rails' type for a String message is the message itself.
    let kind = match &tag.detail {
        crate::expr::ErrorDetail::Type { name, .. } => {
            Expr::new(span, ExprNode::Lit { value: Literal::Str { value: name.as_str().to_string() } })
        }
        crate::expr::ErrorDetail::Message => bare.clone(),
        crate::expr::ErrorDetail::Unknown => return,
    };
    let attribute = tag.attribute.clone();
    // The push keeps its hint: the CRuby and JRuby trees record it too.
    let push = e.clone();
    *e = crate::lower::errors_add::attributed_push(push, None, attribute.as_str(), kind, bare, span);
}

/// `errors << msg; error_details << ErrorDetail.new(..)`, the pair
/// `lower::errors_add::attributed_push` builds.
fn is_attributed_pair(e: &Expr) -> bool {
    let ExprNode::Seq { exprs } = &*e.node else { return false };
    let [push, detail] = exprs.as_slice() else { return false };
    let ExprNode::Send { recv: Some(reader), method, .. } = &*detail.node else { return false };
    let ExprNode::Send { method: reader_method, args, .. } = &*reader.node else { return false };
    is_errors_push(push) && method.as_str() == "<<" && reader_method.as_str() == "error_details" && args.is_empty()
}

/// The text `errors.format` puts before and after the message of an error
/// on `attribute` (`"Title "` and `""` for Rails' `%{attribute} %{message}`,
/// `""` and `" (Title)"` for `%{message} (%{attribute})`); the message
/// alone on `:base`.
fn affixes(i18n: &crate::i18n::ModelI18n, attribute: &str) -> (String, String) {
    if attribute == "base" {
        return (String::new(), String::new());
    }
    let format = i18n.errors_format().replace("%{attribute}", &i18n.human_attribute_name(attribute));
    match format.split_once("%{message}") {
        Some((before, after)) if !after.contains("%{message}") => (before.to_string(), after.to_string()),
        _ => (String::new(), String::new()),
    }
}

/// `message` without the text `errors.format` put around it (`before`
/// and `after`), as the short message Rails' Error keeps. `None` where
/// the message does not show them as written.
fn without_affixes(message: &Expr, before: &str, after: &str) -> Option<Expr> {
    if before.is_empty() && after.is_empty() {
        return Some(message.clone());
    }
    let node = match &*message.node {
        ExprNode::Lit { value: Literal::Str { value } } => ExprNode::Lit {
            value: Literal::Str { value: value.strip_prefix(before)?.strip_suffix(after)?.to_string() },
        },
        ExprNode::StringInterp { parts } => {
            let mut parts = parts.clone();
            if !before.is_empty() {
                let Some(InterpPart::Text { value }) = parts.first() else { return None };
                let rest = value.strip_prefix(before)?.to_string();
                parts[0] = InterpPart::Text { value: rest };
            }
            if !after.is_empty() {
                let Some(InterpPart::Text { value }) = parts.last() else { return None };
                let rest = value.strip_suffix(after)?.to_string();
                let last = parts.len() - 1;
                parts[last] = InterpPart::Text { value: rest };
            }
            parts.retain(|p| !matches!(p, InterpPart::Text { value } if value.is_empty()));
            ExprNode::StringInterp { parts }
        }
        ExprNode::If { cond, then_branch, else_branch } => ExprNode::If {
            cond: cond.clone(),
            then_branch: without_affixes(then_branch, before, after)?,
            else_branch: without_affixes(else_branch, before, after)?,
        },
        // The dedupe's `_association_error(message)` answers a copy of its
        // argument, which the detail keeps as the short message.
        ExprNode::Send { recv: None, method, args, block: None, .. }
            if method.as_str() == "_association_error" && args.len() == 1 =>
        {
            return without_affixes(&args[0], before, after);
        }
        _ => return None,
    };
    Some(Expr::new(message.span, node))
}

/// `def validate; <stmts>; end` on `owner`.
/// A model subclass's own required `belongs_to` checks
/// (`lower::subclass_macros`) in its `validate`: after the inherited
/// ones (`super`), among the subclass's other checks in declaration
/// order, as Rails runs the presence validation `belongs_to` declares.
pub(crate) fn push_subclass_belongs_to_checks(methods: &mut Vec<MethodDef>, view: &Model, models: &[Model]) {
    let mut checks = Vec::new();
    for (span, assoc) in view.spanned_associations() {
        if let Association::BelongsTo { name, target, foreign_key, optional: false, polymorphic, primary_key, .. } = assoc {
            let lookup_pk = belongs_to_lookup_pk(models, target, primary_key.as_ref());
            let mut check =
                inline_belongs_to_check(Messages(Some(view)), name, foreign_key, target, lookup_pk.as_ref(), *polymorphic);
            check.inherit_span(span);
            tag_error_detail(&mut check, name, &typed_detail("blank", vec![]));
            checks.push(check);
        }
    }
    if checks.is_empty() {
        return;
    }
    let existing =
        methods.iter_mut().find(|m| m.name.as_str() == "validate" && m.receiver == MethodReceiver::Instance);
    let Some(existing) = existing else {
        let mut stmts = vec![Expr::new(Span::synthetic(), ExprNode::Super { args: Some(Vec::new()), block: None })];
        stmts.extend(checks);
        methods.push(validate_method_def(&view.name, stmts));
        return;
    };
    let mut stmts = match &*existing.body.node {
        ExprNode::Seq { exprs } => exprs.clone(),
        _ => vec![existing.body.clone()],
    };
    let after_super = stmts.iter().position(|s| matches!(&*s.node, ExprNode::Super { .. })).map_or(0, |i| i + 1);
    for check in checks {
        let at = stmts[after_super..]
            .iter()
            .position(|s| !s.span.is_synthetic() && s.span.file == check.span.file && s.span.start > check.span.start)
            .map_or(stmts.len(), |i| after_super + i);
        stmts.insert(at, check);
    }
    existing.body = seq(stmts);
}

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
            // A value read with the class as `self` reads the declaring
            // class's.
            let mut bound = rule.clone();
            bind_class_scope(&mut bound, &declarer.name);
            // `allow_nil:` / `allow_blank:` / `message:` / `if:` /
            // `unless:` wrap the rule they apply to.
            let (rule, options) = match &bound {
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
                i18n: Some(&model.i18n).filter(|i| !i.is_default()),
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
                    text.push("blank", None, || reader.clone(), Detail::Bare),
                );
                (vec![check], reader)
            } else {
                // `validates_associated` / `validates_each` read the value
                // as `read_attribute_for_validation` does, through the reader,
                // and so does every rule on a name no slot holds.
                let read_by_send = any_assoc
                    || read_through_method(model, &v.attribute, attr_ty)
                    || (!matches!(rule, ValidationRule::Numericality { .. }) && overrides_reader(model, &v.attribute))
                    || matches!(rule, ValidationRule::Associated | ValidationRule::Each { .. });
                let value = if read_by_send {
                    reader
                } else if matches!(rule, ValidationRule::Numericality { .. }) {
                    // Numericality checks the raw value, not a reader an
                    // included module redefines (`validation_readers`).
                    let mut slot = ivar(&v.attribute);
                    slot.decisions |= crate::expr::RAW_ATTRIBUTE_READ;
                    slot
                } else {
                    ivar(&v.attribute)
                };
                (validation_rule_to_calls(model, &declarer.finder, &v.attribute, rule, attr_ty, &value, &text), value)
            };
            let prepend = options.is_some_and(|o| o.prepend);
            let strict = options.and_then(|o| o.strict.as_ref());
            let checks = match options {
                Some(o) => guard_rule(value, checks, o),
                None => checks,
            };
            let mut checks: Vec<Expr> = checks
                .into_iter()
                .map(|mut check| {
                    // Each expanded check attributes to its `validates` line,
                    // and each message it adds to the validated attribute;
                    // a strict one raises the message instead.
                    check.inherit_span(span);
                    match strict {
                        Some(exception) => raise_instead_of_adding(&mut check, exception),
                        None => tag_error_attribute(&mut check, &v.attribute),
                    }
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
        let human = model.human_attribute_name(attr.as_str());
        let msgs = Messages(Some(model));
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
        out.push(if_with_nil_else(blank(digest), errors_push_expr(msgs.error(attr.as_str(), "blank", &Default::default()))));
        out.push(if_with_nil_else(
            bool_op(
                BoolOpKind::And,
                present(plain.clone()),
                send(send(plain.clone(), "bytesize", vec![]), ">", vec![Expr::new(
                    Span::synthetic(),
                    ExprNode::Lit { value: Literal::Int { value: 72 } },
                )]),
            ),
            errors_push_expr(msgs.error(attr.as_str(), "password_too_long", &Default::default())),
        ));
        out.push(if_with_nil_else(
            bool_op(
                BoolOpKind::And,
                bool_op(BoolOpKind::And, present(plain.clone()), not(send(confirmation.clone(), "nil?", vec![]))),
                send(confirmation, "!=", vec![plain]),
            ),
            errors_push_expr(msgs.error(
                &format!("{}_confirmation", attr.as_str()),
                "confirmation",
                &ErrorOpts { attribute: Some(human.clone()), ..Default::default() },
            )),
        ));
        // Rails: `errors.add(attribute, :blank)`, BCrypt's
        // `errors.add(attribute, :password_too_long)` and
        // `validates_confirmation_of attribute, allow_nil: true`.
        tag_error_detail(&mut out[start], &attr, &typed_detail("blank", vec![]));
        tag_error_detail(&mut out[start + 1], &attr, &typed_detail("password_too_long", vec![]));
        let human_lit = Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Str { value: human.clone() } });
        tag_error_detail(&mut out[start + 2], &confirmation_attr, &typed_detail("confirmation", vec![("attribute", human_lit)]));
    }
    out
}

/// `errors << msg` as ActiveModel's `errors.add(..., strict: exception)`
/// runs it: `raise exception, error.full_message`, the error not added.
/// The accumulator's messages are full messages already.
fn raise_instead_of_adding(check: &mut Expr, exception: &Expr) {
    if is_errors_push(check) {
        let ExprNode::Send { args, .. } = &*check.node else { unreachable!() };
        let message = args[0].clone();
        check.hint = None;
        *check.node = ExprNode::Send {
            recv: None,
            method: Symbol::from("raise"),
            args: vec![exception.clone(), message],
            block: None,
            parenthesized: false,
        };
        return;
    }
    check.node.for_each_child_mut(&mut |c| raise_instead_of_adding(c, exception));
}

/// `errors << msg`, the accumulator push every check lowers its error to.
fn is_errors_push(check: &Expr) -> bool {
    matches!(
        &*check.node,
        ExprNode::Send { recv: Some(r), method, args, .. }
            if method.as_str() == "<<" && args.len() == 1
                && matches!(&*r.node, ExprNode::Send { recv: None, method: m, args: a, .. } if m.as_str() == "errors" && a.is_empty())
    )
}

/// Bind every [`RuleValue::ClassScoped`] in `rule` to `class`, the
/// class whose body declared it: `self` reads the class, and a method
/// call without a receiver is the class's (`-> { minimum }` →
/// `-> { Post.minimum }`). Ingest admits such a value only when it reads
/// no instance variable of the class.
pub(crate) fn bind_class_scope(rule: &mut ValidationRule, class: &ClassId) {
    fn bind(value: &mut RuleValue, class: &ClassId) {
        let RuleValue::ClassScoped { value: inner } = value else { return };
        let mut inner = (**inner).clone();
        match &mut inner {
            RuleValue::Expr { expr } => class_self(expr, class),
            RuleValue::Call { callable, .. } => class_self(callable, class),
            _ => {}
        }
        *value = inner;
    }
    fn class_self(e: &mut Expr, class: &ClassId) {
        let konst = || {
            Expr::new(Span::synthetic(), ExprNode::Const { path: class.0.as_str().split("::").map(Symbol::from).collect() })
        };
        match &mut *e.node {
            ExprNode::SelfRef => *e = konst(),
            ExprNode::Send { recv: recv @ None, method, .. }
                if !crate::ingest::validates::SELFLESS_KERNEL.contains(&method.as_str()) =>
            {
                *recv = Some(konst())
            }
            _ => {}
        }
        e.node.for_each_child_mut(&mut |c| class_self(c, class));
    }
    match rule {
        ValidationRule::Guarded { rule, .. } => bind_class_scope(rule, class),
        ValidationRule::Inclusion { list_of: Some(v), .. } => bind(v, class),
        ValidationRule::Numericality { checks, .. } | ValidationRule::Comparison { checks } => {
            for check in checks {
                if let Some(bound) = &mut check.bound {
                    bind(bound, class);
                }
            }
        }
        ValidationRule::LengthOf { checks, .. } => {
            for check in checks {
                bind(&mut check.bound, class);
            }
        }
        ValidationRule::FormatOf { regexp, .. } => bind(regexp, class),
        _ => {}
    }
}

/// Tag each `errors << msg` push in `check` that carries no attribute yet
/// with the one it is for (`IrHint::ErrorAttribute`): the accumulator
/// keeps only the message, and `errors.attribute_names`, `to_hash`,
/// `as_json` and `messages` need the attribute (and the short message,
/// `error_tag`) on the trees that record them.
pub(crate) fn tag_error_attribute(check: &mut Expr, attr: &Symbol) {
    tag_error_detail(check, attr, &crate::expr::ErrorDetail::Unknown);
}

/// `tag_error_attribute` for the association's own `:invalid` error; on a
/// tree that keeps `error_details` (`details`) it records the type and
/// pushes the `ErrorDetail` (see [`push_error_details`]).
pub(crate) fn tag_association_invalid(check: &mut Expr, attr: &Symbol, i18n: &crate::i18n::ModelI18n) {
    tag_error_detail(check, attr, &typed_detail("invalid", vec![]));
    push_error_details(check, i18n);
}

/// `tag_error_attribute`, with the type and options Rails' Error keeps
/// for each push (`errors.details`).
fn tag_error_detail(check: &mut Expr, attr: &Symbol, detail: &crate::expr::ErrorDetail) {
    if is_errors_push(check) && check.hint.is_none() {
        let ExprNode::Send { args, .. } = &*check.node else { unreachable!() };
        check.hint = Some(crate::expr::IrHint::ErrorAttribute(error_tag(attr, &args[0], detail.clone())));
    }
    check.node.for_each_child_mut(&mut |c| tag_error_detail(c, attr, detail));
}

/// An error type with options known as written (`ErrorDetail::Type`).
fn typed_detail(name: &str, options: Vec<(&str, Expr)>) -> crate::expr::ErrorDetail {
    crate::expr::ErrorDetail::Type {
        name: Symbol::from(name),
        options: Some(
            options.into_iter().map(|(key, value)| crate::expr::DetailOption { key: Symbol::from(key), value }).collect(),
        ),
    }
}

/// `message` pushed for `attr`, with where its short message starts:
/// Rails' full message is `"%{attribute} %{message}"` (the bare message
/// on `:base`), and every check here and in `lower::errors_add` builds it
/// that way, from `humanize`. The prefix is confirmed on the message's
/// own shape (`after_prefix`); a message that does not show it is
/// `ErrorShort::Unknown`, which the short-message reads refuse rather
/// than key a wrong text.
pub(crate) fn error_tag(attr: &Symbol, message: &Expr, detail: crate::expr::ErrorDetail) -> crate::expr::ErrorTag {
    let prefix = if attr.as_str() == "base" { String::new() } else { format!("{} ", humanize(attr.as_str())) };
    let short = if after_prefix(message, &prefix) {
        crate::expr::ErrorShort::After(prefix)
    } else {
        crate::expr::ErrorShort::Unknown
    };
    crate::expr::ErrorTag { attribute: attr.clone(), short, detail }
}

/// Whether every value `message` can take starts with `prefix`, as
/// written: a literal, an interpolation whose first text does, both
/// branches of a choice, a sequence's last value, or the dedupe's
/// `_association_error(message)` (`emit::ruby::collection_autosave`,
/// which answers a copy of its argument).
fn after_prefix(message: &Expr, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    match &*message.node {
        ExprNode::Lit { value: Literal::Str { value } } => value.starts_with(prefix),
        ExprNode::StringInterp { parts } => matches!(parts.first(), Some(InterpPart::Text { value }) if value.starts_with(prefix)),
        ExprNode::If { then_branch, else_branch, .. } => after_prefix(then_branch, prefix) && after_prefix(else_branch, prefix),
        ExprNode::Seq { exprs } => exprs.last().is_some_and(|last| after_prefix(last, prefix)),
        ExprNode::Send { recv: None, method, args, block: None, .. } if method.as_str() == "_association_error" && args.len() == 1 => {
            after_prefix(&args[0], prefix)
        }
        _ => false,
    }
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

/// Bare `include ActiveModel::<leaf>` in the class body for any of `modules`.
fn includes_active_model(model: &Model, modules: &[&str]) -> bool {
    model.body.iter().any(|item| {
        let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item else { return false };
        let ExprNode::Send { recv: None, method, args, .. } = &*expr.node else {
            return false;
        };
        method.as_str() == "include"
            && args.iter().any(|a| matches!(&*a.node,
                ExprNode::Const { path } if path.len() == 2
                    && path[0].as_str() == "ActiveModel"
                    && modules.contains(&path[1].as_str())))
    })
}

/// `include ActiveModel::Validations` or `include ActiveModel::Model`
/// (which includes it) in the class body.
fn includes_active_model_validations(model: &Model) -> bool {
    includes_active_model(model, &["Validations", "Model"])
}

/// `include ActiveModel::Model` in the class body. The narrower
/// `ActiveModel::Validations` brings `valid?`/`errors` and no
/// constructor, so the two are not interchangeable here.
fn includes_active_model_model(model: &Model) -> bool {
    includes_active_model(model, &["Model"])
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
    push_active_model_validation_surface_for(methods, model, ErrorsReaders::CreatedOnFirstRead, true)
}

/// `details`: the tree records each error's `ErrorDetail` in an
/// `error_details` list beside `errors` (every target but the CRuby
/// one, which keeps them in the runtime's `ErrorAttributes`).
fn push_active_model_validation_surface_for(
    methods: &mut Vec<MethodDef>,
    model: &Model,
    errors_readers: ErrorsReaders,
    details: bool,
) {
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
        let reset = |name: &str| {
            Expr::new(
                span,
                ExprNode::Assign {
                    target: crate::expr::LValue::Ivar { name: Symbol::from(name) },
                    value: Expr::new(
                        span,
                        ExprNode::Array { elements: vec![], style: ArrayStyle::default() },
                    ),
                },
            )
        };
        body_stmts.push(reset("errors"));
        if details {
            body_stmts.push(reset("error_details"));
        }
        body_stmts.extend(vec![
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
                ty: Ty::Union { variants: vec![Ty::Sym, Ty::Nil].into() }.into(),
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
    // ActiveModel's `invalid?(context = nil)`: `!valid?(context)`, with
    // the context parameter exactly when this `valid?` takes one.
    if !defines("invalid?") {
        let contexts = uses_validation_context(model);
        let context = Symbol::from("context");
        let valid = Expr::new(
            span,
            ExprNode::Send {
                recv: None,
                method: Symbol::from("valid?"),
                args: if contexts { vec![local_read(&context)] } else { Vec::new() },
                block: None,
                parenthesized: contexts,
            },
        );
        let body = Expr::new(
            span,
            ExprNode::Send { recv: Some(valid), method: Symbol::from("!"), args: Vec::new(), block: None, parenthesized: false },
        );
        let mut params = Vec::new();
        let mut signature = fn_sig(vec![], Ty::Bool);
        if contexts {
            params.push(crate::dialect::Param {
                name: context.clone(),
                default: Some(Expr::new(span, ExprNode::Lit { value: Literal::Nil })),
                keyword: false,
                rest: false,
                forwarding: false,
                from_keyword: false,
                from_kwrest: false,
            });
            if let Ty::Fn { params, .. } = &mut signature {
                params.push(crate::ty::Param {
                    name: context,
                    ty: Ty::Union { variants: vec![Ty::Sym, Ty::Nil].into() }.into(),
                    kind: crate::ty::ParamKind::Optional,
                });
            }
        }
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("invalid?"),
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
    // ActiveModel's `validate!(context = nil)`: `valid?(context) ||
    // raise_validation_error`, which raises `ValidationError.new(self)`
    // (Core's PayPal RequestForInformation runs it in `initialize`). The
    // context is passed exactly when this `valid?` takes one.
    if !defines("validate!") {
        let contexts = uses_validation_context(model);
        let context = Symbol::from("context");
        let valid = Expr::new(
            span,
            ExprNode::Send {
                recv: None,
                method: Symbol::from("valid?"),
                args: if contexts { vec![local_read(&context)] } else { Vec::new() },
                block: None,
                parenthesized: contexts,
            },
        );
        let error = Expr::new(
            span,
            ExprNode::Send {
                recv: Some(Expr::new(
                    span,
                    ExprNode::Const { path: vec![Symbol::from("ActiveModel"), Symbol::from("ValidationError")] },
                )),
                method: Symbol::from("new"),
                args: vec![Expr::new(span, ExprNode::SelfRef)],
                block: None,
                parenthesized: true,
            },
        );
        let raise = Expr::new(
            span,
            ExprNode::Send {
                recv: None,
                method: Symbol::from("raise"),
                args: vec![error],
                block: None,
                parenthesized: true,
            },
        );
        let body = Expr::new(
            span,
            ExprNode::BoolOp { op: crate::expr::BoolOpKind::Or, surface: Default::default(), left: valid, right: raise },
        );
        let mut params = Vec::new();
        let mut signature = fn_sig(vec![], Ty::Bool);
        if contexts {
            params.push(crate::dialect::Param {
                name: context.clone(),
                default: Some(Expr::new(span, ExprNode::Lit { value: Literal::Nil })),
                keyword: false,
                rest: false,
                forwarding: false,
                from_keyword: false,
                from_kwrest: false,
            });
            if let Ty::Fn { params, .. } = &mut signature {
                params.push(crate::ty::Param {
                    name: context,
                    ty: Ty::Union { variants: vec![Ty::Sym, Ty::Nil].into() }.into(),
                    kind: crate::ty::ParamKind::Optional,
                });
            }
        }
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("validate!"),
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
    if !defines("valid?") {
        take_explicit_validation_context(methods, model, span);
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
            signature: Some(fn_sig(vec![], Ty::Union { variants: vec![Ty::Sym, Ty::Nil].into() })),
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
            body: errors_reader(span, errors_readers, "errors", Ty::Array { elem: std::sync::Arc::new(Ty::Str) }),
            signature: Some(fn_sig(vec![], Ty::Array { elem: std::sync::Arc::new(Ty::Str) })),
            effects: EffectSet::default(),
            enclosing_class: Some(model.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }
    if details && !defines("error_details") {
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("error_details"),
            receiver: MethodReceiver::Instance,
            params: Vec::new(),
            body: errors_reader(span, errors_readers, "error_details", crate::lower::errors_add::error_details_ty()),
            signature: Some(fn_sig(vec![], crate::lower::errors_add::error_details_ty())),
            effects: EffectSet::default(),
            enclosing_class: Some(model.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }
    if details && !defines("error_messages_for") {
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("error_messages_for"),
            receiver: MethodReceiver::Instance,
            params: vec![crate::dialect::Param::positional(Symbol::from("attribute"))],
            body: error_messages_for_body(span, errors_readers),
            signature: Some(fn_sig(vec![(Symbol::from("attribute"), Ty::Str)], Ty::Array { elem: std::sync::Arc::new(Ty::Str) })),
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

/// `@<name> ||= []`, created on first read as Rails' `errors` is, so an
/// `errors.add` before any `valid?` has a list to add to; `@<name>` for
/// `ErrorsReaders::FieldRead`.
fn errors_reader(span: Span, errors_readers: ErrorsReaders, name: &str, ty: Ty) -> Expr {
    if errors_readers == ErrorsReaders::FieldRead {
        return Expr::new(span, ExprNode::Ivar { name: Symbol::from(name) });
    }
    let mut empty = Expr::new(span, ExprNode::Array { elements: vec![], style: ArrayStyle::default() });
    empty.ty = Some(ty.clone());
    let mut read = Expr::new(
        span,
        ExprNode::OpAssign {
            target: crate::expr::LValue::Ivar { name: Symbol::from(name) },
            op: crate::expr::OpAssignOp::OrOr,
            value: empty,
        },
    );
    read.ty = Some(ty);
    read
}

/// `out = []; error_details.each { |e| out << e.message if e.attribute == attribute }; out`,
/// the body `ActiveRecord::Base#error_messages_for` has in the shared runtime
/// (`@error_details.each` for `ErrorsReaders::FieldRead`).
fn error_messages_for_body(span: Span, errors_readers: ErrorsReaders) -> Expr {
    let str_array = Ty::Array { elem: std::sync::Arc::new(Ty::Str) };
    let typed = |node: ExprNode, ty: Ty| {
        let mut e = Expr::new(span, node);
        e.ty = Some(ty);
        e
    };
    let var = |name: &str, ty: Ty| typed(ExprNode::Var { id: crate::ident::VarId(0), name: Symbol::from(name) }, ty);
    let call = |recv: Expr, method: &str, args: Vec<Expr>, ty: Ty| {
        typed(ExprNode::Send { recv: Some(recv), method: Symbol::from(method), args, block: None, parenthesized: false }, ty)
    };
    let error_ty = Ty::Class { id: ClassId(Symbol::from("ActiveRecord::ErrorDetail")), args: vec![].into() };
    let error = || var("e", error_ty.clone());
    let picked = typed(
        ExprNode::If {
            cond: call(call(error(), "attribute", vec![], Ty::Str), "==", vec![var("attribute", Ty::Str)], Ty::Bool),
            then_branch: call(var("out", str_array.clone()), "<<", vec![call(error(), "message", vec![], Ty::Str)], str_array.clone()),
            else_branch: typed(ExprNode::Lit { value: Literal::Nil }, Ty::Nil),
        },
        Ty::Nil,
    );
    let block = Expr::new(
        span,
        ExprNode::Lambda {
            extra_params: Vec::new(),
            rest_param: None,
            params: vec![Symbol::from("e")],
            block_param: None,
            body: picked,
            block_style: crate::expr::BlockStyle::Brace,
            signature: Default::default(),
        },
    );
    let details_read = match errors_readers {
        ErrorsReaders::FieldRead => ExprNode::Ivar { name: Symbol::from("error_details") },
        ErrorsReaders::CreatedOnFirstRead => {
            ExprNode::Send { recv: None, method: Symbol::from("error_details"), args: vec![], block: None, parenthesized: false }
        }
    };
    let details = typed(details_read, crate::lower::errors_add::error_details_ty());
    let each = typed(
        ExprNode::Send { recv: Some(details), method: Symbol::from("each"), args: vec![], block: Some(block), parenthesized: false },
        crate::lower::errors_add::error_details_ty(),
    );
    let init = Expr::new(
        span,
        ExprNode::Assign {
            target: crate::expr::LValue::Var { id: crate::ident::VarId(0), name: Symbol::from("out") },
            value: typed(ExprNode::Array { elements: vec![], style: ArrayStyle::default() }, str_array.clone()),
        },
    );
    seq(vec![init, each, var("out", str_array)])
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
        ValidationRule::Presence => vec![inline_presence_check(attr, attr_ty, value, text)],
        ValidationRule::Absence => vec![inline_absence_check(value, text)],
        ValidationRule::Length { min, max, allow_nil, .. } => inline_length_check(
            value,
            min.map(|n| n as usize),
            max.map(|n| n as usize),
            text,
            attr_ty,
            *allow_nil,
        ),
        ValidationRule::Format { pattern, flags, without } => {
            vec![inline_format_check(value, pattern, flags, *without, text)]
        }
        ValidationRule::Numericality { only_integer, checks } => {
            inline_numericality_check(attr, value, *only_integer, checks, text)
        }
        ValidationRule::Comparison { checks } => inline_comparison_check(attr, value, checks, text),
        // `acceptance:`: `errors.add(attr, :accepted) unless
        // Array(accept).include?(value)`.
        ValidationRule::Acceptance { accept } => vec![if_with_nil_else(
            not(send(accept.clone(), "include?", vec![value.clone()])),
            text.push("accepted", None, || value.clone(), Detail::Bare),
        )],
        // Reported by `roundhouse check`: ActiveModel has no
        // AssociatedValidator.
        ValidationRule::Associated if model.parent.is_none() => Vec::new(),
        ValidationRule::Associated => vec![inline_associated_check(model, attr, value, text)],
        // `validates_each :a do |record, attr, value| ... end`:
        // BlockValidator's `@block.call(record, attribute, value)`, as
        // a proc takes it (a parameter past the three reads nil).
        ValidationRule::Each { callable, arity } => {
            let s = Span::synthetic();
            let mut args = vec![
                Expr::new(s, ExprNode::SelfRef),
                Expr::new(s, ExprNode::Lit { value: Literal::Sym { value: attr.clone() } }),
                value.clone(),
            ];
            args.resize_with((*arity).max(3), || Expr::new(s, ExprNode::Lit { value: Literal::Nil }));
            vec![Expr::new(
                s,
                ExprNode::Send {
                    recv: Some(callable.clone()),
                    method: Symbol::from("call"),
                    args: args.into_iter().take(*arity).collect(),
                    block: None,
                    parenthesized: true,
                },
            )]
        }
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
            vec![inline_inclusion_check(value, &resolve(members), *exclusion, false, text)]
        }
        ValidationRule::Inclusion { list, exclusion, cover, list_of: None } => {
            vec![inline_inclusion_check(value, list, *exclusion, *cover, text)]
        }
        ValidationRule::LengthOf { checks, nil_fails_maximum } => {
            inline_length_of_check(attr, value, attr_ty, checks, *nil_fails_maximum, text)
        }
        ValidationRule::FormatOf { regexp, without } => {
            let matched = send(resolve(regexp), "match?", vec![send(value.clone(), "to_s", vec![])]);
            let invalid = if *without { matched } else { not(matched) };
            vec![if_with_nil_else(invalid, text.push("invalid", None, || value.clone(), Detail::Value))]
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
                        recv: Some(Expr::new(Span::synthetic(), ExprNode::SelfRef)),
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
    msgs: Messages,
    assoc_name: &Symbol,
    foreign_key: &Symbol,
    target: &ClassId,
    lookup_pk: Option<&Symbol>,
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
    let push_err = errors_push_expr(msgs.error(assoc_name.as_str(), "required", &Default::default()));
    if polymorphic {
        return if_with_nil_else(bool_op(BoolOpKind::Or, nil_check, zero_check), push_err);
    }
    // `!Target.exists?(@fk)`
    // A resolved reference: a namespace-only `OnlineStore::Theme` beside
    // the top-level `Theme` model must not capture the bare spelling.
    let target_const = super::resolved_class_const(target);
    let exists_call = match lookup_pk {
        // `Target.exists?(uuid: @fk)`. Model_to_library synthesis runs
        // after the post-analyze `exists_conditions` rewrite, so that
        // pass never sees this send; arel (`build_exists_kwargs`) in
        // `lower_models_inner` turns the kwargs form into a SELECT 1
        // probe when the PK column is on the schema.
        Some(pk) => {
            let conditions = Expr::new(
                Span::synthetic(),
                ExprNode::Hash {
                    entries: vec![(
                        Expr::new(
                            Span::synthetic(),
                            ExprNode::Lit { value: Literal::Sym { value: pk.clone() } },
                        ),
                        fk_ivar,
                    )],
                    kwargs: true,
                },
            );
            send(target_const, "exists?", vec![conditions])
        }
        // `Target.exists?(@fk)`
        None => send(target_const, "exists?", vec![fk_ivar]),
    };
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

/// Whether `validates :attr` reads `attr` through its method rather
/// than its `@attr` slot. ActiveModel's `read_attribute_for_validation`
/// is `send(attr)`; the slot is the same value for a column and for an
/// `attr_*` / `has_secure_password` attribute, which keep the ivar read.
/// Anything else is a method: a `?`/`!` name (`validates
/// :has_payments_account?, presence: true`, which as `@has_payments_account?`
/// was not even Ruby), or a reader the model defines or inherits, whose
/// ivar nothing writes.
fn read_through_method(model: &Model, attr: &Symbol, attr_ty: Option<&Ty>) -> bool {
    if attr.as_str().ends_with(['?', '!']) {
        return true;
    }
    attr_ty.is_none()
        && !super::markers::declared_attr_names(model).contains(attr)
        && !crate::lower::secure_password::secure_password_attrs(&model.body).contains(attr)
}

/// Whether the model writes its own reader for the column `attr`
/// (`def tax_invoice_status; ::Status.try_deserialize(super); end`):
/// `read_attribute_for_validation` is `send(attr)`, so the rules judge
/// what that reader answers, not the column's slot. Numericality reads
/// the value before type cast and keeps the slot.
fn overrides_reader(model: &Model, attr: &Symbol) -> bool {
    model.attributes.fields.contains_key(attr)
        && model.body.iter().any(|item| matches!(
            item,
            crate::dialect::ModelBodyItem::Method { method, .. }
                if method.name == *attr && method.receiver == crate::dialect::MethodReceiver::Instance
        ))
}

/// The stem of the locals a check on `attr` names (`<stem>_number`):
/// the name less a `?`/`!`, which no local may carry.
fn local_stem(attr: &Symbol) -> &str {
    attr.as_str().trim_end_matches(['?', '!'])
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
fn inline_presence_check(attr: &Symbol, attr_ty: Option<&Ty>, value: &Expr, text: &ErrorText<'_>) -> Expr {
    // A value read through its method (an association, a `?` predicate,
    // a reader the model defines) has no column type to narrow by:
    // Rails' `value.blank?`, which `false` and whitespace answer too.
    if !matches!(&*value.node, ExprNode::Ivar { .. }) {
        let blank = send(
            Expr::new(Span::synthetic(), ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] }),
            "blank?",
            vec![value.clone()],
        );
        return if_with_nil_else(blank, text.push("blank", None, || value.clone(), Detail::Bare));
    }
    // Temporal columns (schema-typed `Time`) store ISO-8601 text in
    // `@<attr>_raw`; `@<attr>` is never the storage slot (the ruby
    // tree's parse memo is `@__t_<attr>`), so a check against it fired
    // unconditionally — lobsters' `validates :created_at, presence:
    // true` on Username rejected every record. Blank on the stored
    // form is nil-or-empty text. A nullable column types `Time | nil`,
    // which stores the same way.
    if matches!(peel_nilable(attr_ty).0, Some(Ty::Time)) {
        let raw_ivar = ivar(&Symbol::from(format!("{}_raw", attr.as_str())));
        let cond = bool_op(
            BoolOpKind::Or,
            send(raw_ivar.clone(), "nil?", vec![]),
            send(raw_ivar, "empty?", vec![]),
        );
        let push_err = text.push("blank", None, || ivar(attr), Detail::Bare);
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
    let push_err = text.push("blank", None, || ivar(attr), Detail::Bare);
    // The wrapping `if cond then push_err end` (Nil else).
    if_with_nil_else(cond, push_err)
}

/// Inline `validates :attr, absence: true` — the negation of presence.
///   if !(@attr.nil? || (@attr.is_a?(String) && @attr.empty?) || (@attr.is_a?(Array) && @attr.empty?))
///     errors << "attr must be blank"
///   end
/// Reuses the presence condition tree and wraps with unary `!`.
fn inline_absence_check(value: &Expr, text: &ErrorText<'_>) -> Expr {
    // Re-derive the blank-condition (matches inline_presence_check's tree).
    let attr_ivar = value.clone();
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
    let push_err = text.push("present", None, || value.clone(), Detail::Bare);
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
    value: &Expr,
    list: &Expr,
    exclusion: bool,
    cover: bool,
    text: &ErrorText<'_>,
) -> Expr {
    let member = send(list.clone(), if cover { "cover?" } else { "include?" }, vec![value.clone()]);
    if exclusion {
        return if_with_nil_else(member, text.push("exclusion", None, || value.clone(), Detail::Value));
    }
    if_with_nil_else(not(member), text.push("inclusion", None, || value.clone(), Detail::Value))
}

/// Inline `validates :attr, format: { with: /re/ }` (`without:` with
/// `without`). Rails matches `value.to_s`, so nil reads as "":
///   errors << "Attr is invalid" unless /re/.match?(@attr.to_s)
fn inline_format_check(value: &Expr, pattern: &str, flags: &str, without: bool, text: &ErrorText<'_>) -> Expr {
    let regex_lit = Expr::new(
        Span::synthetic(),
        ExprNode::Lit { value: Literal::Regex { pattern: pattern.to_string(), flags: flags.to_string() } },
    );
    let matched = send(regex_lit, "match?", vec![send(value.clone(), "to_s", vec![])]);
    let invalid = if without { matched } else { not(matched) };
    if_with_nil_else(invalid, text.push("invalid", None, || value.clone(), Detail::Value))
}

/// Inline `validates :attr, length: { minimum: M, maximum: N }`.
///
/// Rails' `LengthValidator` (8.1.4) does not treat nil as "skip" for
/// every bound. With `allow_nil` unset, a nil value fails `:minimum`
/// (the error is added without consulting the length) and is skipped
/// for a lone `:maximum` (`skip_nil_check?`). `allow_nil: true` skips
/// the whole check. `allow_nil: false` fails both bounds on nil.
///
/// The `is` (exact length) option isn't in the current ValidationRule
/// shape; left for when the IR adds it.
fn inline_length_check(
    value: &Expr,
    min: Option<usize>,
    max: Option<usize>,
    text: &ErrorText<'_>,
    attr_ty: Option<&Ty>,
    allow_nil: Option<bool>,
) -> Vec<Expr> {
    let (attr_ty, nilable) = peel_nilable(attr_ty);
    // Same narrowing as the presence check: the caller guards the
    // length computation with `!@attr.nil?`, so the read inside it is
    // the nil-excluded value.
    let attr_ivar = match (nilable, attr_ty) {
        (true, Some(inner)) => narrowed(value, inner),
        _ => value.clone(),
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
    let mut nil_errors: Vec<Expr> = Vec::new();
    // One pass over the two bounds. `nil_fails_bound` is Rails'
    // `skip_nil_check?` plus `allow_nil`: unset fails minimum only,
    // `true` skips nil, `false` fails both.
    for (bound, n, kind, op) in [
        (LengthBound::Min, min, "too_short", "<"),
        (LengthBound::Max, max, "too_long", ">"),
    ] {
        let Some(n) = n else { continue };
        let cmp = send(
            len_read.clone(),
            op,
            vec![Expr::new(
                Span::synthetic(),
                ExprNode::Lit { value: Literal::Int { value: n as i64 } },
            )],
        );
        let count = Count::Static(Literal::Int { value: n as i64 });
        let push = text.push(kind, Some(&count), || value.clone(), Detail::Count);
        inner_stmts.push(if_with_nil_else(cmp, push));
        if nil_fails_bound(bound, allow_nil) {
            nil_errors.push(text.push(kind, Some(&count), || value.clone(), Detail::Count));
        }
    }
    let body_seq = seq(inner_stmts);

    // A nil value has no length to read. LengthValidator fails it on
    // `minimum:` (nil is shorter than any minimum) and passes it on
    // `maximum:` (`skip_nil_check?`), unless `allow_nil:` says otherwise:
    //   if @attr.nil? then <too short, when min is set> else body end
    // The guard tests the RAW value — `attr_ivar` above may be the
    // narrowed `Cast` read, which is what the non-nil arm exists to
    // make safe.
    let nil_check = send(value.clone(), "nil?", vec![]);
    let not_nil = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(nil_check.clone()),
            method: Symbol::from("!"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    if nil_errors.is_empty() {
        // `allow_nil: true`, or a lone maximum (Rails skips nil).
        return vec![if_with_nil_else(not_nil, body_seq)];
    }
    let nil_arm = if nil_errors.len() == 1 { nil_errors.pop().unwrap() } else { seq(nil_errors) };
    vec![Expr::new(
        Span::synthetic(),
        ExprNode::If { cond: nil_check, then_branch: nil_arm, else_branch: body_seq },
    )]
}

enum LengthBound {
    Min,
    Max,
}

/// Whether a nil value fails this bound. `None` is Rails' default
/// (`skip_nil_check?` is true only for `:maximum`), not a missing flag.
fn nil_fails_bound(bound: LengthBound, allow_nil: Option<bool>) -> bool {
    match allow_nil {
        Some(true) => false,
        Some(false) => true,
        None => matches!(bound, LengthBound::Min),
    }
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
                let local = Symbol::from(format!("{}_length_{key}", local_stem(attr)));
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
        own.push(kind, Some(count), || value.clone(), Detail::Count)
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
    let len = Symbol::from(format!("{}_length", local_stem(attr)));
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
            if let Association::BelongsTo { name: n, foreign_key, polymorphic, foreign_type, .. } = a {
                if n == name {
                    let mut cols = vec![foreign_key.clone()];
                    if *polymorphic {
                        cols.push(foreign_type.clone().unwrap_or_else(|| Symbol::from(format!("{}_type", name.as_str()))));
                    }
                    return cols;
                }
            }
        }
        vec![name.clone()]
    };
    let rel = Symbol::from(format!("{}_relation", local_stem(attr)));
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
        Finder::Declaring { type_names, type_column, .. } if !type_names.is_empty() => {
            let names = type_names
                .iter()
                .map(|n| Expr::new(s, ExprNode::Lit { value: Literal::Str { value: n.clone() } }))
                .collect();
            let list = Expr::new(s, ExprNode::Array { elements: names, style: ArrayStyle::Brackets });
            send(relation, "where", vec![hash(vec![(sym(type_column.as_str()), list)])])
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
    out.push(if_with_nil_else(taken, text.push("taken", None, || own_value.clone(), Detail::Unknown)));
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
    let err = Symbol::from(format!("{}_comparison_error", local_stem(attr)));
    let rescued = |body: Expr| {
        Expr::new(
            s,
            ExprNode::BeginRescue {
                body,
                rescues: vec![crate::expr::RescueClause {
                    classes: vec![Expr::new(s, ExprNode::Const { path: vec![Symbol::from("ArgumentError")] })],
                    binding: Some(err.clone()),
                    // Rails' `errors.add(attr_name, e.message)`: the
                    // message is the type.
                    body: {
                        let mut push = errors_push_expr(Expr::new(
                            s,
                            ExprNode::StringInterp {
                                parts: vec![
                                    InterpPart::Text { value: format!("{human} ") },
                                    InterpPart::Expr { expr: send(local_read(&err), "message", vec![]) },
                                ],
                            },
                        ));
                        tag_error_detail(&mut push, attr, &crate::expr::ErrorDetail::Message);
                        push
                    },
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
                let local = Symbol::from(format!("{}_{}", local_stem(attr), check.op.key()));
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
    pre.push(text.push("blank", Some(&count), || value.clone(), Detail::CountValue));
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
        pre.push(if_with_nil_else(not(holds), text.push(check.op.key(), Some(&count), || value.clone(), Detail::CountValue)));
        each.push(rescued(seq(pre)));
    }
    vec![Expr::new(s, ExprNode::If { cond: blank, then_branch: blank_branch, else_branch: seq(each) })]
}

fn inline_numericality_check(
    attr: &Symbol,
    value: &Expr,
    only_integer: bool,
    checks: &[NumericCheck],
    text: &ErrorText<'_>,
) -> Vec<Expr> {
    let s = Span::synthetic();
    let raw = || value.clone();
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
    let name = Symbol::from(format!("{}_number", local_stem(attr)));
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
                compares.push(if_with_nil_else(not(holds), text.push(check.op.key(), None, number, Detail::Unknown)));
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
                let local = Symbol::from(format!("{}_{}", local_stem(attr), check.op.key()));
                compares.push(assign_local(&local, resolve(other)));
                (local_read(&local), Count::Dynamic(local_read(&local)))
            }
        };
        let holds = if check.op == NumericOp::In {
            send(bound_expr, op, vec![number()])
        } else {
            send(number(), op, vec![bound_expr])
        };
        compares.push(if_with_nil_else(not(holds), text.push(check.op.key(), Some(&count), number, Detail::Unknown)));
    }
    let tail = if compares.is_empty() { nil() } else { seq(compares) };
    let tail = if only_integer {
        Expr::new(
            s,
            ExprNode::If {
                cond: not(send(regex(integer_re), "match?", vec![send(raw(), "to_s", vec![])])),
                then_branch: text.push("not_an_integer", None, raw, Detail::Value),
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
            then_branch: text.push("not_a_number", None, raw, Detail::Value),
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

/// The options a validator passes its error beside the callback and
/// `message:` ones (activemodel's validators: presence, absence and
/// acceptance none, length `count:`, inclusion, exclusion, format and
/// numericality's `not_a_number` / `not_an_integer` the `value:`,
/// comparison both), or `Unknown` where the push cannot say: a
/// numericality bound's value is Rails' parsed number (a BigDecimal for
/// a Float), uniqueness adds the `existing_id` it found, and
/// validates_associated the association's records.
#[derive(Clone, Copy)]
pub(super) enum Detail {
    Bare,
    Count,
    Value,
    CountValue,
    Unknown,
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
    /// The model's catalog entries, when the app's locale adds any: its
    /// attribute names and message wording then come from there.
    i18n: Option<&'a crate::i18n::ModelI18n>,
    message: Option<&'a RuleMessage>,
}

impl ErrorText<'_> {
    /// The attribute as the message names it: `human_attribute_name`.
    fn human(&self) -> String {
        match self.i18n {
            Some(i18n) => i18n.human_attribute_name(self.attr.as_str()),
            None => humanize(self.attr.as_str()),
        }
    }

    /// `errors << "<Attr> <message>"` for an error of type `kind`
    /// (`blank`, `too_long`, ...), its `%{count}` `count`, and the value
    /// `%{value}` and a callable message see.
    ///
    /// The push records Rails' Error type, `kind` whatever the message,
    /// and the options the validator passes beside the callback and
    /// `message:` ones (`detail`).
    fn push(&self, kind: &str, count: Option<&Count>, value: impl Fn() -> Expr, detail: Detail) -> Expr {
        let mut out = self.push_message(kind, count, &value);
        let simple = |e: &Expr| match &*e.node {
            ExprNode::Ivar { .. } | ExprNode::Var { .. } | ExprNode::Lit { .. } => true,
            ExprNode::Send { recv: None, args, block: None, .. } => args.is_empty(),
            _ => false,
        };
        let count = || match count {
            Some(Count::Static(c)) => Some(Expr::new(Span::synthetic(), ExprNode::Lit { value: c.clone() })),
            Some(Count::Dynamic(e)) => Some(e.clone()),
            None => None,
        }
        .filter(simple);
        let value = || Some(value()).filter(simple);
        let options = match detail {
            Detail::Bare => Some(vec![]),
            Detail::Count => count().map(|c| vec![("count", c)]),
            Detail::Value => value().map(|v| vec![("value", v)]),
            Detail::CountValue => count().zip(value()).map(|(c, v)| vec![("count", c), ("value", v)]),
            Detail::Unknown => None,
        };
        let detail = match options {
            Some(options) => typed_detail(kind, options),
            None => crate::expr::ErrorDetail::Type { name: Symbol::from(kind), options: None },
        };
        tag_error_detail(&mut out, self.attr, &detail);
        out
    }

    fn push_message(&self, kind: &str, count: Option<&Count>, value: &dyn Fn() -> Expr) -> Expr {
        let human = self.human();
        match self.message {
            None => self.push_type(kind, count, value),
            Some(RuleMessage::Key { key }) => self.push_type(key.as_str(), count, value),
            Some(RuleMessage::Text { text }) => errors_push_expr(self.interpolate(text, count, value)),
            Some(RuleMessage::Value { expr }) => {
                // `m = <expr>`, then a Proc's call or the String with
                // the keys ActiveModel supplies interpolated.
                let s = Span::synthetic();
                let local = Symbol::from(format!("{}_message", local_stem(self.attr)));
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
        // The app's locale words the message: `ActiveModel::Error.generate_message`
        // over the catalog (a run-time `count` keeps the English plural form).
        if let Some(i18n) = self.i18n {
            let opts = match count {
                None => Some(crate::i18n::ErrorOpts::default()),
                Some(Count::Static(c)) => Some(crate::i18n::ErrorOpts::count(format_count(c))),
                Some(Count::Dynamic(_)) => None,
            };
            if let Some(parts) = opts.and_then(|o| i18n.try_full_error(self.attr.as_str(), kind, &o)) {
                let parts = parts
                    .into_iter()
                    .map(|p| match p {
                        crate::i18n::Part::Text(value) => InterpPart::Text { value },
                        crate::i18n::Part::Value => InterpPart::Expr { expr: value() },
                    })
                    .collect::<Vec<_>>();
                return errors_push_expr(match parts.as_slice() {
                    [InterpPart::Text { value }] => Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Str { value: value.clone() } }),
                    _ => Expr::new(Span::synthetic(), ExprNode::StringInterp { parts }),
                });
            }
        }
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
        let human = self.human();
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
        // `validation_statements_split` binds these first.
        RuleValue::ClassScoped { value } => resolve(value),
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

/// `String#humanize` on an attribute name, for the messages the lowerer
/// bakes without a model's catalog.
pub(crate) fn humanize(attr: &str) -> String {
    crate::i18n::humanize(attr)
}

/// What a baked message needs from its model: the attribute's
/// `human_attribute_name` and the locale's `errors.format`.
#[derive(Clone, Copy, Default)]
pub(crate) struct Messages<'a>(pub(crate) Option<&'a Model>);

use crate::i18n::ErrorOpts;

impl Messages<'_> {
    /// The full message Rails stores for an error of `kind` on `attr`.
    fn error(&self, attr: &str, kind: &str, opts: &ErrorOpts) -> Expr {
        let default = crate::i18n::ModelI18n::default();
        let i18n = self.0.map(|m| &m.i18n).unwrap_or(&default);
        crate::lower::errors_add::parts_expr(Span::synthetic(), i18n.full_error(attr, kind, opts), None, attr)
    }
}

/// `validates_associated :attr` (ActiveRecord's AssociatedValidator):
///
///   errors << "Attr is invalid" if Array(value).reject { |r|
///     (r.respond_to?(:marked_for_destruction?) && r.marked_for_destruction?) ||
///       r.valid?([:create, :update].include?(validation_context) ? nil : validation_context)
///   }.any?
///
/// Each record validates in the record's own context when that is a
/// custom one (`record_validation_context_for_association`), else in
/// its own default; `reject` asks every record, so each one's errors
/// are filled as in Rails. A collection association reads `to_a`, a
/// singular one is `[]` when nil, and any other attribute is `Array()`.
fn inline_associated_check(model: &Model, attr: &Symbol, value: &Expr, text: &ErrorText<'_>) -> Expr {
    let s = Span::synthetic();
    let sym = |name: &str| Expr::new(s, ExprNode::Lit { value: Literal::Sym { value: Symbol::from(name) } });
    let nil = || Expr::new(s, ExprNode::Lit { value: Literal::Nil });
    let context = || {
        Expr::new(
            s,
            ExprNode::Send { recv: None, method: Symbol::from("validation_context"), args: vec![], block: None, parenthesized: false },
        )
    };
    let record = Symbol::from("associated_record");
    let r = || local_read(&record);
    let default_context = send(
        Expr::new(s, ExprNode::Array { elements: vec![sym("create"), sym("update")], style: ArrayStyle::default() }),
        "include?",
        vec![context()],
    );
    let own_context = Expr::new(s, ExprNode::If { cond: default_context, then_branch: nil(), else_branch: context() });
    let marked = bool_op(
        BoolOpKind::And,
        send(r(), "respond_to?", vec![sym("marked_for_destruction?")]),
        send(r(), "marked_for_destruction?", vec![]),
    );
    let valid_object = bool_op(BoolOpKind::Or, marked, send(r(), "valid?", vec![own_context]));
    let association = model.associations().find(|a| a.name() == attr);
    let records = match association {
        Some(Association::HasMany { .. } | Association::HasAndBelongsToMany { .. }) => send(value.clone(), "to_a", vec![]),
        Some(Association::BelongsTo { .. } | Association::HasOne { .. }) => Expr::new(
            s,
            ExprNode::If {
                cond: send(value.clone(), "nil?", vec![]),
                then_branch: Expr::new(s, ExprNode::Array { elements: vec![], style: ArrayStyle::default() }),
                else_branch: Expr::new(s, ExprNode::Array { elements: vec![value.clone()], style: ArrayStyle::default() }),
            },
        ),
        None => Expr::new(
            s,
            ExprNode::Send { recv: None, method: Symbol::from("Array"), args: vec![value.clone()], block: None, parenthesized: true },
        ),
    };
    let block = Expr::new(
        s,
        ExprNode::Lambda { signature: Default::default(), extra_params: Vec::new(),
            params: vec![record.clone()],
            rest_param: None,
            block_param: None,
            body: valid_object,
            block_style: crate::expr::BlockStyle::default(),
        },
    );
    let invalid = Expr::new(
        s,
        ExprNode::Send { recv: Some(records), method: Symbol::from("reject"), args: vec![], block: Some(block), parenthesized: false },
    );
    if_with_nil_else(send(invalid, "any?", vec![]), text.push("invalid", None, || value.clone(), Detail::Unknown))
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
        ErrorText { attr, model_human: "Post", i18n_keys: keys, i18n_scope: "activerecord", i18n: None, message: None }
    }

    #[test]
    fn presence_emits_blank_error() {
        let a = attr();
        let expr = inline_presence_check(&a, None, &ivar(&a), &plain(&a));
        assert_eq!(collect_error_messages(&expr), vec!["Title can't be blank"]);
    }

    #[test]
    fn presence_str_typed_attr_drops_is_a_array_branch() {
        // Statically-typed string attr: the `is_a?(Array) && ...` arm
        // must drop out, otherwise tsc narrows the dead branch to
        // `never` and rejects the subsequent `.length` access.
        let a = attr();
        let expr = inline_presence_check(&a, Some(&Ty::Str), &ivar(&a), &plain(&a));
        let dbg = format!("{:?}", expr);
        assert!(
            !dbg.contains("\"Array\""),
            "Str-typed attr should drop is_a?(Array); tree: {dbg}",
        );
    }

    #[test]
    fn presence_of_a_temporal_attr_reads_the_stored_text() {
        // A temporal column keeps ISO-8601 text in `@<attr>_raw`; `@<attr>`
        // is never assigned, so reading it made the check fire for every
        // record. The nullable `Time | nil` column stores the same way.
        let a = attr();
        let nullable = Ty::Union { variants: vec![Ty::Time, Ty::Nil].into() };
        for ty in [Ty::Time, nullable] {
            let expr = inline_presence_check(&a, Some(&ty), &ivar(&a), &plain(&a));
            let dbg = format!("{:?}", expr);
            assert!(dbg.contains("title_raw"), "{ty:?} should read @title_raw; tree: {dbg}");
            assert!(!dbg.contains("\"Array\""), "{ty:?} has no Array arm; tree: {dbg}");
            assert_eq!(collect_error_messages(&expr), vec!["Title can't be blank"]);
        }
    }

    #[test]
    fn presence_array_typed_attr_drops_is_a_string_branch() {
        let a = attr();
        let expr = inline_presence_check(&a, Some(&Ty::Array { elem: std::sync::Arc::new(Ty::Str) }), &ivar(&a), &plain(&a));
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
        let text = ErrorText { attr: &a, model_human: "Post", i18n_keys: &[String::from("post")], i18n_scope: "activerecord", i18n: None, message: Some(&message) };
        let expr = inline_presence_check(&a, None, &ivar(&a), &text);
        assert_eq!(collect_error_messages(&expr), vec!["Title must be given"]);
    }

    #[test]
    fn error_details_mode_records_the_detail_beside_the_message() {
        // Targets without the runtime's `ErrorAttributes` keep each
        // error's type and short message in `error_details`.
        let a = attr();
        let mut expr = inline_presence_check(&a, None, &ivar(&a), &plain(&a));
        push_error_details(&mut expr, &crate::i18n::ModelI18n::default());
        let dbg = format!("{expr:?}");
        assert!(dbg.contains("error_details") && dbg.contains("ErrorDetail"), "no detail pushed: {dbg}");
        assert!(dbg.contains("\"blank\"") && dbg.contains("\"can't be blank\""), "type or short message missing: {dbg}");
        // The hint stays: the CRuby and JRuby trees record the push in `ErrorAttributes` too.
        assert!(dbg.contains("ErrorAttribute"), "the push lost its hint: {dbg}");
        assert_eq!(collect_error_messages(&expr), vec!["Title can't be blank"]);
    }

    #[test]
    fn a_push_that_already_pushed_its_detail_is_left_be() {
        // `errors.add` (`lower::errors_add`) pushes the detail with its message
        // and keeps the hint, so the pass over a body that holds one must not
        // record the error a second time.
        let a = attr();
        let mut expr = inline_presence_check(&a, None, &ivar(&a), &plain(&a));
        let i18n = crate::i18n::ModelI18n::default();
        push_error_details(&mut expr, &i18n);
        let once = format!("{expr:?}");
        push_error_details(&mut expr, &i18n);
        assert_eq!(format!("{expr:?}"), once, "a second pass pushed the detail again");
        assert_eq!(once.matches("\"error_details\"").count(), 1, "{once}");
    }

    #[test]
    fn absence_emits_must_be_blank_error() {
        let a = attr();
        let expr = inline_absence_check(&ivar(&a), &plain(&a));
        assert_eq!(collect_error_messages(&expr), vec!["Title must be blank"]);
    }

    #[test]
    fn length_min_only_emits_too_short() {
        let a = attr();
        let exprs = inline_length_check(&ivar(&a), Some(5), None, &plain(&a), None, None);
        assert_eq!(exprs.len(), 1, "length lowers to one outer expression");
        let msgs: Vec<String> = exprs.iter().flat_map(collect_error_messages).collect();
        // Nil fails the minimum outright, and a present value fails when shorter.
        assert_eq!(
            msgs,
            vec![
                "Title is too short (minimum is 5 characters)",
                "Title is too short (minimum is 5 characters)",
            ],
        );
        let (cond, then_branch, else_branch) = root_if(&exprs);
        assert!(is_nil_predicate(cond), "nil takes the too-short arm");
        assert_eq!(collect_error_messages(then_branch), vec!["Title is too short (minimum is 5 characters)"]);
        assert!(!is_nil_lit(else_branch), "the present value still measures length");
    }

    #[test]
    fn length_max_only_skips_nil() {
        let a = attr();
        let exprs = inline_length_check(&ivar(&a), None, Some(100), &plain(&a), None, None);
        let msgs: Vec<String> = exprs.iter().flat_map(collect_error_messages).collect();
        assert_eq!(msgs, vec!["Title is too long (maximum is 100 characters)"]);
        let (cond, _, else_branch) = root_if(&exprs);
        assert!(is_not_nil(cond), "a lone maximum skips nil, as Rails does");
        assert!(is_nil_lit(else_branch));
    }

    #[test]
    fn length_allow_nil_skips_the_minimum() {
        let a = attr();
        let exprs = inline_length_check(&ivar(&a), Some(5), None, &plain(&a), None, Some(true));
        let (cond, _, else_branch) = root_if(&exprs);
        assert!(is_not_nil(cond), "allow_nil skips the check when the value is nil");
        assert!(is_nil_lit(else_branch));
    }

    #[test]
    fn length_allow_nil_false_fails_a_maximum_on_nil() {
        let a = attr();
        let exprs = inline_length_check(&ivar(&a), None, Some(100), &plain(&a), None, Some(false));
        let (cond, then_branch, _) = root_if(&exprs);
        assert!(is_nil_predicate(cond));
        assert_eq!(
            collect_error_messages(then_branch),
            vec!["Title is too long (maximum is 100 characters)"],
        );
    }

    #[test]
    fn length_min_and_max_emits_both_in_order() {
        let a = attr();
        let exprs = inline_length_check(&ivar(&a), Some(5), Some(100), &plain(&a), None, None);
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

    fn root_if(exprs: &[Expr]) -> (&Expr, &Expr, &Expr) {
        let ExprNode::If { cond, then_branch, else_branch } = exprs[0].node.as_ref() else {
            panic!("length check is an if");
        };
        (cond, then_branch, else_branch)
    }

    fn is_nil_predicate(expr: &Expr) -> bool {
        matches!(expr.node.as_ref(), ExprNode::Send { method, .. } if method.as_str() == "nil?")
    }

    fn is_not_nil(expr: &Expr) -> bool {
        matches!(
            expr.node.as_ref(),
            ExprNode::Send { method, recv: Some(recv), .. }
                if method.as_str() == "!" && is_nil_predicate(recv)
        )
    }

    fn is_nil_lit(expr: &Expr) -> bool {
        matches!(expr.node.as_ref(), ExprNode::Lit { value: Literal::Nil })
    }

    #[test]
    fn length_message_interpolates_count() {
        let a = attr();
        let message = RuleMessage::Text { text: "needs %{count} for %{model}".into() };
        let text = ErrorText { attr: &a, model_human: "Post", i18n_keys: &[String::from("post")], i18n_scope: "activerecord", i18n: None, message: Some(&message) };
        let exprs = inline_length_check(&ivar(&a), None, Some(4), &text, None, None);
        let msgs: Vec<String> = exprs.iter().flat_map(collect_error_messages).collect();
        assert_eq!(msgs, vec!["Title needs 4 for Post"]);
    }

    #[test]
    fn format_emits_invalid_error() {
        let a = attr();
        let expr = inline_format_check(&ivar(&a), "[A-Z]+", "", false, &plain(&a));
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
        let expr = inline_inclusion_check(&ivar(&a), &list, false, false, &plain(&a));
        assert_eq!(collect_error_messages(&expr), vec!["Title is not included in the list"]);
        let expr = inline_inclusion_check(&ivar(&a), &list, true, false, &plain(&a));
        assert_eq!(collect_error_messages(&expr), vec!["Title is reserved"]);
    }

    #[test]
    fn numericality_bare_emits_nan_only() {
        let a = attr();
        let exprs = inline_numericality_check(&a, &ivar(&a), false, &[], &plain(&a));
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
        let exprs = inline_numericality_check(&a, &ivar(&a), true, &checks, &plain(&a));
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
        let exprs = inline_numericality_check(&a, &ivar(&a), false, &checks, &plain(&a));
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
        let expr = inline_belongs_to_check(Messages::default(), &assoc_name, &foreign_key, &target, None, false);
        assert_eq!(collect_error_messages(&expr), vec!["Article must exist"]);
    }

    #[test]
    fn belongs_to_with_a_declared_primary_key_looks_the_target_up_by_it() {
        // `belongs_to :product, foreign_key: "product_uuid", primary_key:
        // "uuid"`: the foreign key holds `products.uuid`, so asking
        // `Product.exists?(@product_uuid)` would compare it with `id`.
        let assoc_name = Symbol::from("product");
        let foreign_key = Symbol::from("product_uuid");
        let target = ClassId(Symbol::from("Product"));
        let pk = Symbol::from("uuid");
        let expr = inline_belongs_to_check(
            Messages::default(),
            &assoc_name,
            &foreign_key,
            &target,
            Some(&pk),
            false,
        );
        let rendered = format!("{expr:?}");
        assert!(rendered.contains("\"exists?\"") && rendered.contains("\"uuid\"") && rendered.contains("kwargs: true"), "{rendered}");
        assert_eq!(collect_error_messages(&expr), vec!["Product must exist"]);
    }

    #[test]
    fn polymorphic_belongs_to_checks_the_id_but_names_no_class() {
        // The class to query lives in the row's `<assoc>_type` column,
        // and `target` is the assoc-name phantom — writing it out was an
        // uninitialized-constant NameError on the first save.
        let assoc_name = Symbol::from("record");
        let foreign_key = Symbol::from("record_id");
        let target = ClassId(Symbol::from("Record"));
        let expr = inline_belongs_to_check(Messages::default(), &assoc_name, &foreign_key, &target, None, true);
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
    narrowed(&ivar(attr), inner)
}

/// `Cast(value, T)`, [`narrowed_ivar`] for a read already built.
fn narrowed(value: &Expr, inner: &Ty) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Cast { value: value.clone(), target_ty: inner.clone() },
    )
}
