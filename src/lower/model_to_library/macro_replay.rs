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
/// class-body time, so a macro that reaches it can be replayed. None yet.
///
/// `attribute` is the switch for the class-side `Base.attribute` port:
/// once each lowered model carries its retypable columns
/// (`ingest::retypable_attributes`), a call whose column is in the
/// model's set goes through here, and the runtime raises with the
/// column's name for any other.
fn runtime_answers_at_class_body(name: &str, _owner: &ClassId) -> bool {
    match name {
        "attribute" => false,
        _ => false,
    }
}

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
        Self::new(&model.name, own, body_statements(model).collect(), model.parent.as_ref(), app)
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
        let mut side = Self { owner, app, own, chain, extended: Vec::new() };
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

struct Walk<'s, 'a> {
    side: &'s ClassSide<'a>,
    /// Macros already entered (recursion stops there).
    seen: Vec<String>,
    /// The macros the walk went through to reach the current one.
    path: Vec<String>,
    /// An `extended` hook's `belongs_to_required_by_default` write is
    /// applied statically rather than refused.
    allow_static: bool,
    statics: Vec<bool>,
    /// Modules whose `extended` hook is being walked.
    in_progress: Vec<&'a ClassId>,
}

impl<'s, 'a> Walk<'s, 'a> {
    fn new(side: &'s ClassSide<'a>, root: &str) -> Self {
        Walk {
            side,
            seen: vec![root.to_string()],
            path: vec![root.to_string()],
            allow_static: false,
            statics: Vec::new(),
            in_progress: Vec::new(),
        }
    }

    fn via(&self) -> String {
        self.path.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(" → ")
    }

    /// A macro given by an extended module runs only if the `extend` is
    /// replayed.
    fn module_extended(&mut self, m: &'a LibraryClass, name: &str) -> Result<(), String> {
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
        if (is_model_dsl(name) && !runtime_answers_at_class_body(name, self.side.owner)) || name == REQUIRED_BY_DEFAULT_WRITER {
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
        if CLASS_SIDE_BUILTINS.contains(&name) {
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
    let callbacks = super::callback_dsl_calls(model, &app.models);
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
    lc.unknown_calls
        .iter()
        .filter(|expr| !callbacks.iter().any(|c| std::ptr::eq(*c, *expr)) && !super::claimed_elsewhere(expr))
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
