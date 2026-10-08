//! `include ActiveModel::Model` on a plain class — synthesized, not
//! provided.
//!
//! Rails' `ActiveModel::Model` (an alias for `ActiveModel::API`) mixes
//! six modules into a class that has no table: attribute assignment,
//! validations, conversion, naming, translation, access. Almost all of
//! that is machinery for something else; what a class *including* it
//! actually gains, at the call sites our corpus has, is three methods:
//!
//!   * `initialize(attributes = {})` — assign each key through the
//!     matching public writer;
//!   * `valid?` — run the declared validations (none, in a class that
//!     declares none, so: `true`);
//!   * `persisted?` — `false`, the whole point of a tableless model.
//!
//! Two ways to give a class those. Ship `ActiveModel::Model` as a
//! runtime module and leave the `include` standing, or synthesize the
//! three methods into the class and drop the `include`. This pass does
//! the second, because the runtime module can only assign through
//! `public_send("#{name}=", value)` — a dispatch on a computed name,
//! which the CRuby overlay serves and no strict target can. The
//! synthesized form names each ivar literally, so every target compiles
//! it, and the emitted class says what it got instead of pointing at a
//! module the reader has to go read.
//!
//! The writer list IS the contract, not a convenience: Rails assigns
//! through public writers and raises `UnknownAttributeError` for a key
//! with none, so a class's `attr_accessor`/`attr_writer` surface (which
//! ingest has already lowered to `AttributeWriter` methods) is exactly
//! the set of keys `new` accepts.
//!
//! WHY LIBRARY CLASSES ONLY. A superclass-less class under
//! `app/models/` that includes this is classified `ClassKind::Model` at
//! ingest (`library_class::includes_active_model`) and rides the
//! tableless-model path, which lowers its `validates` DSL into a real
//! `valid?`/`errors` pair. That path is strictly better and already
//! exists; this one covers the classes it cannot see — campfire's
//! `ActionText::Attachment::OpengraphEmbed` lives in `lib/rails_ext/`,
//! so it is a library class no matter what it includes, and the
//! surviving `include` was a `NameError` at load time.
//!
//! DECLINES, each with a `lower_residue` entry naming the reason:
//!   * the class defines its own `initialize` — Rails' would be
//!     overridden anyway, but then `valid?`/`persisted?` are the only
//!     gain and dropping the `include` silently would be a guess about
//!     what else the module was there for;
//!   * the class declares a validation (`validate` method or a
//!     `validates` class-body call) — a synthesized `valid? => true`
//!     would answer *yes* for a record the app wrote rules to reject,
//!     which is the one failure mode worth refusing outright;
//!   * the class has no attribute writers — nothing to assign, so the
//!     `include` is doing something this pass does not model.

use crate::app::App;
use crate::diagnostic::Diagnostic;
use crate::dialect::{AccessorKind, LibraryClass, MethodDef, MethodReceiver, Param};
use crate::effect::EffectSet;
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;

const MODULE: &str = "ActiveModel::Model";

pub fn apply_active_model_model_synthesis(app: &mut App) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for lc in &mut app.library_classes {
        if !lc.includes.iter().any(|i| i.0.as_str() == MODULE) {
            continue;
        }
        if let Some(reason) = decline_reason(lc) {
            diags.push(crate::lower::residue_diagnostic(
                "active_model_model",
                "include ActiveModel::Model",
                Span::synthetic(),
                reason,
                format!(
                    "`{}` includes `{MODULE}`, left standing ({reason}) — the module has no \
                     definition in any emitted tree, so the class raises NameError at load time",
                    lc.name.0.as_str()
                ),
            ));
            continue;
        }
        let writers = attribute_writers(&lc.methods);
        let owner = lc.name.clone();
        lc.methods.push(attributes_initialize(&owner, Span::synthetic(), &writers));
        if !defines(lc, "valid?") {
            lc.methods.push(constant_predicate(&owner, "valid?", true));
        }
        if !defines(lc, "persisted?") {
            lc.methods.push(constant_predicate(&owner, "persisted?", false));
        }
        lc.includes.retain(|i| i.0.as_str() != MODULE);
    }
    diags
}

/// `None` when the class can be synthesized; otherwise the reason the
/// ledger records.
fn decline_reason(lc: &LibraryClass) -> Option<&'static str> {
    if defines(lc, "initialize") {
        return Some("class defines its own initialize");
    }
    if defines(lc, "validate") || declares_validates(lc) {
        return Some("class declares validations, which this pass does not lower");
    }
    if attribute_writers(&lc.methods).is_empty() {
        return Some("class declares no attribute writers to assign through");
    }
    None
}

fn defines(lc: &LibraryClass, name: &str) -> bool {
    lc.methods
        .iter()
        .any(|m| m.receiver == MethodReceiver::Instance && m.name.as_str() == name)
}

/// A `validates …` / `validate :sym` call captured in the class body.
/// Ingest parks any class-body call it does not model in `unknown_calls`
/// as real IR, which is what makes this checkable rather than a guess.
fn declares_validates(lc: &LibraryClass) -> bool {
    lc.unknown_calls.iter().any(|e| {
        matches!(&*e.node, ExprNode::Send { recv: None, method, .. } if is_validation_macro(method.as_str()))
    })
}

/// The names a `attr_writer` / `attr_accessor` declared, in declaration
/// order. Ingest tags those `AccessorKind::AttributeWriter`, so this
/// reads the tag rather than re-deriving the shape from the body.
fn attribute_writers(methods: &[MethodDef]) -> Vec<Symbol> {
    let mut out = Vec::new();
    for m in methods {
        if m.receiver != MethodReceiver::Instance || m.kind != AccessorKind::AttributeWriter {
            continue;
        }
        let name = m.name.as_str();
        let Some(base) = name.strip_suffix('=') else { continue };
        let base = Symbol::from(base);
        if !out.contains(&base) {
            out.push(base);
        }
    }
    out
}

/// `def initialize(attrs = {}) @href = attrs[:href]; … end` — the
/// constructor `ActiveModel::Model` supplies, built once here and
/// shared with the model-side twin
/// (`model_to_library::validations::push_active_model_constructor`),
/// which reaches the same shape from a `Model`'s `attr_*` declarations
/// instead of a `LibraryClass`'s lowered writers.
///
/// Symbol keys, and only symbol keys: every call site in the corpus
/// builds the hash as a literal with symbol keys. Rails' own path goes
/// through `assign_attributes`, which stringifies and would accept
/// both; accepting both here would mean a lookup per key with a
/// fallback — two reads of a hash that has one spelling. A string-keyed
/// caller belongs here as a deliberate change, not as a shape guessed
/// at in advance.
pub(crate) fn attributes_initialize(
    owner: &ClassId,
    span: Span,
    names: &[Symbol],
) -> MethodDef {
    let attrs = Symbol::from("attrs");
    let assigns: Vec<Expr> = names
        .iter()
        .map(|name| {
            let recv = Expr::new(
                span,
                ExprNode::Var { id: crate::ident::VarId(0), name: attrs.clone() },
            );
            // `attrs[:name]` as the `[]` send every target already
            // lowers — there is no Index node.
            let read = Expr::new(
                span,
                ExprNode::Send {
                    recv: Some(recv),
                    method: Symbol::from("[]"),
                    args: vec![Expr::new(
                        span,
                        ExprNode::Lit { value: Literal::Sym { value: name.clone() } },
                    )],
                    block: None,
                    parenthesized: true,
                },
            );
            Expr::new(
                span,
                ExprNode::Assign { target: LValue::Ivar { name: name.clone() }, value: read },
            )
        })
        .collect();
    let mut param = Param::positional(attrs);
    param.default =
        Some(Expr::new(span, ExprNode::Hash { entries: Vec::new(), kwargs: false }));
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Private,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("initialize"),
        receiver: MethodReceiver::Instance,
        params: vec![param],
        body: Expr::new(span, ExprNode::Seq { exprs: assigns }),
        signature: None,
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: true,
        block_param: None,
    }
}

fn constant_predicate(owner: &ClassId, name: &str, value: bool) -> MethodDef {
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from(name),
        receiver: MethodReceiver::Instance,
        params: Vec::new(),
        body: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Bool { value } }),
        signature: None,
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

/// Whether `name` is an ActiveModel validation macro: `validates`,
/// `validates!`, `validate`, `validates_with`, `validates_each`, or a
/// `validates_<kind>_of` helper.
pub(crate) fn is_validation_macro(name: &str) -> bool {
    matches!(name, "validates" | "validates!" | "validate" | "validates_with" | "validates_each")
        || (name.starts_with("validates_") && name.ends_with("_of"))
}

/// The validation macros a LIBRARY class's body declares that its
/// emitted class drops, each an `unsupported` error at the declaration.
///
/// A class is lowered as a (tableless) model when ingest classifies it
/// one: an ActiveRecord descendant, or a superclass-less ActiveModel
/// class that validates (`ingest::app`'s `validating_active_model`).
/// Any other class declaring a validation is a library class, and
/// the emitter replays its body only on a base it does not model
/// (`emit::ruby::replays_class_body`), or its validation calls over the
/// ActiveModel::Validations port where it includes the module and the
/// tree carries the port (`emit::ruby::library::active_model_validation_calls`);
/// otherwise the call is dropped with a `lower_residue` warning that
/// `check` never saw. Core's `class Form < BaseForm; include
/// ActiveModel::Validations; validates …` would answer `valid?` without
/// the rule, so a dropped rule is an error, as a dropped model DSL call
/// is (`model_to_library::diagnose_unclaimed_model_dsl`, which also
/// covers an STI subclass of a model, skipped here).
pub(crate) fn diagnose_dropped_validations(app: &App) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for lc in &app.library_classes {
        // A class below a model is an STI subclass
        // `lower::sti_subclass_validations` lowers or refuses (and
        // `diagnose_unclaimed_model_dsl` reports).
        if below_model(lc, app)
            || !lc.unknown_calls.iter().any(|call| validation_call(call).is_some())
            || crate::emit::ruby::replays_class_body(lc, app)
        {
            continue;
        }
        let kept = crate::emit::ruby::modelled_class_body_calls(lc, app);
        let why = if lc.is_module {
            "it is a module body, which no lowering runs validations from".to_string()
        } else if crate::emit::ruby::includes_active_model_validations(lc, app) {
            let parent = lc
                .parent
                .as_ref()
                .map(|p| format!(" (its superclass `{}` is no model the tree lowers)", p.0.as_str()))
                .unwrap_or_default();
            format!(
                "it is a library class{parent} over ActiveModel::Validations, whose port only the PostgreSQL Ruby-family tree carries"
            )
        } else if let Some(parent) = &lc.parent {
            format!(
                "it is a library class (its superclass `{}` is no model the tree lowers), so its `valid?` would not run the rule",
                parent.0.as_str()
            )
        } else {
            "it is a library class (no `include ActiveModel::Validations` or `ActiveModel::Model` of its own), so its `valid?` would not run the rule"
                .to_string()
        };
        for call in &lc.unknown_calls {
            let Some(name) = validation_call(call) else { continue };
            if kept.iter().any(|k| std::ptr::eq(*k, call)) {
                continue;
            }
            out.push(Diagnostic::unsupported(
                call.span,
                None,
                name,
                format!("validation on `{}` not lowered and dropped: {why}", lc.name.0.as_str()),
            ));
        }
    }
    out
}

/// Whether `lc`'s superclass chain, through library classes, reaches a
/// model.
pub(crate) fn below_model(lc: &LibraryClass, app: &App) -> bool {
    let mut seen: Vec<&ClassId> = vec![&lc.name];
    let mut parent = lc.parent.as_ref();
    while let Some(p) = parent {
        if app.models.iter().any(|m| &m.name == p) {
            return true;
        }
        if seen.contains(&p) {
            return false;
        }
        seen.push(p);
        parent = app.library_classes.iter().find(|c| &c.name == p && !c.is_module).and_then(|c| c.parent.as_ref());
    }
    false
}

/// The classes whose `errors` is the ActiveModel::Errors port
/// (runtime/active_model_validations.rb) rather than the shared
/// runtime's `Array[String]` accumulator: on the PostgreSQL tree, every
/// library class over ActiveModel::Validations that no model is above
/// (api_versioning's FrozenRecord ApiVersionRecord), and every app
/// module only such classes include (its ApiVersionValidations, whose
/// `handle_is_valid` runs `errors.add(:handle, msg)` on the record).
/// The errors lowerings (errors_add, errors_index, errors_full_messages)
/// rewrite the accumulator's spellings, so they stand down on these.
pub(crate) struct ValidationsPort {
    owners: crate::det::HashSet<ClassId>,
    /// The classes and modules above the port's classes (superclasses,
    /// includes) that are not themselves on it: a receiver typed as one
    /// (frozen_record's `FrozenRecord::Base`) may be a port record.
    above: crate::det::HashSet<ClassId>,
}

impl ValidationsPort {
    pub(crate) fn of(app: &App) -> Self {
        let mut owners = crate::det::HashSet::default();
        if app.primary_database_adapter() != Some(&crate::database::DatabaseAdapter::Postgresql) {
            return ValidationsPort { owners, above: Default::default() };
        }
        let port: Vec<&LibraryClass> = app
            .library_classes
            .iter()
            .filter(|lc| {
                !lc.is_module
                    && !below_model(lc, app)
                    && crate::emit::ruby::includes_active_model_validations(lc, app)
            })
            .collect();
        if port.is_empty() {
            return ValidationsPort { owners, above: Default::default() };
        }
        owners.extend(port.iter().map(|lc| lc.name.clone()));
        let names = |include: &ClassId, module: &str| {
            let include = include.0.as_str().trim_start_matches("::");
            include == module || module.ends_with(&format!("::{include}"))
        };
        for module in app.library_classes.iter().filter(|c| c.is_module) {
            let name = module.name.0.as_str();
            if !port.iter().any(|lc| crate::emit::ruby::includes_module(lc, app, name)) {
                continue;
            }
            // Any includer off the port (a model, another library
            // class, or a module one of those includes) keeps the
            // module on the accumulator's spellings, as before.
            let library_includer = app.library_classes.iter().any(|c| {
                !c.is_module
                    && !owners.contains(&c.name)
                    && crate::emit::ruby::includes_module(c, app, name)
            });
            let model_includer = app.models.iter().any(|m| {
                crate::analyze::model_includes(m).iter().any(|include| {
                    names(include, name)
                        || app.library_classes.iter().any(|c| {
                            c.is_module
                                && names(include, c.name.0.as_str())
                                && crate::emit::ruby::includes_module(c, app, name)
                        })
                })
            });
            if !library_includer && !model_includer {
                owners.insert(module.name.clone());
            }
        }
        let mut above = crate::det::HashSet::default();
        for lc in &port {
            let mut queue: Vec<&LibraryClass> = vec![lc];
            let mut seen: Vec<&ClassId> = Vec::new();
            while let Some(c) = queue.pop() {
                if seen.contains(&&c.name) {
                    continue;
                }
                seen.push(&c.name);
                above.insert(c.name.clone());
                let parent = c.parent.iter().filter(|_| !c.is_module);
                for (name, module) in c.includes.iter().map(|i| (i, true)).chain(parent.map(|p| (p, false))) {
                    let name = name.0.as_str().trim_start_matches("::");
                    above.insert(ClassId(crate::ident::Symbol::from(name)));
                    queue.extend(
                        crate::emit::ruby::library_classes_named(name, app).into_iter().filter(|c| c.is_module == module),
                    );
                }
            }
        }
        above.retain(|c| !owners.contains(c));
        ValidationsPort { owners, above }
    }

    /// Whether `errors` (a zero-arg `errors` send) may read the port's
    /// Errors without the receiver's type saying so: the tree carries
    /// the port, and the receiver is untyped, or typed as a class above
    /// a port class. The errors lowerings refuse such a site rather than
    /// guess the Array accumulator (which a port record does not have).
    pub(crate) fn unknown(&self, errors: &Expr, owner: Option<&ClassId>) -> bool {
        if self.owners.is_empty() || self.answers(errors, owner) {
            return false;
        }
        let ExprNode::Send { recv: Some(r), .. } = &*errors.node else { return false };
        if matches!(&*r.node, ExprNode::SelfRef) {
            return false;
        }
        fn maybe(ty: Option<&crate::ty::Ty>, above: &crate::det::HashSet<ClassId>) -> bool {
            use crate::ty::Ty;
            match ty {
                None | Some(Ty::Untyped | Ty::Var { .. }) => true,
                Some(Ty::Class { id, .. }) => above.contains(id),
                Some(Ty::Union { variants }) => variants.iter().any(|v| maybe(Some(v), above)),
                Some(_) => false,
            }
        }
        maybe(r.ty.as_ref(), &self.above)
    }

    /// Marks `site` (an `errors` spelling `unknown` holds for) refused,
    /// naming the port's classes: the Ruby emitter writes the raise.
    pub(crate) fn refuse(&self, site: &mut Expr, spelling: &str) {
        let mut names: Vec<&str> = self.owners.iter().map(|c| c.0.as_str()).collect();
        names.sort_unstable();
        let shown = names.iter().take(4).map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
        let more = if names.len() > 4 { format!(" and {} more", names.len() - 4) } else { String::new() };
        site.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
            target: None,
            construct: crate::ident::Symbol::from(spelling),
            detail: format!(
                "the receiver's type is unknown, and on this PostgreSQL tree `errors` is the ActiveModel::Errors \
                 port for {shown}{more} but the Array[String] accumulator for models: type the receiver"
            ),
        });
    }

    /// Whether `errors` (a zero-arg `errors` send) reads the port's
    /// Errors: the body's owner answers it for a receiver-less or `self`
    /// send, the receiver's type otherwise (`record.errors` on a record
    /// typed as one of the classes, or a nilable one).
    pub(crate) fn answers(&self, errors: &Expr, owner: Option<&ClassId>) -> bool {
        if self.owners.is_empty() {
            return false;
        }
        let ExprNode::Send { recv, .. } = &*errors.node else { return false };
        let port_class = |ty: &crate::ty::Ty| {
            matches!(ty, crate::ty::Ty::Class { id, .. } if self.owners.contains(id))
        };
        match recv {
            None => owner.is_some_and(|o| self.owners.contains(o)),
            Some(r) if matches!(&*r.node, ExprNode::SelfRef) => owner.is_some_and(|o| self.owners.contains(o)),
            Some(r) => match r.ty.as_ref() {
                Some(crate::ty::Ty::Union { variants }) => {
                    variants.iter().any(port_class)
                        && variants.iter().all(|v| port_class(v) || matches!(v, crate::ty::Ty::Nil))
                }
                Some(ty) => port_class(ty),
                None => false,
            },
        }
    }
}

/// A class-body call ActiveModel::Validations answers: a validation
/// macro, or a `with_options` whose block holds only such calls
/// (api_versioning's `with_options unless: :reconnaissance do validates
/// … end`). The name is the call's.
pub(crate) fn validation_call(call: &Expr) -> Option<&str> {
    let ExprNode::Send { recv, method, block, .. } = &*call.node else { return None };
    if recv.as_ref().is_some_and(|r| !matches!(&*r.node, ExprNode::SelfRef)) {
        return None;
    }
    if is_validation_macro(method.as_str()) {
        return Some(method.as_str());
    }
    if method.as_str() != "with_options" {
        return None;
    }
    let Some(block) = block else { return None };
    let ExprNode::Lambda { body, .. } = &*block.node else { return None };
    let statements: Vec<&Expr> = match &*body.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![body],
    };
    (!statements.is_empty() && statements.iter().all(|s| validation_call(s).is_some())).then_some(method.as_str())
}

