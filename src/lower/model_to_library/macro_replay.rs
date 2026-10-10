//! Model-body calls of app-defined class macros, replayed at their
//! source position or refused with the reason.
//!
//! A model body calls class methods its own concerns (`class_methods
//! do`), its model ancestors (ApplicationRecord's concerns) or an app
//! module it extends define: Core's `metadata_accessor :flag`,
//! `enable_reaper!`, `trusted_id_attribute :shop_id`. The lowered model
//! already defines each such `def self.x` (or extends the module), so
//! replaying the call is what Ruby runs, provided everything the macro
//! does at class-body time is something the lowered class answers.
//!
//! It is not when the macro reaches model DSL (`belongs_to`,
//! `attribute`, `query_constraints`, a lifecycle callback, ...): the
//! lowered model applies those statically from the body it was ingested
//! from and the runtime base has no class-side `belongs_to` to call, so
//! a replay would either raise or declare something the model never
//! reads. Such a call is refused, naming the DSL it reaches and the path
//! to it ([`MacroVerdict::Refuse`]). So is one that calls a class-side
//! method neither the app nor Ruby's Module/Class surface (plus the
//! runtime's core extensions) answers, one that `send`s a computed name,
//! and one that calls `super`.
//!
//! The walk follows what runs at class-body time: the macro's body, the
//! class macros it calls on `self` (or on a local bound to `self`), and
//! the blocks of iterators it runs now (`names.each do ... end`,
//! `class_eval do ... end`, `tap`). It does not enter what runs later or
//! on another object: a `define_method` body, a lambda, a `Module.new` /
//! `Class.new` block.
//!
//! `lower::replay_model_macros` turns each replayed `Unknown` item into a
//! `ClassBody` statement, which the Ruby-family emitter writes into the
//! class body after the methods it may call; `unclaimed_unknowns` reports
//! the refusals; an STI subclass (a library class under a model) replays
//! its own through `emit::ruby::library::modelled_class_body_calls`.

use crate::dialect::{LibraryClass, MethodDef, MethodReceiver, Model, ModelBodyItem};
use crate::expr::{Expr, ExprNode, InterpPart, LValue, Literal};
use crate::ident::ClassId;

/// What becomes of one model-body call of an app class macro.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MacroVerdict {
    /// Everything the macro does at class-body time the lowered class
    /// answers: the call is written into the emitted body.
    Replay,
    /// Its only class-body effect is a setting the lowered model applies
    /// statically: `belongs_to_required_by_default = <bool>` (written by
    /// an extended module's `extended` hook, Core's
    /// FixMeMyModelHasImplicitAssociationValidation, or by the body),
    /// which decides whether the `belongs_to` declarations after it, and
    /// in subclasses, are required ([`replay_model_macros`]).
    BelongsToRequiredByDefault(bool),
    /// The reason it is not, for the diagnostic.
    Refuse(String),
}

/// ActiveRecord::Core's `class_attribute :belongs_to_required_by_default`:
/// read by `belongs_to` when it declares the association.
const REQUIRED_BY_DEFAULT_WRITER: &str = "belongs_to_required_by_default=";

/// Module hooks Ruby calls on the module itself; extending with a module
/// that defines only these gives the class no methods.
const MODULE_HOOKS: &[&str] = &["extended", "included", "inherited", "prepended", "extend_object"];

/// Model DSL the lowered model declares statically. A macro that reaches
/// one at class-body time cannot be replayed: the runtime base defines
/// none of these on the class side.
const MODEL_DSL: &[&str] = &[
    "accepts_nested_attributes_for",
    "alias_attribute",
    "attr_readonly",
    "attribute",
    "belongs_to",
    "composed_of",
    "default_scope",
    "delegated_type",
    "encrypts",
    "enum",
    "generates_token_for",
    "has_and_belongs_to_many",
    "has_many",
    "has_many_attached",
    "has_one",
    "has_one_attached",
    "has_rich_text",
    "has_secure_password",
    "has_secure_token",
    "ignored_columns=",
    "inheritance_column=",
    "normalizes",
    "primary_key=",
    "query_constraints",
    "scope",
    "serialize",
    "store",
    "store_accessor",
    "table_name=",
    "validate",
    "validates",
    "validates_associated",
    "validates_with",
];

/// `before_save`, `after_commit`, `around_destroy`, `after_create_commit`, ...
fn is_lifecycle_callback(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("before_")
        .or_else(|| name.strip_prefix("after_"))
        .or_else(|| name.strip_prefix("around_"))
    else {
        return false;
    };
    matches!(
        rest,
        "validation"
            | "save"
            | "create"
            | "update"
            | "destroy"
            | "commit"
            | "rollback"
            | "initialize"
            | "find"
            | "touch"
            | "create_commit"
            | "update_commit"
            | "destroy_commit"
            | "save_commit"
    )
}

/// Model DSL the Ruby-family runtime base answers on the class side at
/// class-body time, so a macro that reaches it can be replayed.
///
/// `attribute` and `query_constraints`: the runtime's class-side
/// `Base.attribute` retypes a column the model compiled with the path
/// that consults a declared type (`Model::retypable_attributes`), and
/// raises with the name for any other; `Base.query_constraints` takes
/// the model's compiled list (`Model::query_constraints`);
/// `Base.alias_attribute` checks the alias against the compiled ones
/// (`Model::attribute_aliases`, which counts a macro's top-level
/// `alias_attribute` statements over its parameters). Ingest records
/// the macro calls whose literal columns those answer
/// (`Model::claimed_body_calls`, `analyze::retypable_attributes`); for
/// such a call (`answered`, decided per call in [`ClassSide::verdict`])
/// the walk lets both through and judges the rest of what the macro does.
fn runtime_answers_at_class_body(name: &str, answered: bool) -> bool {
    match name {
        "attribute" | "query_constraints" | "alias_attribute" => answered,
        _ => false,
    }
}

/// What a lowered model with a compiled table answers about it on the
/// class side, as Active Record does from the first class-body
/// statement: the synthesized `def self.table_name` / `_table_sql`
/// (`schema::push_schema_methods`), and `primary_key` (synthesized when
/// the model names one, else the runtime base's `"id"`). A replayed
/// macro call is a class-body statement, which the Ruby family writes
/// below the class's methods (`collect_class_attr_initializers`), so a
/// macro reading these finds them: Core's `database_documentation`-style
/// macros and `record_table`-style helpers that read `table_name`.
const SCHEMA_CLASS_METHODS: &[&str] = &["table_name", "_table_sql", "primary_key"];

/// `validates_presence_of`, `validates_uniqueness_of`, ...
fn is_model_dsl(name: &str) -> bool {
    MODEL_DSL.contains(&name)
        || is_lifecycle_callback(name)
        || (name.starts_with("validates_") && name.ends_with("_of"))
}

/// Class-side methods every lowered model answers at class-body time:
/// Ruby's Module/Class/Kernel surface, and what the Ruby-family runtime's
/// core extensions add (`Module#delegate`, `Object#present?`).
const CLASS_SIDE_BUILTINS: &[&str] = &[
    "!", "!=", "<", "<=", "<=>", "==", "===", ">", ">=", "Array", "Float", "Hash", "Integer", "String",
    "__method__", "alias_method", "ancestors", "attr", "attr_accessor", "attr_reader", "attr_writer",
    "binding", "blank?", "block_given?", "caller", "caller_locations", "catch", "class", "class_eval",
    "class_exec", "class_variable_defined?", "class_variable_get", "class_variable_set", "clone",
    "const_defined?", "const_get", "const_set", "constants", "define_method", "define_singleton_method",
    "delegate", "dup", "equal?", "extend", "fail", "format", "freeze", "frozen?", "hash", "include",
    "include?", "inspect", "instance_eval", "instance_exec", "instance_method", "instance_methods",
    "instance_of?", "instance_variable_defined?", "instance_variable_get", "instance_variable_set",
    "instance_variables", "is_a?", "itself", "kind_of?", "lambda", "loop", "method", "method_defined?",
    "methods", "module_eval", "module_exec", "name", "nil?", "object_id", "p", "prepend", "presence",
    "present?", "private", "private_class_method", "private_constant", "private_instance_methods",
    "private_method_defined?", "proc", "protected", "protected_method_defined?", "public",
    "public_class_method", "public_constant", "public_instance_method", "public_instance_methods",
    "public_method", "public_method_defined?", "puts", "raise", "remove_instance_variable",
    "remove_method", "require", "require_relative", "respond_to?", "singleton_class", "singleton_class?",
    "singleton_methods", "sprintf", "superclass", "tap", "then", "throw", "to_s", "undef_method", "warn",
    "yield_self",
];

/// Blocks that run later, or with another `self`: their bodies are not
/// class-body effects of the macro.
const DEFERRED_BLOCK_METHODS: &[&str] = &["define_method", "define_singleton_method", "lambda", "proc"];


/// The class side a model-body call is answered from: the class's own
/// class methods, its app model ancestors' (nearest first), and the
/// instance methods of the app modules its body (or an ancestor's)
/// extends.
pub(crate) struct ClassSide<'a> {
    /// Whose body the calls are (for diagnostics and constant lookup).
    pub owner: &'a ClassId,
    app: &'a crate::App,
    own: Vec<&'a MethodDef>,
    chain: Vec<&'a Model>,
    extended: Vec<&'a LibraryClass>,
    /// The body calls whose `attribute`/`query_constraints` the model's
    /// compiled columns answer (`Model::claimed_body_calls`).
    claimed: &'a [crate::span::Span],
    /// The body calls whose `belongs_to`/`has_one`/`has_many` the model
    /// compiled as an association (`ingest::macro_associations`), which
    /// the class answers with its reflection at class-body run time.
    declared: Vec<crate::span::Span>,
    /// The class-method modules every model inherits from
    /// ActiveRecord::Base's boot includes, with the include
    /// ([`base_include_class_modules`]); read when a call is not
    /// answered nearer.
    base: std::cell::OnceCell<Vec<(&'a LibraryClass, &'a LibraryClass)>>,
    /// Whether the class, or a model ancestor, has a compiled table, so
    /// it answers [`SCHEMA_CLASS_METHODS`].
    schema: bool,
}

/// The body statements of a class, whatever the pass has made of them.
fn body_statements(model: &Model) -> impl Iterator<Item = &Expr> {
    model.body.iter().filter_map(|item| match item {
        ModelBodyItem::Unknown { expr, .. } | ModelBodyItem::ClassBody { expr, .. } => Some(expr),
        _ => None,
    })
}

/// `extend M, ...` (bare or `self.`): its argument expressions.
fn extend_args(call: &Expr) -> Option<&[Expr]> {
    let ExprNode::Send { recv, method, args, block: None, .. } = &*call.node else { return None };
    (method.as_str() == "extend" && !args.is_empty() && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)))
        .then_some(args.as_slice())
}

/// `self.belongs_to_required_by_default = true|false` as a statement.
fn required_by_default_write(expr: &Expr) -> Option<bool> {
    let (recv, name, value) = match &*expr.node {
        ExprNode::Assign { target: LValue::Attr { recv, name }, value } => (recv, name, value),
        ExprNode::Send { recv: Some(recv), method, args, block: None, .. } if args.len() == 1 => (recv, method, &args[0]),
        _ => return None,
    };
    if !matches!(&*recv.node, ExprNode::SelfRef) || format!("{}=", name.as_str().trim_end_matches('=')) != REQUIRED_BY_DEFAULT_WRITER {
        return None;
    }
    match &*value.node {
        ExprNode::Lit { value: Literal::Bool { value } } => Some(*value),
        _ => None,
    }
}

/// Does module `m` run `extend self` (so its instance methods, an
/// `extended` hook among them, are its own singleton methods)?
fn extends_self(m: &LibraryClass) -> bool {
    m.unknown_calls.iter().any(|c| extend_args(c).is_some_and(|args| matches!(args, [a] if matches!(&*a.node, ExprNode::SelfRef))))
}

/// Does `super` in module `m`'s `extended` hook reach only Ruby's
/// `Module#extended`, a no-op? It does while the module's singleton
/// extends nothing but itself and sorbet's markers.
fn hook_super_is_module_default(m: &LibraryClass) -> bool {
    m.unknown_calls.iter().filter_map(|c| extend_args(c)).flatten().all(|a| match &*a.node {
        ExprNode::SelfRef => true,
        ExprNode::Const { path } => {
            let written = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::");
            matches!(written.as_str(), "T::Sig" | "T::Helpers" | "T::Generic")
        }
        _ => false,
    })
}

/// Module `m`'s `self.extended(base)` hook.
fn extended_hook(m: &LibraryClass) -> Option<&MethodDef> {
    let named = |d: &&MethodDef| d.name.as_str() == "extended";
    m.methods.iter().filter(|d| d.receiver == MethodReceiver::Class).find(named).or_else(|| {
        extends_self(m).then(|| m.methods.iter().filter(|d| d.receiver == MethodReceiver::Instance).find(named)).flatten()
    })
}

impl<'a> ClassSide<'a> {
    /// The class side of `model`.
    pub(crate) fn of_model(model: &'a Model, app: &'a crate::App) -> Self {
        let own: Vec<&MethodDef> = model.methods().filter(|d| d.receiver == MethodReceiver::Class).collect();
        let mut side = Self::new(&model.name, own, body_statements(model).collect(), model.parent.as_ref(), app);
        side.claimed = &model.claimed_body_calls;
        side.declared = super::macro_associations::declaring_calls(model);
        side.schema |= app.schema.tables.contains_key(&model.table.0);
        side
    }

    /// The class side of a class whose own class methods are `own`, whose
    /// body statements are `body` and whose superclass is `parent` (an
    /// STI subclass passes its own).
    pub(crate) fn new(
        owner: &'a ClassId,
        own: Vec<&'a MethodDef>,
        body: Vec<&'a Expr>,
        parent: Option<&'a ClassId>,
        app: &'a crate::App,
    ) -> Self {
        let mut chain: Vec<&Model> = Vec::new();
        let mut next = parent;
        while let Some(name) = next {
            match app.models.iter().find(|m| &m.name == name) {
                Some(model) if !chain.iter().any(|seen| seen.name == model.name) => {
                    chain.push(model);
                    next = model.parent.as_ref();
                }
                _ => break,
            }
        }
        let schema = chain.iter().any(|m| app.schema.tables.contains_key(&m.table.0));
        let mut side = Self {
            owner,
            app,
            own,
            chain,
            extended: Vec::new(),
            claimed: &[],
            declared: Vec::new(),
            base: std::cell::OnceCell::new(),
            schema,
        };
        let mut extended = Vec::new();
        let ancestor_bodies: Vec<&Expr> = side.chain.iter().flat_map(|m| body_statements(m)).collect();
        for call in body.into_iter().chain(ancestor_bodies) {
            for arg in extend_args(call).unwrap_or_default() {
                if let Some(m) = side.module_named(arg) {
                    extended.push(m);
                }
            }
        }
        side.extended = extended;
        side
    }

    /// The app module (not a gem's) a constant names, as Ruby resolves it
    /// from the owner's namespace outwards.
    fn module_named(&self, constant: &Expr) -> Option<&'a LibraryClass> {
        let ExprNode::Const { path } = &*constant.node else { return None };
        let rooted = path.first().is_some_and(|s| s.as_str().is_empty());
        let written = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::");
        let written = written.trim_start_matches("::").to_string();
        let find = |name: &str| {
            self.app.library_classes.iter().find(|c| c.is_module && c.origin.is_none() && c.name.0.as_str() == name)
        };
        if rooted {
            return find(&written);
        }
        let scope: Vec<&str> = self.owner.0.as_str().split("::").collect();
        (0..scope.len()).rev().find_map(|n| {
            if n == 0 {
                find(&written)
            } else {
                find(&format!("{}::{written}", scope[..n].join("::")))
            }
        })
    }

    /// Module `m`'s instance methods, and those of the app modules it includes.
    fn module_method(&self, m: &'a LibraryClass, name: &str, seen: &mut Vec<&'a ClassId>) -> Option<&'a MethodDef> {
        if seen.contains(&&m.name) {
            return None;
        }
        seen.push(&m.name);
        m.methods.iter().filter(|d| d.receiver == MethodReceiver::Instance && d.name.as_str() == name).last().or_else(|| {
            m.includes.iter().find_map(|inc| {
                let inc = inc.0.as_str().trim_start_matches("::");
                let included = self.app.library_classes.iter().find(|c| c.is_module && c.origin.is_none() && c.name.0.as_str() == inc)?;
                self.module_method(included, name, seen)
            })
        })
    }

    /// The class macro a body call named `name` runs, wherever it is
    /// defined.
    pub(crate) fn macro_def(&self, name: &str) -> Option<&'a MethodDef> {
        if is_model_dsl(name) {
            return None;
        }
        self.class_method(name).map(|(def, _)| def)
    }

    /// The nearest definition of class method `name`, and the extended
    /// module that gives it, if one does.
    fn class_method(&self, name: &str) -> Option<(&'a MethodDef, Option<&'a LibraryClass>)> {
        self.own
            .iter()
            .rev()
            .find(|d| d.name.as_str() == name)
            .copied()
            .or_else(|| {
                self.chain.iter().find_map(|m| {
                    m.methods().filter(|d| d.receiver == MethodReceiver::Class && d.name.as_str() == name).last()
                })
            })
            .map(|d| (d, None))
            .or_else(|| {
                self.extended.iter().find_map(|m| self.module_method(m, name, &mut Vec::new()).map(|d| (d, Some(*m))))
            })
            .or_else(|| {
                self.base_modules().iter().find_map(|(m, _)| self.module_method(m, name, &mut Vec::new()).map(|d| (d, Some(*m))))
            })
    }

    /// Class-side readers the lowered model synthesizes from its schema
    /// table (`schema::push_schema_methods`, [`SCHEMA_CLASS_METHODS`]),
    /// which a replayed call reaches as ActiveRecord's:
    /// database_documentation's macro builds its
    /// `ModelAnnotation.new(table_name: table_name, ...)`. Answered when
    /// the class or a model ancestor has a compiled table.
    fn synthesized(&self, name: &str) -> bool {
        SCHEMA_CLASS_METHODS.contains(&name)
            && (self.schema
                || std::iter::once(self.owner)
                    .chain(self.chain.iter().map(|m| &m.name))
                    .filter_map(|n| self.app.models.iter().find(|m| &m.name == n))
                    .any(|m| self.app.schema.tables.contains_key(&m.table.0)))
    }

    fn base_modules(&self) -> &[(&'a LibraryClass, &'a LibraryClass)] {
        self.base.get_or_init(|| base_include_class_modules(self.app))
    }

    /// The boot include of ActiveRecord::Base that gives class-method
    /// module `m` to every model, if one does.
    fn base_include_of(&self, m: &LibraryClass) -> Option<&'a LibraryClass> {
        self.base_modules().iter().find(|(provider, _)| provider.name == m.name).map(|(_, include)| *include)
    }

    /// The verdict on `call`, a body statement, when it is a bare or
    /// `self.` call of an app class macro, an `extend` of app modules, or
    /// a `belongs_to_required_by_default` write; `None` otherwise.
    pub(crate) fn verdict(&self, call: &Expr) -> Option<MacroVerdict> {
        if let Some(value) = required_by_default_write(call) {
            return Some(MacroVerdict::BelongsToRequiredByDefault(value));
        }
        if let Some(args) = extend_args(call) {
            let modules: Option<Vec<&LibraryClass>> = args.iter().map(|a| self.module_named(a)).collect();
            let mut verdict = MacroVerdict::Replay;
            for m in modules? {
                match self.extend_verdict(m, &mut Vec::new()) {
                    MacroVerdict::Replay => {}
                    other => verdict = other,
                }
                if matches!(verdict, MacroVerdict::Refuse(_)) {
                    break;
                }
            }
            return Some(verdict);
        }
        let ExprNode::Send { recv, method, args, block, .. } = &*call.node else { return None };
        if !recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)) || is_model_dsl(method.as_str()) {
            return None;
        }
        let (def, module) = self.class_method(method.as_str())?;
        let mut walk = Walk::new(self, method.as_str());
        walk.answered = self.claimed.contains(&call.span);
        walk.declares_association = self.declared.contains(&call.span);
        let result = (|| {
            if let Some(m) = module {
                walk.module_extended(m, method.as_str())?;
            }
            // The call's own arguments run in the class body too.
            let mut aliases = Vec::new();
            for arg in args.iter().chain(block.iter()) {
                walk.expr(arg, &mut aliases)?;
            }
            walk.expr(&def.body, &mut Vec::new())
        })();
        Some(match result {
            Ok(()) => MacroVerdict::Replay,
            Err(reason) => MacroVerdict::Refuse(reason),
        })
    }

    /// What `extend m` does: its `extended` hook's class-body effects.
    fn extend_verdict(&self, m: &'a LibraryClass, in_progress: &mut Vec<&'a ClassId>) -> MacroVerdict {
        let Some(hook) = extended_hook(m) else { return MacroVerdict::Replay };
        if in_progress.contains(&&m.name) {
            return MacroVerdict::Replay;
        }
        in_progress.push(&m.name);
        let label = format!("{}.extended", m.name.0.as_str());
        let mut walk = Walk::new(self, &label);
        walk.allow_static = true;
        walk.default_super = hook_super_is_module_default(m);
        walk.in_progress = in_progress.clone();
        let mut aliases: Vec<String> = hook.params.first().map(|p| p.name.as_str().to_string()).into_iter().collect();
        let result = walk.expr(&hook.body, &mut aliases);
        in_progress.pop();
        if let Err(reason) = result {
            return MacroVerdict::Refuse(format!("`extend {}`: {reason}", m.name.0.as_str()));
        }
        let Some(&value) = walk.statics.last() else { return MacroVerdict::Replay };
        // The hook's write is applied statically, so the `extend` is not
        // replayed; that is faithful only while the module gives the class
        // nothing else.
        let gives = m.methods.iter().any(|d| d.receiver == MethodReceiver::Instance && !MODULE_HOOKS.contains(&d.name.as_str()))
            || !m.includes.is_empty();
        if gives {
            return MacroVerdict::Refuse(format!(
                "`{label}` writes `belongs_to_required_by_default`, which the lowered model applies statically, and `{}` also gives the class methods, which are not replayed without the hook",
                m.name.0.as_str()
            ));
        }
        MacroVerdict::BelongsToRequiredByDefault(value)
    }
}

/// The class-method modules every model inherits from ActiveRecord::Base's
/// boot includes (`App::framework_base_includes`), each with its include,
/// in include order: what the include's `self.included(base)` hook
/// extends onto the base (`base.extend(ClassMethods)`, shopify-money's
/// MoneyColumn::ActiveRecordHooks), and an ActiveSupport::Concern's
/// `ClassMethods`. A constant resolves from the include's namespace
/// outwards; gem modules count, as the tree carries their source.
pub(crate) fn base_include_class_modules(app: &crate::App) -> Vec<(&LibraryClass, &LibraryClass)> {
    if app.framework_base_includes.is_empty() {
        return Vec::new();
    }
    let includes: Vec<&LibraryClass> = app
        .framework_base_includes
        .iter()
        .filter_map(|id| app.library_classes.iter().find(|c| c.is_module && &c.name == id))
        .collect();
    // Per include, the candidate names of each module it gives, nearest
    // scope first.
    let mut wanted: Vec<(usize, Vec<String>)> = Vec::new();
    for (i, m) in includes.iter().enumerate() {
        let scope: Vec<&str> = m.name.0.as_str().split("::").collect();
        let candidates = |written: &str| -> Vec<String> {
            if let Some(rooted) = written.strip_prefix("::") {
                return vec![rooted.to_string()];
            }
            (0..=scope.len()).rev().map(|n| if n == 0 { written.to_string() } else { format!("{}::{written}", scope[..n].join("::")) }).collect()
        };
        for written in included_hook_extends(m) {
            wanted.push((i, candidates(&written)));
        }
        let concern = m.unknown_calls.iter().any(|c| {
            extend_args(c).is_some_and(|args| {
                args.iter().any(|a| matches!(&*a.node, ExprNode::Const { path } if path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).eq(["ActiveSupport", "Concern"])))
            })
        });
        if concern {
            wanted.push((i, vec![format!("{}::ClassMethods", m.name.0.as_str())]));
        }
    }
    let mut out: Vec<(&LibraryClass, &LibraryClass)> = Vec::new();
    for (i, names) in wanted {
        let found = names.iter().find_map(|n| app.library_classes.iter().find(|c| c.is_module && c.name.0.as_str() == n));
        if let Some(provider) = found {
            if !out.iter().any(|(p, _)| p.name == provider.name) {
                out.push((provider, includes[i]));
            }
        }
    }
    out
}

/// The modules `m`'s `self.included(base)` hook extends onto `base`, as
/// written.
fn included_hook_extends(m: &LibraryClass) -> Vec<String> {
    let Some(hook) = m.methods.iter().find(|d| d.receiver == MethodReceiver::Class && d.name.as_str() == "included") else {
        return Vec::new();
    };
    let Some(base) = hook.params.first().map(|p| p.name.as_str().to_string()) else { return Vec::new() };
    let mut out = Vec::new();
    fn walk(e: &Expr, base: &str, out: &mut Vec<String>) {
        if let ExprNode::Send { recv: Some(r), method, args, .. } = &*e.node {
            if method.as_str() == "extend" && matches!(&*r.node, ExprNode::Var { name, .. } if name.as_str() == base) {
                for a in args {
                    if let ExprNode::Const { path } = &*a.node {
                        let rooted = path.first().is_some_and(|s| s.as_str().is_empty());
                        let joined = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::");
                        out.push(if rooted { format!("::{joined}") } else { joined });
                    }
                }
                return;
            }
        }
        e.node.for_each_child(&mut |c| walk(c, base, out));
    }
    walk(&hook.body, &base, &mut out);
    out
}

/// Ruby's own instance methods Ruby itself calls (`dup` runs
/// `initialize_dup`): an include's override of one runs on the emitted
/// records as it does natively.
const RUBY_OBJECT_HOOKS: &[&str] = &[
    "initialize_dup", "initialize_clone", "initialize_copy", "inspect", "to_s", "==", "eql?", "hash",
    "respond_to_missing?", "method_missing", "freeze", "dup", "clone", "instance_variables",
];

/// The runtime base's own instance methods (runtime/ruby/active_record/base.rb).
fn runtime_base_methods() -> &'static crate::det::HashSet<String> {
    static METHODS: std::sync::OnceLock<crate::det::HashSet<String>> = std::sync::OnceLock::new();
    METHODS.get_or_init(|| {
        crate::runtime_src::parse_methods(include_str!("../../../runtime/ruby/active_record/base.rb"))
            .expect("framework Base method bodies must ingest")
            .into_iter()
            .filter(|m| m.receiver == MethodReceiver::Instance)
            .map(|m| m.name.as_str().to_string())
            .collect()
    })
}

/// The PostgreSQL CRuby tree's ActiveRecord::Base ancestry
/// (runtime/spinel/cruby/active_record_core.rb and
/// active_record_active_model.rb, which `project::apply_postgres_bootstrap`
/// carries): the instance methods of the modules Base includes there, and
/// the runtime base's own methods it removes from Base so the modules'
/// answer.
struct PostgresBase {
    module_methods: crate::det::HashSet<String>,
    removed: crate::det::HashSet<String>,
}

const POSTGRES_BASE_FILES: [(&str, &str); 2] = [
    ("runtime/spinel/cruby/active_record_core.rb", include_str!("../../../runtime/spinel/cruby/active_record_core.rb")),
    (
        "runtime/spinel/cruby/active_record_active_model.rb",
        include_str!("../../../runtime/spinel/cruby/active_record_active_model.rb"),
    ),
];

fn postgres_base() -> &'static PostgresBase {
    static BASE: std::sync::OnceLock<PostgresBase> = std::sync::OnceLock::new();
    BASE.get_or_init(|| {
        let mut classes = Vec::new();
        let mut removed = crate::det::HashSet::default();
        for (file, text) in POSTGRES_BASE_FILES {
            classes.extend(
                crate::ingest::ingest_library_classes(text.as_bytes(), file).expect("Active Record Base overlay must ingest"),
            );
            let result = crate::ingest::prism::parse_silent(text.as_bytes());
            collect_remove_method(&result.node(), &mut removed);
        }
        let base = classes.iter().filter(|c| !c.is_module && c.name.0.as_str() == "ActiveRecord::Base");
        let included: Vec<&ClassId> = base.flat_map(|c| c.includes.iter()).collect();
        // A module's own includes come with it (AttributeAssignment's
        // ForbiddenAttributesProtection). A name matches the module it
        // names in full or relative to an enclosing namespace.
        let mut modules: Vec<&LibraryClass> = Vec::new();
        let mut pending: Vec<&ClassId> = included;
        while let Some(name) = pending.pop() {
            let relative = format!("::{}", name.0.as_str());
            for m in classes.iter().filter(|c| c.is_module && (c.name == *name || c.name.0.as_str().ends_with(&relative))) {
                if !modules.iter().any(|n| n.name == m.name) {
                    modules.push(m);
                    pending.extend(m.includes.iter());
                }
            }
        }
        let module_methods = modules
            .iter()
            .flat_map(|m| m.methods.iter())
            .filter(|d| d.receiver == MethodReceiver::Instance)
            .map(|d| d.name.as_str().to_string())
            .collect();
        PostgresBase { module_methods, removed }
    })
}

/// The symbols `remove_method :name` names in `node`.
fn collect_remove_method(node: &ruby_prism::Node<'_>, out: &mut crate::det::HashSet<String>) {
    struct V<'o> {
        out: &'o mut crate::det::HashSet<String>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for V<'_> {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if node.receiver().is_none() && node.name().as_slice() == b"remove_method" {
                for arg in node.arguments().iter().flat_map(|a| a.arguments().iter()) {
                    if let Some(sym) = arg.as_symbol_node() {
                        self.out.insert(String::from_utf8_lossy(sym.unescaped()).into_owned());
                    }
                }
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    ruby_prism::Visit::visit(&mut V { out }, node);
}

/// Framework methods the PostgreSQL CRuby tree's Base ancestry defines
/// in an overridable module but does not run on every path Rails does,
/// with what an override misses.
const PARTIAL_BASE_HOOKS: &[(&str, &str)] = &[
    (
        "initialize",
        "records loaded from the database are built through `new`, so the override runs for them where Rails' `instantiate` skips it",
    ),
];

/// What a boot include of ActiveRecord::Base overrides that the runtime
/// base does not run: each instance method of `include` that calls
/// `super` (so it wraps a framework method) and that the runtime base
/// either defines in the class itself, ahead of any included module, so
/// the override never runs, or neither defines nor calls, so nothing
/// runs it and its `super` has no target. Ruby's own hooks
/// ([`RUBY_OBJECT_HOOKS`]) run as natively. On a PostgreSQL app, whose
/// CRuby tree gives Base Rails' module ancestry for some of them
/// ([`PostgresBase`]), an override of one of those modules' methods runs
/// as in Rails, short of what [`PARTIAL_BASE_HOOKS`] names.
pub(crate) fn base_include_gaps(include: &LibraryClass, postgresql: bool) -> Vec<String> {
    fn calls_super(e: &Expr) -> bool {
        if matches!(&*e.node, ExprNode::Super { .. }) {
            return true;
        }
        let mut found = false;
        e.node.for_each_child(&mut |c| found = found || calls_super(c));
        found
    }
    let base = runtime_base_methods();
    let overlay = postgresql.then(postgres_base);
    let (mut shadowed, mut never, mut partial) = (Vec::new(), Vec::new(), Vec::new());
    for def in include.methods.iter().filter(|d| d.receiver == MethodReceiver::Instance && calls_super(&d.body)) {
        let name = def.name.as_str();
        if RUBY_OBJECT_HOOKS.contains(&name) {
            continue;
        }
        let in_class = base.contains(name) && !overlay.is_some_and(|o| o.removed.contains(name));
        let in_module = overlay.is_some_and(|o| o.module_methods.contains(name));
        let list = if in_class {
            &mut shadowed
        } else if in_module {
            match PARTIAL_BASE_HOOKS.iter().find(|(n, _)| *n == name) {
                Some((_, missing)) => {
                    let entry = format!("`{name}` ({missing})");
                    if !partial.contains(&entry) {
                        partial.push(entry);
                    }
                }
                None => {}
            }
            continue;
        } else {
            &mut never
        };
        if !list.iter().any(|n: &String| n == name) {
            list.push(name.to_string());
        }
    }
    let quote = |names: &[String]| names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
    let mut out = Vec::new();
    if !never.is_empty() {
        out.push(format!("{} (the runtime base neither defines nor calls them, so they never run)", quote(&never)));
    }
    if !shadowed.is_empty() {
        out.push(format!(
            "{} (the runtime base defines its own in the class, ahead of an included module, so the override never runs)",
            quote(&shadowed)
        ));
    }
    out.extend(partial);
    out
}

struct Walk<'s, 'a> {
    side: &'s ClassSide<'a>,
    /// Macros already entered (recursion stops there).
    seen: Vec<String>,
    /// The macros the walk went through to reach the current one.
    path: Vec<String>,
    /// An `extended` hook's `belongs_to_required_by_default` write is
    /// applied statically rather than refused.
    allow_static: bool,
    /// The walked `extended` hook's own `super` is Ruby's no-op
    /// `Module#extended` ([`hook_super_is_module_default`]).
    default_super: bool,
    statics: Vec<bool>,
    /// Modules whose `extended` hook is being walked.
    in_progress: Vec<&'a ClassId>,
    /// The macro call is one whose `attribute`/`query_constraints` the
    /// model's compiled columns answer.
    answered: bool,
    /// The macro call is one whose `belongs_to` the model compiled.
    declares_association: bool,
}

impl<'s, 'a> Walk<'s, 'a> {
    fn new(side: &'s ClassSide<'a>, root: &str) -> Self {
        Walk {
            side,
            seen: vec![root.to_string()],
            path: vec![root.to_string()],
            allow_static: false,
            default_super: false,
            statics: Vec::new(),
            in_progress: Vec::new(),
            answered: false,
            declares_association: false,
        }
    }

    fn via(&self) -> String {
        self.path.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(" → ")
    }

    /// A macro given by an extended module runs only if the `extend` is
    /// replayed.
    fn module_extended(&mut self, m: &'a LibraryClass, name: &str) -> Result<(), String> {
        if let Some(include) = self.side.base_include_of(m) {
            let postgresql =
                self.side.app.primary_database_adapter() == Some(&crate::database::DatabaseAdapter::Postgresql);
            let gaps = base_include_gaps(include, postgresql);
            if !gaps.is_empty() {
                return Err(format!(
                    "`{name}` comes from `{}`, which `{}` extends onto ActiveRecord::Base when boot includes it there, and the runtime base does not run what that include overrides: {}",
                    m.name.0.as_str(),
                    include.name.0.as_str(),
                    gaps.join("; ")
                ));
            }
        }
        match self.side.extend_verdict(m, &mut self.in_progress.clone()) {
            MacroVerdict::Replay => Ok(()),
            MacroVerdict::Refuse(reason) => Err(format!(
                "`{name}` comes from `{}`, whose extend is not replayed ({reason})",
                m.name.0.as_str()
            )),
            MacroVerdict::BelongsToRequiredByDefault(_) => Err(format!(
                "`{name}` comes from `{}`, whose extend is applied statically, not replayed",
                m.name.0.as_str()
            )),
        }
    }

    /// The class-side call `name` a macro makes on `self`.
    fn self_call(&mut self, name: &str) -> Result<(), String> {
        if runtime_answers_at_class_body(name, self.answered) {
            return Ok(());
        }
        // The compiled class answers these from the association
        // (`macro_associations::push_reflection_methods`).
        if self.declares_association && matches!(name, "belongs_to" | "has_one" | "has_many" | "reflect_on_association") {
            return Ok(());
        }
        if is_model_dsl(name) || name == REQUIRED_BY_DEFAULT_WRITER {
            return Err(format!(
                "{} reaches `{name}`, which the lowered model declares statically from its body, not at class-body run time",
                self.via()
            ));
        }
        if let Some((def, module)) = self.side.class_method(name) {
            if self.seen.iter().any(|s| s == name) {
                return Ok(());
            }
            if let Some(m) = module {
                if !self.in_progress.contains(&&m.name) {
                    self.module_extended(m, name)?;
                }
            }
            self.seen.push(name.to_string());
            self.path.push(name.to_string());
            let walked = self.expr(&def.body, &mut Vec::new());
            self.path.pop();
            return walked;
        }
        if CLASS_SIDE_BUILTINS.contains(&name) || self.side.synthesized(name) {
            return Ok(());
        }
        Err(format!("{} calls `{name}`, which the lowered `{}` does not define", self.via(), self.side.owner.0.as_str()))
    }

    fn expr(&mut self, expr: &Expr, aliases: &mut Vec<String>) -> Result<(), String> {
        match &*expr.node {
            ExprNode::Assign { target: LValue::Var { name, .. }, value } => {
                self.expr(value, aliases)?;
                if self.is_self(value, aliases) {
                    aliases.push(name.as_str().to_string());
                } else {
                    aliases.retain(|a| a != name.as_str());
                }
                Ok(())
            }
            ExprNode::Assign { target: LValue::Attr { recv, name }, value }
            | ExprNode::OpAssign { target: LValue::Attr { recv, name }, value, .. } => {
                self.expr(value, aliases)?;
                if !self.is_self(recv, aliases) {
                    return self.expr(recv, aliases);
                }
                let writer = format!("{}=", name.as_str().trim_end_matches('='));
                if writer == REQUIRED_BY_DEFAULT_WRITER && self.allow_static {
                    if let (ExprNode::Assign { .. }, ExprNode::Lit { value: Literal::Bool { value } }) = (&*expr.node, &*value.node) {
                        self.statics.push(*value);
                        return Ok(());
                    }
                }
                if matches!(&*expr.node, ExprNode::OpAssign { .. }) {
                    self.self_call(name.as_str().trim_end_matches('='))?;
                }
                self.self_call(&writer)
            }
            // Core's TrackCurrent.extended ends in `super`.
            ExprNode::Super { args, .. } if self.default_super && self.path.len() == 1 => {
                for arg in args.iter().flatten() {
                    self.expr(arg, aliases)?;
                }
                Ok(())
            }
            ExprNode::Super { .. } => Err(format!("{} calls `super`", self.via())),
            ExprNode::Lambda { .. } => Ok(()),
            ExprNode::Send { recv, method, args, block, .. } => {
                let on_self = recv.as_ref().is_none_or(|r| self.is_self(r, aliases));
                if let Some(r) = recv {
                    self.expr(r, aliases)?;
                }
                let mut name = method.as_str().to_string();
                if on_self && matches!(name.as_str(), "send" | "public_send" | "__send__") {
                    match args.first().map(|a| &*a.node) {
                        Some(ExprNode::Lit { value: Literal::Sym { value } }) => name = value.as_str().to_string(),
                        Some(ExprNode::Lit { value: Literal::Str { value } }) => name = value.clone(),
                        _ => return Err(format!("{} calls `{name}` with a computed method name", self.via())),
                    }
                }
                if on_self && name == REQUIRED_BY_DEFAULT_WRITER && self.allow_static {
                    if let [arg] = args.as_slice() {
                        if let ExprNode::Lit { value: Literal::Bool { value } } = &*arg.node {
                            self.statics.push(*value);
                            return Ok(());
                        }
                    }
                }
                if on_self {
                    self.self_call(&name)?;
                    if matches!(name.as_str(), "class_eval" | "module_eval" | "instance_eval") {
                        self.eval_strings(args)?;
                    }
                }
                let skip_first = on_self && name != method.as_str();
                for (i, arg) in args.iter().enumerate() {
                    if skip_first && i == 0 {
                        continue;
                    }
                    self.expr(arg, aliases)?;
                }
                if let Some(block) = block {
                    // `mod.send(:define_method, name) do ... end` (Core's
                    // generated-methods modules) defers its block like a
                    // direct `define_method`, whatever the receiver.
                    let sent = match (method.as_str(), args.first().map(|a| &*a.node)) {
                        ("send" | "public_send" | "__send__", Some(ExprNode::Lit { value: Literal::Sym { value } })) => {
                            Some(value.as_str().to_string())
                        }
                        ("send" | "public_send" | "__send__", Some(ExprNode::Lit { value: Literal::Str { value } })) => Some(value.clone()),
                        _ => None,
                    };
                    let name = sent.unwrap_or_else(|| name.clone());
                    // `Module.new do ... end`: another `self`.
                    let new_scope = name == "new"
                        && recv.as_ref().is_some_and(|r| {
                            matches!(&*r.node, ExprNode::Const { path } if matches!(path.last().map(|s| s.as_str()), Some("Module" | "Class" | "Struct" | "Data")))
                        });
                    let changes_self = !on_self
                        && matches!(name.as_str(), "class_eval" | "module_eval" | "instance_eval" | "instance_exec" | "class_exec" | "module_exec");
                    if !(DEFERRED_BLOCK_METHODS.contains(&name.as_str()) || new_scope || changes_self) {
                        if let ExprNode::Lambda { body, .. } = &*block.node {
                            self.expr(body, aliases)?;
                        }
                    }
                }
                Ok(())
            }
            _ => {
                let mut result = Ok(());
                expr.node.for_each_child(&mut |child| {
                    if result.is_ok() {
                        result = self.expr(child, aliases);
                    }
                });
                result
            }
        }
    }

    /// `self`, a local bound to it, or either under an inline cast
    /// (Core's `untyped_self = self #: as untyped`).
    fn is_self(&self, recv: &Expr, aliases: &[String]) -> bool {
        match &*recv.node {
            ExprNode::SelfRef => true,
            ExprNode::Cast { value, .. } => self.is_self(value, aliases),
            ExprNode::Var { name, .. } => aliases.iter().any(|a| a == name.as_str()),
            _ => false,
        }
    }

    /// A `class_eval` string defines methods (Core's `def trusted_#{name}`);
    /// one that writes model DSL into the class body is refused like a
    /// direct call. The string's text is scanned for a DSL name at the
    /// start of a line.
    fn eval_strings(&self, args: &[Expr]) -> Result<(), String> {
        let Some(code) = args.first() else { return Ok(()) };
        let text = match &*code.node {
            ExprNode::Lit { value: Literal::Str { value } } => value.clone(),
            ExprNode::StringInterp { parts } => parts
                .iter()
                .map(|p| match p {
                    InterpPart::Text { value } => value.as_str(),
                    InterpPart::Expr { .. } => " ",
                })
                .collect(),
            _ => return Ok(()),
        };
        for line in text.lines() {
            let word: String = line.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if is_model_dsl(&word) {
                return Err(format!(
                    "{}'s `class_eval` string calls `{word}`, which the lowered model declares statically from its body",
                    self.via()
                ));
            }
        }
        Ok(())
    }
}

/// The verdicts on `model`'s body statements that call app class macros,
/// extend app modules or write `belongs_to_required_by_default`, by item.
pub(crate) fn model_macro_verdicts<'a>(model: &'a Model, app: &'a crate::App) -> Vec<(&'a Expr, MacroVerdict)> {
    let mut callbacks = super::callback_dsl_calls(model, &app.models);
    callbacks.extend(super::gem_port_macro_calls(model, &app.models));
    let side = ClassSide::of_model(model, app);
    model
        .body
        .iter()
        .filter_map(|item| match item {
            ModelBodyItem::Unknown { expr, .. } => Some(expr),
            _ => None,
        })
        .filter(|expr| !callbacks.iter().any(|c| std::ptr::eq(*c, *expr)) && !super::claimed_in_model(model, expr))
        .filter_map(|expr| side.verdict(expr).map(|v| (expr, v)))
        .collect()
}

/// Apply the verdicts: each replayed `Unknown` model-body statement
/// becomes a `ClassBody` statement, which the Ruby-family emitter writes
/// into the class body (`model_class` → `class_ivar_initializers`), and
/// each `belongs_to_required_by_default` setting decides the `belongs_to`
/// declarations after it in the body and in subclasses that name neither
/// `optional:` nor `required:` (`Association::BelongsTo::optional`, from
/// `declared_optional`), as ActiveRecord's `belongs_to` reads the class
/// attribute when it declares the association.
pub fn replay_model_macros(app: &mut crate::App) {
    // A replayed statement belongs to the one model the class emits, and
    // a model's starting `belongs_to_required_by_default` is read from
    // its ancestor by name: one model per class first
    // (`merge_model_reopens`), which also leaves every later pass and the
    // emitter one model to find by name.
    super::merge_model_reopens(app);
    let mut replays: Vec<(usize, Vec<crate::span::Span>)> = Vec::new();
    // Per model, the settings by statement span.
    let mut settings: Vec<Vec<(crate::span::Span, bool)>> = vec![Vec::new(); app.models.len()];
    for (i, model) in app.models.iter().enumerate() {
        let mut spans = Vec::new();
        for (expr, verdict) in model_macro_verdicts(model, app) {
            match verdict {
                MacroVerdict::Replay => spans.push(expr.span),
                MacroVerdict::BelongsToRequiredByDefault(value) => settings[i].push((expr.span, value)),
                MacroVerdict::Refuse(_) => {}
            }
        }
        if !spans.is_empty() {
            replays.push((i, spans));
        }
    }
    for (i, spans) in replays {
        for item in &mut app.models[i].body {
            let ModelBodyItem::Unknown { expr, leading_comments, leading_blank_line } = item else { continue };
            if spans.contains(&expr.span) {
                *item = ModelBodyItem::ClassBody {
                    expr: expr.clone(),
                    leading_comments: std::mem::take(leading_comments),
                    leading_blank_line: *leading_blank_line,
                };
            }
        }
    }
    for model in &mut app.models {
        carry_read_body_locals(&mut model.body);
    }
    if settings.iter().all(Vec::is_empty) {
        return;
    }
    // The value each model's body starts from: its nearest model
    // ancestor's final one (ActiveRecord's default is required).
    let final_value = |i: usize| -> Option<bool> { settings[i].last().map(|(_, v)| *v) };
    let starts: Vec<bool> = (0..app.models.len())
        .map(|i| {
            let mut seen = vec![i];
            let mut parent = app.models[i].parent.clone();
            while let Some(p) = parent {
                let Some(j) = app.models.iter().position(|m| m.name == p) else { break };
                if seen.contains(&j) {
                    break;
                }
                seen.push(j);
                if let Some(v) = final_value(j) {
                    return v;
                }
                parent = app.models[j].parent.clone();
            }
            true
        })
        .collect();
    for (i, model) in app.models.iter_mut().enumerate() {
        let mut required = starts[i];
        for item in &mut model.body {
            match item {
                ModelBodyItem::Unknown { expr, .. } => {
                    if let Some((_, v)) = settings[i].iter().find(|(s, _)| *s == expr.span) {
                        required = *v;
                    }
                }
                ModelBodyItem::Association {
                    assoc: crate::dialect::Association::BelongsTo { optional, declared_optional: None, .. },
                    ..
                } => *optional = !required,
                _ => {}
            }
        }
        // A subclass's own declarations (`lower::subclass_macros`) run
        // after the model's body; a setting in the subclass before one
        // is refused (`sti_macro_verdicts`).
        for (_, items) in &mut model.subclass_declarations {
            for item in items {
                if let ModelBodyItem::Association {
                    assoc: crate::dialect::Association::BelongsTo { optional, declared_optional: None, .. },
                    ..
                } = item
                {
                    *optional = !required;
                }
            }
        }
    }
}

/// A model-body local write (`coder = Serializer.new(...)`) a carried
/// statement reads (`serialize :line_items, coder: coder`) becomes a
/// `ClassBody` statement too, at its source position: Ruby runs the
/// write where it stands, and the read raises NameError at class load
/// without it. Core's Checkout builds its `line_items` coder this way;
/// dropped, the class body raised and Ruby discarded the autoloaded
/// class, with `Checkout::ShippingLine` already loaded on the discarded
/// one. A write only statically lowered declarations read stays
/// `Unknown`, as before.
pub(crate) fn carry_read_body_locals(body: &mut [ModelBodyItem]) {
    fn reads(e: &Expr, out: &mut Vec<crate::ident::Symbol>) {
        if let ExprNode::Var { name, .. } = &*e.node {
            out.push(name.clone());
        }
        e.node.for_each_child(&mut |c| reads(c, out));
    }
    let mut read: Vec<crate::ident::Symbol> = Vec::new();
    for item in body.iter_mut().rev() {
        match item {
            ModelBodyItem::ClassBody { expr, .. } => reads(expr, &mut read),
            ModelBodyItem::Unknown { expr, leading_comments, leading_blank_line } => {
                let written = match &*expr.node {
                    ExprNode::Assign { target: LValue::Var { name, .. }, .. }
                    | ExprNode::OpAssign { target: LValue::Var { name, .. }, .. } => name,
                    _ => continue,
                };
                if !read.contains(written) {
                    continue;
                }
                reads(expr, &mut read);
                *item = ModelBodyItem::ClassBody {
                    expr: expr.clone(),
                    leading_comments: std::mem::take(leading_comments),
                    leading_blank_line: *leading_blank_line,
                };
            }
            _ => {}
        }
    }
}

/// The verdicts on an STI subclass's body calls (a library class whose
/// superclass is a model, which ingest does not lower as one): the same
/// gate, over its own class methods and its model ancestors'. A
/// `belongs_to_required_by_default` setting there is refused when a
/// `belongs_to` follows it in the subclass or appears in one of its
/// subclasses (their declarations are not lowered as a model's). With
/// none, the setting has nothing to apply to (Rails reads it only when
/// `belongs_to` declares an association), and its verdict stays
/// [`MacroVerdict::BelongsToRequiredByDefault`]: neither replayed nor
/// refused.
pub(crate) fn sti_macro_verdicts<'a>(lc: &'a LibraryClass, app: &crate::App) -> Vec<(&'a Expr, MacroVerdict)> {
    if !lc.parent.as_ref().is_some_and(|p| app.models.iter().any(|m| &m.name == p)) {
        return Vec::new();
    }
    let own: Vec<&MethodDef> = lc.methods.iter().filter(|m| m.receiver == MethodReceiver::Class).collect();
    let callbacks = super::callback_calls(lc.unknown_calls.iter(), &own, lc.parent.as_ref(), &app.models);
    let side = ClassSide::new(&lc.name, own, lc.unknown_calls.iter().collect(), lc.parent.as_ref(), app);
    // The parent model's claims cover its STI subclasses' `query_constraints`
    // macro calls naming its list, which the Ruby emit drops (Rails' list
    // falls back to the base class's): neither replayed nor refused.
    let parent_claims = app.models.iter().find(|m| lc.parent.as_ref() == Some(&m.name)).map(|m| m.claimed_body_calls.as_slice()).unwrap_or_default();
    lc.unknown_calls
        .iter()
        .filter(|expr| {
            !callbacks.iter().any(|c| std::ptr::eq(*c, *expr)) && !super::claimed_elsewhere(expr) && !parent_claims.contains(&expr.span)
        })
        .filter_map(|expr| {
            side.verdict(expr).map(|v| match v {
                MacroVerdict::BelongsToRequiredByDefault(value) if !declares_belongs_to_after(lc, expr, app) => {
                    (expr, MacroVerdict::BelongsToRequiredByDefault(value))
                }
                MacroVerdict::BelongsToRequiredByDefault(_) => (
                    expr,
                    MacroVerdict::Refuse(
                        "it sets `belongs_to_required_by_default`, which is applied to a lowered model's `belongs_to` declarations, and an STI subclass's are not lowered as a model's".to_string(),
                    ),
                ),
                v => (expr, v),
            })
        })
        .collect()
}

/// Does a `belongs_to` declaration follow `setting` in STI subclass `lc`'s
/// body, or appear in the body of a library class below `lc`?
fn declares_belongs_to_after(lc: &LibraryClass, setting: &Expr, app: &crate::App) -> bool {
    let is_belongs_to = |c: &Expr| matches!(&*c.node, ExprNode::Send { method, .. } if method.as_str() == "belongs_to");
    if lc.unknown_calls.iter().any(|c| (c.span.file != setting.span.file || c.span.start > setting.span.start) && is_belongs_to(c)) {
        return true;
    }
    let mut below = vec![&lc.name];
    let mut i = 0;
    while i < below.len() {
        let parent = below[i];
        for sub in app.library_classes.iter().filter(|c| c.parent.as_ref() == Some(parent)) {
            if sub.unknown_calls.iter().any(|c| is_belongs_to(c)) {
                return true;
            }
            if !below.contains(&&sub.name) {
                below.push(&sub.name);
            }
        }
        i += 1;
    }
    false
}
