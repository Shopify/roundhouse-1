//! `class Current < ActiveSupport::CurrentAttributes` → a plain,
//! statically-resolvable class.
//!
//! Rails' CurrentAttributes is three pieces of metaprogramming stacked:
//! `attribute :a, :b` defines instance accessors into a generated module,
//! the class-level `Current.user` is `method_missing` forwarding to a
//! per-thread singleton, and an app's own `def user=` reaches the
//! generated writer through `super`. None of that survives into an
//! emitted tree — the runtime has no `ActiveSupport::CurrentAttributes`,
//! and a target that resolves calls statically cannot follow
//! `method_missing` at all ([[feedback_runtime_must_be_statically_resolvable]]).
//!
//! campfire routes essentially everything through it (`Current.user`,
//! `Current.account`, `Current.session`), so this is load-bearing rather
//! than a nicety: without it the class replays verbatim and every
//! `Current.x` is unresolvable.
//!
//! ## What it becomes
//!
//! * one `@__instance` singleton plus `self.instance` / `self.reset`;
//! * a reader and writer per declared attribute, storing in an ivar;
//! * `delegate … to: :x, prefix: true, allow_nil: true` expanded to real
//!   methods (`request_host` → `@request.nil? ? nil : @request.host`);
//! * a `self.<name>` forwarder for every PUBLIC instance method, which is
//!   what makes the class-level call sites app code actually writes
//!   resolve. activesupport 8.2's `method_added` delegates only public
//!   methods, and only when no class method of that name exists; a
//!   method taking arguments or a block forwards them, its parameters
//!   spelled out (Rails writes `(...)`, which analysis cannot verify);
//! * `before_reset` / `resets` / `after_reset` callbacks (a block or
//!   method names) run by `self.reset` as Rails' `run_callbacks(:reset)`
//!   runs them: the before ones in order on the instance being reset,
//!   the after ones in reverse order on the reset one. api_versioning
//!   4.6.0's `ThreadLocalVersionStorage` `resets` its `requested_handle`;
//! * `attribute :x, default: v`: every new instance (so every `reset`)
//!   starts the attribute at its default, as `resolve_defaults` does — a
//!   lambda is called, any other value is `dup`ed. The default is a
//!   generated class method (`self.__rh_default_x`), so a lambda's body
//!   runs with the class as `self`, where Rails' lambda was created;
//! * `set(attributes) { … }`, instance and class level, as
//!   `CurrentAttributes#set` → `Object#with` runs it: for each pair in
//!   order, save the public reader's value then call the public writer;
//!   yield the instance; in an `ensure`, write back exactly the saved
//!   pairs, in order. The attributes are a run-time Hash, so the names are
//!   dispatched by a `case` over the class's reader/writer pairs rather
//!   than `public_send`. `lower::current_set` still spells a literal
//!   keyword hash with a plain block inline; every other call reaches
//!   this method.
//!
//! ## `super` in an app-written writer
//!
//! campfire writes `def session=(value); super(value); …; end`. Rather
//! than synthesize a module for the app's `super` to find — a mixin is
//! Ruby-family-only, and the strict targets have no equivalent — the
//! `super` call is REWRITTEN to the storage write it means (`@session =
//! value`). Same reasoning as the concern splices: resolve it in the IR
//! once, for all thirteen targets. A bare `super` (zsuper) forwards the
//! writer's own parameter, so it is the write of that parameter's
//! current value: Core's `Podding::Current#pod_id=` clears two caches and
//! ends in `super`. A `super` in an app-written reader is the storage
//! read (`@session`): api_versioning's
//! `def requested_handle; super || UNSPECIFIED_VERSION; end`.

use crate::dialect::{LibraryClass, MethodDef, MethodReceiver, MethodVisibility};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};

const BASE: &str = "ActiveSupport::CurrentAttributes";

/// Rewrite every `Current`-shaped class in the app.
pub fn lower_current_attributes(app: &mut crate::App) {
    let mut generated: Vec<(usize, Vec<MethodDef>)> = Vec::new();
    let mut lowered: Vec<ClassId> = Vec::new();
    let mut signatures: Vec<(ClassId, Symbol, crate::ty::Ty)> = Vec::new();
    for (i, lc) in app.library_classes.iter_mut().enumerate() {
        if lc.parent.as_ref().map(|p| p.0.as_str()) != Some(BASE) {
            continue;
        }
        let AttributeDecls { names: attrs, declared, defaults } = take_attribute_decls(lc);
        let delegates = take_delegate_decls(lc);
        let callbacks = take_reset_callbacks(lc);
        rewrite_super_writers(lc, &attrs);
        rewrite_super_readers(lc, &attrs);
        let defaults: Vec<(Symbol, DefaultBody)> =
            defaults.into_iter().map(|(name, value)| (name, DefaultBody::of(&value))).collect();
        let authored_initialize = lc
            .methods
            .iter()
            .any(|m| m.receiver == MethodReceiver::Instance && m.name.as_str() == "initialize");
        let src = synthesized_source(lc, &attrs, &delegates, &callbacks, &defaults, authored_initialize);
        // Isolated in its own `prism::scope` — never the outer one
        // that spans the whole app's ingest — so a bug in
        // `synthesized_source` can't render its parse errors against
        // an unrelated real file (see `ingest::sources`'s module doc).
        let (parsed, diags) = crate::ingest::prism::scope(|| {
            crate::ingest::ingest_library_classes(src.as_bytes(), "<current>")
        });
        let methods = match parsed {
            Ok(classes) if diags.is_empty() => {
                let mut methods: Vec<MethodDef> = classes.into_iter().flat_map(|c| c.methods).collect();
                fill_reset_callback_blocks(&mut methods, &callbacks);
                fill_default_bodies(&mut methods, &defaults);
                if authored_initialize {
                    prepend_default_assignments(lc, &defaults);
                }
                for (name, signature) in declare_attribute_types(&mut methods, &declared) {
                    signatures.push((lc.name.clone(), name, signature));
                }
                methods
            }
            Ok(_) => {
                super::survey::record_synthesis_failure(
                    "<current>",
                    &format!("CurrentAttributes forwarder for `{}`", lc.name.0.as_str()),
                    &diags,
                );
                Vec::new()
            }
            Err(err) => {
                super::survey::record(&err);
                Vec::new()
            }
        };
        // The base is gone: nothing is inherited any more. Record the
        // name first — clearing the parent is exactly what makes this
        // class unrecognizable to everything downstream.
        lc.parent = None;
        lowered.push(lc.name.clone());
        generated.push((i, methods));
    }
    for (i, methods) in generated {
        app.library_classes[i].methods.extend(methods);
    }
    // Declared like an inline signature, so the analysis reads the
    // written type in the class's lexical scope (`BuyerContext::…`
    // inside `Customers::CustomerAccount`).
    for (class, name, signature) in signatures {
        app.rbs_signatures.entry(class).or_default().entry(name).or_insert(signature);
    }
    app.current_attribute_classes = lowered;
}

/// The class's `attribute` declarations, in source order.
#[derive(Default)]
struct AttributeDecls {
    names: Vec<Symbol>,
    /// The RBS type a single-name declaration carries.
    declared: Vec<(Symbol, crate::ty::Ty)>,
    /// `default:` per name; every name of one declaration shares it.
    defaults: Vec<(Symbol, Expr)>,
}

/// `attribute :session, :user, :request` — consumed, not replayed.
/// A single attribute may carry its type in a trailing RBS comment
/// (Core's `attribute :buyer_context #: BuyerContext::CustomerAccountImpl`),
/// which ingest reads as a cast around the call: that type is returned
/// with the name. Without unwrapping it the declaration was not taken,
/// the class got no accessor, and `Current.buyer_context` had no method.
///
/// The one option activesupport 8.2 takes is `default:`
/// (`attribute :section, default: -> { NilSectionGlobals }`); it was
/// dropped here once, so `ApplicationSection.current` read nil where
/// Rails answers the null section. A declaration with any other
/// argument shape (a splat, another option) stays in `unknown_calls`,
/// visible, rather than half-declared.
fn take_attribute_decls(lc: &mut LibraryClass) -> AttributeDecls {
    let mut out = AttributeDecls::default();
    lc.unknown_calls.retain(|call| {
        let (call, declared_ty) = match &*call.node {
            ExprNode::Cast { value, target_ty } => (value, Some(target_ty)),
            _ => (call, None),
        };
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*call.node else { return true };
        if method.as_str() != "attribute" {
            return true;
        }
        let mut names: Vec<Symbol> = Vec::new();
        let mut default: Option<Expr> = None;
        for (i, a) in args.iter().enumerate() {
            match &*a.node {
                ExprNode::Lit { value: Literal::Sym { value } } => names.push(value.clone()),
                ExprNode::Lit { value: Literal::Str { value } } => names.push(Symbol::from(value.as_str())),
                ExprNode::Hash { entries, .. } if i + 1 == args.len() => {
                    for (k, v) in entries {
                        match &*k.node {
                            ExprNode::Lit { value: Literal::Sym { value } }
                                if value.as_str() == "default" && default.is_none() =>
                            {
                                default = Some(v.clone())
                            }
                            _ => return true,
                        }
                    }
                }
                _ => return true,
            }
        }
        if names.is_empty() {
            return true;
        }
        if let (Some(ty), [name]) = (declared_ty, names.as_slice()) {
            out.declared.push((name.clone(), ty.clone()));
        }
        if let Some(default) = default {
            out.defaults.extend(names.iter().map(|n| (n.clone(), default.clone())));
        }
        out.names.extend(names);
        false
    });
    out
}

/// What an attribute's generated `self.__rh_default_<attr>` returns:
/// the value activesupport 8.2's `resolve_defaults` stores for it on
/// every new instance and every reset — `Proc === value ? value.call :
/// value.dup`, `value` having been evaluated once, in the class body.
enum DefaultBody {
    /// A literal `dup` returns as is (`nil`, `false`, `0`, `:shared`).
    Immediate(Expr),
    /// Any other literal (`{}`, `[]`, `"x"`): a fresh copy. The literal
    /// is evaluated per instance rather than once, which is the same
    /// value — Rails never hands out the original, only its copies.
    Copy(Expr),
    /// `-> { … }` / `lambda { … }` without parameters: Rails calls it, so
    /// the body (lifted: its `next`/`break`/`return` become the method's
    /// `return`) is the method body.
    Called(Expr),
    /// Anything else (`EphemeralCacheHeader::HTLC_OFF`, `Set.new`): the
    /// value comes from `self.__rh_default_value_<attr>` and the
    /// `Proc ===` test is made at run time, as Rails makes it.
    Dynamic(Expr),
}

impl DefaultBody {
    fn of(value: &Expr) -> DefaultBody {
        use crate::lower::lifted_block::{self, Form};
        let lifted = |body: &Expr| {
            if lifted_block::jumps(body).unliftable(Form::Lambda).is_some() {
                return None;
            }
            let mut body = body.clone();
            lifted_block::to_method_body(&mut body, Form::Lambda);
            Some(body)
        };
        let plain_lambda = |e: &Expr| match &*e.node {
            ExprNode::Lambda { params, rest_param: None, block_param: None, body, signature, .. }
                if params.is_empty() && signature.is_empty() =>
            {
                Some(body.clone())
            }
            _ => None,
        };
        match &*value.node {
            ExprNode::Lit {
                value: Literal::Nil | Literal::Bool { .. } | Literal::Int { .. } | Literal::BigInt { .. } | Literal::Float { .. } | Literal::Sym { .. },
            } => DefaultBody::Immediate(value.clone()),
            ExprNode::Lit { value: Literal::Str { .. } }
            | ExprNode::StringInterp { .. }
            | ExprNode::Hash { .. }
            | ExprNode::Array { .. } => DefaultBody::Copy(value.clone()),
            ExprNode::Lambda { .. } => match plain_lambda(value).as_ref().and_then(lifted) {
                Some(body) => DefaultBody::Called(body),
                None => DefaultBody::Dynamic(value.clone()),
            },
            ExprNode::Send { recv: None, method, args, block: Some(block), .. }
                if method.as_str() == "lambda" && args.is_empty() =>
            {
                match plain_lambda(block).as_ref().and_then(lifted) {
                    Some(body) => DefaultBody::Called(body),
                    None => DefaultBody::Dynamic(value.clone()),
                }
            }
            _ => DefaultBody::Dynamic(value.clone()),
        }
    }
}

/// The generated class method answering an attribute's default.
fn default_method(attr: &str) -> String {
    format!("__rh_default_{attr}")
}

/// The generated class method holding a [`DefaultBody::Dynamic`] value.
fn default_value_method(attr: &str) -> String {
    format!("__rh_default_value_{attr}")
}

/// The generated default methods get the declarations' expressions.
fn fill_default_bodies(methods: &mut [MethodDef], defaults: &[(Symbol, DefaultBody)]) {
    for (attr, default) in defaults {
        let (name, body) = match default {
            DefaultBody::Immediate(value) | DefaultBody::Called(value) => (default_method(attr.as_str()), value.clone()),
            DefaultBody::Copy(value) => {
                let span = value.span;
                let dup = ExprNode::Send {
                    recv: Some(value.clone()),
                    method: Symbol::from("dup"),
                    args: Vec::new(),
                    block: None,
                    parenthesized: false,
                };
                (default_method(attr.as_str()), Expr::new(span, dup))
            }
            DefaultBody::Dynamic(value) => (default_value_method(attr.as_str()), value.clone()),
        };
        if let Some(m) = methods.iter_mut().find(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == name) {
            m.body = body;
        }
    }
}

/// An app-written `initialize` starts with the default assignments the
/// synthesized one would make: activesupport's `initialize`, which the
/// app's reaches through `super`, is where `@attributes` gets them.
fn prepend_default_assignments(lc: &mut LibraryClass, defaults: &[(Symbol, DefaultBody)]) {
    if defaults.is_empty() {
        return;
    }
    let class: Vec<Symbol> = lc.name.0.as_str().split("::").map(Symbol::from).collect();
    let Some(init) = lc
        .methods
        .iter_mut()
        .find(|m| m.receiver == MethodReceiver::Instance && m.name.as_str() == "initialize")
    else {
        return;
    };
    let span = init.body.span;
    let mut exprs: Vec<Expr> = defaults
        .iter()
        .map(|(attr, _)| {
            let read = ExprNode::Send {
                recv: Some(Expr::new(span, ExprNode::Const { path: class.clone() })),
                method: Symbol::from(default_method(attr.as_str())),
                args: Vec::new(),
                block: None,
                parenthesized: false,
            };
            Expr::new(
                span,
                ExprNode::Assign {
                    target: crate::expr::LValue::Ivar { name: attr.clone() },
                    value: Expr::new(span, read),
                },
            )
        })
        .collect();
    match &*init.body.node {
        ExprNode::Seq { exprs: rest } => exprs.extend(rest.iter().cloned()),
        _ => exprs.push(init.body.clone()),
    }
    init.body = Expr::new(span, ExprNode::Seq { exprs });
}

/// The synthesized reader answers, and the writer takes, an
/// attribute's declared type, nilable: every attribute reads nil until
/// the request sets it (and after `reset`), and Core asks exactly that
/// (`create_buyer_context unless Current.buyer_context`). Read as written,
/// the presence test would fold to true (see current_attributes_typing).
/// Returns each accessor's signature.
fn declare_attribute_types(methods: &mut [MethodDef], declared: &[(Symbol, crate::ty::Ty)]) -> Vec<(Symbol, crate::ty::Ty)> {
    use crate::ty::{Param, ParamKind, Ty};
    let mut out = Vec::new();
    for (name, written) in declared {
        let ty = &match written {
            Ty::Nil => Ty::Nil,
            Ty::Union { variants } if variants.contains(&Ty::Nil) => written.clone(),
            _ => Ty::Union { variants: vec![written.clone(), Ty::Nil] },
        };
        let writer = format!("{}=", name.as_str());
        for method in methods.iter_mut().filter(|m| m.receiver == MethodReceiver::Instance) {
            let params = if method.name == *name {
                vec![]
            } else if method.name.as_str() == writer {
                vec![Param { name: Symbol::from("value"), ty: ty.clone(), kind: ParamKind::Required }]
            } else {
                continue;
            };
            let signature = Ty::Fn { params, block: None, ret: Box::new(ty.clone()), effects: method.effects.clone() };
            method.signature = Some(signature.clone());
            out.push((method.name.clone(), signature));
        }
    }
    out
}

/// `delegate :host, :protocol, to: :request, prefix: true, allow_nil: true`.
/// Returns `(method, target, generated_name, allow_nil)`.
struct Delegation {
    method: Symbol,
    target: Symbol,
    name: String,
    allow_nil: bool,
}

fn take_delegate_decls(lc: &mut LibraryClass) -> Vec<Delegation> {
    let mut out = Vec::new();
    lc.unknown_calls.retain(|call| {
        let ExprNode::Send { recv: None, method, args, .. } = &*call.node else { return true };
        if method.as_str() != "delegate" {
            return true;
        }
        let mut names: Vec<Symbol> = Vec::new();
        let (mut to, mut prefix, mut allow_nil) = (None, false, false);
        for a in args {
            match &*a.node {
                ExprNode::Lit { value: Literal::Sym { value } } => names.push(value.clone()),
                ExprNode::Hash { entries, .. } => {
                    for (k, v) in entries {
                        let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else {
                            continue;
                        };
                        match key.as_str() {
                            "to" => {
                                if let ExprNode::Lit { value: Literal::Sym { value } } = &*v.node {
                                    to = Some(value.clone());
                                }
                            }
                            "prefix" => prefix = matches!(&*v.node,
                                ExprNode::Lit { value: Literal::Bool { value: true } }),
                            "allow_nil" => allow_nil = matches!(&*v.node,
                                ExprNode::Lit { value: Literal::Bool { value: true } }),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        // Only the shape we can reproduce exactly. Anything else stays
        // in `unknown_calls`, visible, rather than half-expanded.
        let Some(target) = to else { return true };
        for m in names {
            let name = if prefix {
                format!("{}_{}", target.as_str(), m.as_str())
            } else {
                m.as_str().to_string()
            };
            out.push(Delegation { method: m, target: target.clone(), name, allow_nil });
        }
        false
    });
    out
}

/// When a reset callback runs (`ActiveSupport::CurrentAttributes`'
/// `before_reset`, and `after_reset` with its alias `resets`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResetPhase {
    Before,
    After,
}

/// What a reset callback runs: a block, instance-exec'd on the
/// instance (its body becomes a generated instance method), or one of
/// the class's methods by name.
enum ResetCallback {
    Block(Expr),
    Method(Symbol),
}

/// The generated instance method a reset callback block becomes.
fn reset_block_method(index: usize) -> String {
    format!("__rh_reset_callback_{index}")
}

/// `before_reset { … }`, `resets do … end`, `after_reset :name` —
/// consumed, in source order. Rails sets each as a `:reset` callback
/// (`set_callback :reset, :before | :after, *methods, &block`). Only a
/// block without parameters and plain symbol names are reproduced;
/// anything else (options such as `if:`, a block taking the instance)
/// stays in `unknown_calls`, visible.
fn take_reset_callbacks(lc: &mut LibraryClass) -> Vec<(ResetPhase, ResetCallback)> {
    let mut out = Vec::new();
    lc.unknown_calls.retain(|call| {
        let ExprNode::Send { recv: None, method, args, block, .. } = &*call.node else { return true };
        let phase = match method.as_str() {
            "before_reset" => ResetPhase::Before,
            "resets" | "after_reset" => ResetPhase::After,
            _ => return true,
        };
        let names: Option<Vec<Symbol>> = args
            .iter()
            .map(|a| match &*a.node {
                ExprNode::Lit { value: Literal::Sym { value } } => Some(value.clone()),
                _ => None,
            })
            .collect();
        let Some(names) = names else { return true };
        let body = match block.as_ref().map(|b| &*b.node) {
            None => None,
            Some(ExprNode::Lambda { params, rest_param: None, block_param: None, body, signature, .. }) if params.is_empty() && signature.is_empty() => {
                // The body becomes a method: its own `next` is `return`
                // there; its own `break`/`return` (LocalJumpError in
                // Rails) leave the call unknown, visible.
                use crate::lower::lifted_block::{self, Form};
                if lifted_block::jumps(body).unliftable(Form::Block).is_some() {
                    return true;
                }
                let mut body = body.clone();
                lifted_block::to_method_body(&mut body, Form::Block);
                Some(body)
            }
            Some(_) => return true,
        };
        if names.is_empty() && body.is_none() {
            return true;
        }
        out.extend(names.into_iter().map(|n| (phase, ResetCallback::Method(n))));
        if let Some(body) = body {
            out.push((phase, ResetCallback::Block(body)));
        }
        false
    });
    out
}

/// The generated callback-block methods get their blocks' bodies.
fn fill_reset_callback_blocks(methods: &mut [MethodDef], callbacks: &[(ResetPhase, ResetCallback)]) {
    for (index, (_, callback)) in callbacks.iter().enumerate() {
        let ResetCallback::Block(body) = callback else { continue };
        let name = reset_block_method(index);
        if let Some(m) = methods
            .iter_mut()
            .find(|m| m.receiver == MethodReceiver::Instance && m.name.as_str() == name)
        {
            m.body = body.clone();
        }
    }
}

/// `super` inside an app-written attribute reader is the generated
/// storage read: rewritten to `@attr`, as the writers' `super(value)`
/// is to the storage write.
fn rewrite_super_readers(lc: &mut LibraryClass, attrs: &[Symbol]) {
    fn rewrite(expr: &mut Expr, ivar: &Symbol) {
        expr.node.for_each_child_mut(&mut |c| rewrite(c, ivar));
        let ExprNode::Super { args, block: None } = &*expr.node else { return };
        if args.as_ref().is_some_and(|a| !a.is_empty()) {
            return;
        }
        let span = expr.span;
        *expr = Expr::new(span, ExprNode::Ivar { name: ivar.clone() });
    }
    for m in &mut lc.methods {
        if m.receiver != MethodReceiver::Instance || !m.params.is_empty() {
            continue;
        }
        if attrs.iter().any(|a| *a == m.name) {
            let ivar = m.name.clone();
            rewrite(&mut m.body, &ivar);
        }
    }
}

/// `super(value)` inside an app-written `<attr>=` is the generated
/// storage write. Rewrite it in place so no superclass is needed.
fn rewrite_super_writers(lc: &mut LibraryClass, attrs: &[Symbol]) {
    for m in &mut lc.methods {
        if m.receiver != MethodReceiver::Instance {
            continue;
        }
        let Some(attr) = m.name.as_str().strip_suffix('=') else { continue };
        if !attrs.iter().any(|a| a.as_str() == attr) {
            continue;
        }
        let ivar = Symbol::from(attr);
        // The writer's one positional parameter, which a bare `super`
        // forwards. Any other parameter list leaves a bare `super` alone.
        let param = match m.params.as_slice() {
            [p] if !p.keyword && !p.rest && !p.forwarding && !p.name.as_str().is_empty() => Some(p.name.clone()),
            _ => None,
        };
        rewrite_super(&mut m.body, &ivar, param.as_ref());
    }
}

fn rewrite_super(expr: &mut Expr, ivar: &Symbol, param: Option<&Symbol>) {
    expr.node.for_each_child_mut(&mut |c| rewrite_super(c, ivar, param));
    // A super that passes a block is not a plain setter call; leave it.
    let ExprNode::Super { args, block: None } = &*expr.node else { return };
    let span = expr.span;
    // `super(value)` writes that value. A bare `super` (zsuper) forwards
    // the method's parameter as it is NOW — a reassignment before it
    // counts — so it writes the parameter's current value: Core's
    // `def pod_id=(value); …; super; end`. `super()` (no argument to a
    // one-argument writer) is Rails' ArgumentError and stays as written.
    let value = match args {
        Some(args) => match args.as_slice() {
            [value] => value.clone(),
            _ => return,
        },
        None => match param {
            Some(name) => Expr::new(span, ExprNode::Var { id: crate::ident::VarId(0), name: name.clone() }),
            None => return,
        },
    };
    *expr = Expr::new(
        span,
        ExprNode::Assign {
            target: crate::expr::LValue::Ivar { name: ivar.clone() },
            value,
        },
    );
}

/// Build the synthesized half as Ruby and let ingest parse it, rather
/// than hand-assembling three dozen `Expr` trees.
fn synthesized_source(
    lc: &LibraryClass,
    attrs: &[Symbol],
    delegates: &[Delegation],
    callbacks: &[(ResetPhase, ResetCallback)],
    defaults: &[(Symbol, DefaultBody)],
    authored_initialize: bool,
) -> String {
    let class = lc.name.0.as_str();
    let defines = |name: &str| {
        lc.methods
            .iter()
            .any(|m| m.receiver == MethodReceiver::Instance && m.name.as_str() == name)
    };

    let mut body = String::new();
    // ONE INSTANCE PER THREAD, which is what `CurrentAttributes` means:
    // Rails keeps the instance in `IsolatedExecutionState`, so two
    // requests served at once each see their own `Current.user`. A
    // class-level `@__instance` was one instance for the whole process
    // -- correct under the single-fiber server the binary used to run,
    // and a cross-request identity leak the moment two green threads (or
    // two Puma threads on the CRuby lane) dispatch at once. The slot is
    // `Thread.current[...]`, keyed per class so two CurrentAttributes
    // classes in one app do not share it; spinel types thread-local
    // storage by key from its assignments, the way it types ivars, so
    // the reader below narrows to the class and every forwarder stays
    // typed.
    //
    // `reset` REPLACES the instance rather than nilling it. Rails'
    // `CurrentAttributes.reset` sets every attribute back to its
    // default, which a fresh instance is -- and a reader that ever
    // answered a nilable made `Current.instance.user` fail to dispatch
    // and EVERY class-level forwarder register `Untyped`. The reader
    // narrows its slot read behind `is_a?`, not `nil?`, for the same
    // reason: `Thread#[]` answers untyped, and a `nil?` test leaves it so.
    //
    // The reset callbacks run around the replacement as Rails'
    // `run_callbacks(:reset)` runs them around the attribute reset: the
    // before ones in order on the instance being reset, the after ones in
    // reverse order on the reset one.
    let key = format!("__current_attrs_{}", class.replace("::", "_"));
    let call = |index: usize, callback: &ResetCallback| match callback {
        ResetCallback::Block(_) => reset_block_method(index),
        ResetCallback::Method(name) => name.as_str().to_string(),
    };
    let before: Vec<String> = callbacks
        .iter()
        .enumerate()
        .filter(|(_, (phase, _))| *phase == ResetPhase::Before)
        .map(|(i, (_, c))| call(i, c))
        .collect();
    let after: Vec<String> = callbacks
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, (phase, _))| *phase == ResetPhase::After)
        .map(|(i, (_, c))| call(i, c))
        .collect();
    let mut reset = String::new();
    if !before.is_empty() {
        reset.push_str(&format!("    {class}.instance.__rh_before_reset\n"));
    }
    reset.push_str(&format!("    Thread.current[:{key}] = {class}.new\n"));
    if !after.is_empty() {
        reset.push_str(&format!("    {class}.instance.__rh_after_reset\n"));
    }
    body.push_str(&format!(
        "  def self.instance\n    c = Thread.current[:{key}]\n    return c if c.is_a?({class})\n    c = {class}.new\n    Thread.current[:{key}] = c\n    c\n  end\n\n\
         \x20 def self.reset\n{reset}    nil\n  end\n\n"
    ));
    for (runner, calls) in [("__rh_before_reset", &before), ("__rh_after_reset", &after)] {
        if !calls.is_empty() {
            body.push_str(&format!("  def {runner}\n"));
            for c in calls.iter() {
                body.push_str(&format!("    {c}\n"));
            }
            body.push_str("    nil\n  end\n\n");
        }
    }
    for (index, (_, callback)) in callbacks.iter().enumerate() {
        if matches!(callback, ResetCallback::Block(_)) {
            body.push_str(&format!("  def {}\n    nil\n  end\n\n", reset_block_method(index)));
        }
    }

    for a in attrs {
        let a = a.as_str();
        if !defines(a) {
            body.push_str(&format!("  def {a}\n    @{a}\n  end\n\n"));
        }
        if !defines(&format!("{a}=")) {
            body.push_str(&format!("  def {a}=(value)\n    @{a} = value\n  end\n\n"));
        }
    }

    for d in delegates {
        if defines(&d.name) {
            continue;
        }
        let (t, m) = (d.target.as_str(), d.method.as_str());
        if d.allow_nil {
            body.push_str(&format!(
                "  def {}\n    @{t}.nil? ? nil : @{t}.{m}\n  end\n\n",
                d.name
            ));
        } else {
            body.push_str(&format!("  def {}\n    @{t}.{m}\n  end\n\n", d.name));
        }
    }

    body.push_str(&defaults_source(class, defaults, authored_initialize));
    if !defines("set") {
        body.push_str(&set_source(lc, attrs, delegates));
    }

    // Class-level forwarders — the surface app code actually calls
    // (`Current.user`, `Current.account`). Rails gets these from
    // `method_missing`; a statically-resolved target needs them real.
    // activesupport 8.2 delegates an app-written method only when it is
    // public (`method_added`), never `initialize`, and never over a class
    // method of the same name; a forwarder emitted after an app-written
    // `def self.x` would replace it.
    let class_method = |name: &str| {
        lc.methods
            .iter()
            .any(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == name)
    };
    let mut forwarded: Vec<(String, Option<(String, String)>)> = Vec::new();
    for a in attrs {
        forwarded.push((a.as_str().to_string(), None));
        forwarded.push((format!("{}=", a.as_str()), None));
    }
    for d in delegates {
        forwarded.push((d.name.clone(), None));
    }
    for m in &lc.methods {
        if m.receiver == MethodReceiver::Instance
            && m.visibility == MethodVisibility::Public
            && m.name.as_str() != "initialize"
        {
            forwarded.push((m.name.as_str().to_string(), forwarder_signature(m)));
        }
    }
    // The generated `set` (an app-written one forwards as written).
    if !defines("set") {
        forwarded.push(("set".to_string(), Some(("attributes, &block".to_string(), "attributes, &block".to_string()))));
    }
    // An app-written method replaces the attribute's or delegate's
    // generated one, and its own signature is the one to forward.
    forwarded.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.is_some().cmp(&a.1.is_some())));
    forwarded.dedup_by(|b, a| a.0 == b.0);
    for (name, signature) in forwarded {
        if class_method(&name) {
            continue;
        }
        if let Some(attr) = name.strip_suffix('=') {
            body.push_str(&format!(
                "  def self.{attr}=(value)\n    {class}.instance.{attr} = value\n  end\n\n"
            ));
        } else if let Some((params, args)) = signature {
            body.push_str(&format!(
                "  def self.{name}({params})\n    {class}.instance.{name}({args})\n  end\n\n"
            ));
        } else {
            body.push_str(&format!(
                "  def self.{name}\n    {class}.instance.{name}\n  end\n\n"
            ));
        }
    }

    format!("class {class}\n{body}end\n")
}

/// The parameter list and argument list of the class-level forwarder of
/// an app-written instance method that takes arguments or a block, or
/// `None` for one that takes neither. Rails forwards with `...`; the
/// analysis cannot verify a `...` destination, so the forwarder spells
/// the method's own parameters out (`def self.fetch(shop_id, &block)` →
/// `instance.fetch(shop_id, &block)`). A keyword stays a keyword, also
/// one ingest flattened to a positional (`from_keyword`), which the Ruby
/// family writes back as a keyword on the method itself.
/// A default is written into the forwarder only when it is a literal or
/// a constant, which reads the same with the class as `self`; any other
/// default, or a parameter shape the list cannot spell, forwards
/// everything instead (`*args, **kwargs, &block`).
fn forwarder_signature(m: &MethodDef) -> Option<(String, String)> {
    let takes_block = m.block_param.is_some() || m.has_anonymous_block || yields(&m.body);
    if m.params.is_empty() && !takes_block {
        return None;
    }
    let everything = || Some(("*args, **kwargs, &block".to_string(), "*args, **kwargs, &block".to_string()));
    fn constant(e: &Expr) -> bool {
        match &*e.node {
            ExprNode::Lit { .. } | ExprNode::Const { .. } => true,
            ExprNode::Array { elements, .. } => elements.iter().all(constant),
            ExprNode::Hash { entries, .. } => entries.iter().all(|(k, v)| constant(k) && constant(v)),
            _ => false,
        }
    }
    let (mut params, mut args): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for p in &m.params {
        let name = p.name.as_str();
        if p.forwarding || name.is_empty() {
            return everything();
        }
        let default = match &p.default {
            Some(d) if constant(d) => Some(crate::emit::ruby::emit_expr(d)),
            Some(_) => return everything(),
            None => None,
        };
        // Ingest flattened an optional keyword (or a `**rest`) to a
        // positional; the source, and the emitted method, still take it
        // by keyword.
        let keyword = p.keyword || p.from_keyword || p.from_kwrest;
        match (p.rest || p.from_kwrest, keyword, default) {
            (true, true, _) => {
                params.push(format!("**{name}"));
                args.push(format!("**{name}"));
            }
            (true, false, _) => {
                params.push(format!("*{name}"));
                args.push(format!("*{name}"));
            }
            (false, true, default) => {
                params.push(match default {
                    Some(d) => format!("{name}: {d}"),
                    None => format!("{name}:"),
                });
                args.push(format!("{name}: {name}"));
            }
            (false, false, default) => {
                params.push(match default {
                    Some(d) => format!("{name} = {d}"),
                    None => name.to_string(),
                });
                args.push(name.to_string());
            }
        }
    }
    if takes_block {
        let block = m.block_param.as_ref().map(|b| b.name.as_str().to_string()).filter(|b| b != "__blk");
        let block = block.unwrap_or_else(|| "block".to_string());
        if m.params.iter().any(|p| p.name.as_str() == block) {
            return everything();
        }
        params.push(format!("&{block}"));
        args.push(format!("&{block}"));
    }
    Some((params.join(", "), args.join(", ")))
}

/// Does the body `yield` (outside a `def` nested in it)? A forwarder must
/// then pass its block on.
fn yields(body: &Expr) -> bool {
    match &*body.node {
        ExprNode::Yield { .. } => true,
        ExprNode::MethodDefExpr { .. } => false,
        ExprNode::Send { recv: None, method, .. } if method.as_str() == "block_given?" => true,
        node => {
            let mut found = false;
            node.for_each_child(&mut |c| found |= yields(c));
            found
        }
    }
}

/// `initialize` assigning each defaulted attribute (unless the app wrote
/// one: [`prepend_default_assignments`]), and the default methods it
/// reads. Their bodies are placeholders [`fill_default_bodies`] replaces.
fn defaults_source(class: &str, defaults: &[(Symbol, DefaultBody)], authored_initialize: bool) -> String {
    let mut out = String::new();
    if defaults.is_empty() {
        return out;
    }
    if !authored_initialize {
        out.push_str("  def initialize\n");
        for (attr, _) in defaults {
            out.push_str(&format!("    @{attr} = {class}.{}\n", default_method(attr.as_str()), attr = attr.as_str()));
        }
        out.push_str("  end\n\n");
    }
    for (attr, default) in defaults {
        let attr = attr.as_str();
        match default {
            DefaultBody::Dynamic(_) => {
                out.push_str(&format!(
                    "  def self.{}\n    value = {class}.{}\n    value.is_a?(Proc) ? value.call : value.dup\n  end\n\n",
                    default_method(attr),
                    default_value_method(attr)
                ));
                out.push_str(&format!("  def self.{}\n    nil\n  end\n\n", default_value_method(attr)));
            }
            _ => out.push_str(&format!("  def self.{}\n    nil\n  end\n\n", default_method(attr))),
        }
    }
    out
}

/// `CurrentAttributes#set`, which is `Object#with(**attributes)`:
///
/// ```ruby
/// old_values = {}
/// begin
///   attributes.each do |key, value|
///     old_values[key] = public_send(key)
///     public_send("#{key}=", value)
///   end
///   yield self
/// ensure
///   old_values.each { |key, old_value| public_send("#{key}=", old_value) }
/// end
/// ```
///
/// `public_send` becomes a `case` over the names that have a public
/// reader and writer here (the attributes, and app-written pairs). A
/// name with only a reader is read and saved, then fails on its writer;
/// any other name fails as `public_send` fails on it.
fn set_source(lc: &LibraryClass, attrs: &[Symbol], delegates: &[Delegation]) -> String {
    let class = lc.name.0.as_str();
    let public = |name: &str| {
        lc.methods
            .iter()
            .filter(|m| m.receiver == MethodReceiver::Instance && m.name.as_str() == name)
            .all(|m| m.visibility == MethodVisibility::Public)
    };
    let identifier = |name: &str| {
        let core = name.strip_suffix(['?', '!']).unwrap_or(name);
        core.chars().next().is_some_and(|c| c == '_' || c.is_ascii_lowercase())
            && core.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
    };
    let mut readers: Vec<String> = Vec::new();
    let mut writers: Vec<String> = Vec::new();
    for a in attrs {
        readers.push(a.as_str().to_string());
        writers.push(a.as_str().to_string());
    }
    readers.extend(delegates.iter().map(|d| d.name.clone()));
    for m in lc.methods.iter().filter(|m| m.receiver == MethodReceiver::Instance) {
        let name = m.name.as_str();
        if let Some(base) = name.strip_suffix('=') {
            writers.push(base.to_string());
        } else if name != "initialize" && m.params.iter().all(|p| p.default.is_some() || p.rest || (p.keyword && p.default.is_some())) {
            readers.push(name.to_string());
        }
    }
    readers.retain(|r| identifier(r) && public(r));
    writers.retain(|w| identifier(w) && !w.ends_with(['?', '!']) && public(&format!("{w}=")));
    let mut seen = std::collections::BTreeSet::new();
    readers.retain(|r| seen.insert(r.clone()));

    let mut assign = String::new();
    let mut restore = String::new();
    for r in &readers {
        assign.push_str(&format!("        when \"{r}\"\n          previous[key] = self.{r}\n"));
        if writers.contains(r) {
            assign.push_str(&format!("          self.{r} = value\n"));
            restore.push_str(&format!("        when \"{r}\"\n          self.{r} = value\n"));
        } else {
            assign.push_str(&format!(
                "          raise NoMethodError.new(\"undefined method '{r}=' for an instance of {class}\", :\"{r}=\")\n"
            ));
        }
    }
    let restore = if restore.is_empty() {
        String::new()
    } else {
        format!("      previous.each do |key, value|\n        case key.to_s\n{restore}        end\n      end\n")
    };
    format!(
        "  def set(attributes)\n    previous = {{}}\n    begin\n      attributes.each do |key, value|\n        case key.to_s\n{assign}        else\n          raise NoMethodError.new(\"undefined method '#{{key}}' for an instance of {class}\", key.to_s.to_sym)\n        end\n      end\n      yield self\n    ensure\n{restore}    end\n  end\n\n"
    )
}

