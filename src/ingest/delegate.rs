//! ActiveSupport's `delegate :a, :b, to: :target` → real methods.
//!
//! Rails defines these with `class_eval` at load time, so nothing about
//! them survives into an emitted tree: the declaration lands in
//! `unknown_calls` and every call to a delegated name is a bare send no
//! class defines. The failure is SILENT wherever the caller rescues —
//! campfire's `message_presentation` wraps its whole body in `rescue
//! Exception` and returns `""`, so a missing `fragment` rendered every
//! message with an EMPTY body and no error anywhere. The search page
//! looked like it had no results; it had one, drawn blank.
//!
//! [`super::current_attributes`] already expands the declaration for
//! `ActiveSupport::CurrentAttributes` subclasses, where the target is a
//! declared attribute and the forwarder can read its ivar directly.
//! This is the general case: the target is a METHOD (campfire's
//! `attr_reader :content` beside the declaration), so the forwarder
//! CALLS it, which is also what Rails' generated body does.
//!
//! ## Where it declines
//!
//! A delegated method that takes ARGUMENTS. Rails forwards them with
//! `*args, &block`; argument forwarding is the shape the strict targets
//! do not lower ([[project_kwarg_forwarding_strict_targets_gap]]), and
//! a zero-arg forwarder for a method that takes two is an arity error
//! standing in for a NameError — a different wrong answer, not a fix.
//! The test is the declaring class's own call sites: campfire's
//! `Messages::AttachmentPresentation` delegates `:tag`, `:link_to` and
//! four more `to: :context` and calls every one of them WITH arguments
//! in the same file, so that declaration stays in `unknown_calls`,
//! visible, exactly as it is today.
//!
//! The limitation that leaves: a declaration in a BASE class whose only
//! argument-passing callers are subclasses reads as zero-arg here.
//! campfire has no such case — `ActionText::Content::Filter`'s
//! `fragment` is a reader on both sides — and closing it properly means
//! the arity coming from the TARGET's own signature rather than from
//! call sites.
//!
//! ## The class side
//!
//! `delegate` inside `class << self` (recorded by the body walk as
//! `singleton_class.delegate`, which a class body may also write) defines
//! class methods. Their callers are other files, so no call site here
//! says what arity they need: each forwards `(...)`, which is exactly
//! the method Rails generates.
//!
//! ## The Ruby target
//!
//! Ruby has no argument-forwarding gap, so the zero-arg instance
//! forwarder is only the strict targets' answer. Each one also gets its
//! `(...)` twin in `App::ruby_delegate_forwarders`, which the Ruby emit
//! writes instead: a caller in another file that passes arguments
//! (asset_cloud's includers calling `add_error(msg)`) reaches the target
//! as it does under Rails. Declarations this pass declines stay declined
//! there too.
//!
//! A declined instance-side declaration stays in `unknown_calls` for the
//! strict targets, but the Ruby target has no argument-forwarding gap, so
//! it gets the `(...)` forwarders Rails generates
//! (`App::ruby_declined_delegate_forwarders`).
//!
//! ## Forwardable
//!
//! Ruby's own `def_delegators :@validation, :group_id, …` and
//! `def_delegator :@x, :m, :ali` (after `extend Forwardable`) go through
//! the same expansion: each defines `def ali(*args, &block) =
//! accessor.m(*args, &block)`, which is the `(...)` forwarder above, and
//! `def_delegators` skips `__send__` and `__id__` as Ruby does. Only an
//! instance-variable or method-name accessor is reproduced; a compiled
//! accessor expression (`"self.class"`) and SingleForwardable's singleton
//! side stay in `unknown_calls`. Once every definer call is expanded, the
//! `extend Forwardable` itself is spent and leaves `unknown_calls` too.
//! A module holding only such declarations is a mixin
//! (`util::body_has_direct_method_decl`), not a namespace: Core's
//! `Apps::Models::UiExtension::Schema::ValidationDelegation`.

use crate::dialect::{LibraryClass, MethodDef, MethodReceiver};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;

/// One expanded `delegate` entry: `<name>` forwards to `<target>.<method>`.
#[derive(Debug)]
struct Delegation {
    method: Symbol,
    target: Symbol,
    name: String,
    allow_nil: bool,
    /// Declared on the singleton class (`delegate` inside `class << self`,
    /// or `singleton_class.delegate`): the forwarder is a class method.
    /// Its callers are other files' (`Secrets.fetch(:mysql)`), so it
    /// forwards every argument with `(...)`, as Rails' generated
    /// method does, instead of reading an arity off this class's own
    /// call sites.
    class_side: bool,
    /// The real app file the `delegate` call was declared in, when the
    /// source registry can still resolve it (it can, at this point in
    /// the pipeline — see `ingest::sources`'s module doc). Empty when
    /// it can't; callers fall back to the pass's own `"<delegate>"`
    /// label for a synthesis-failure report rather than misattribute.
    file: String,
}

/// Expand every `delegate … to: …` the app's library classes declare.
///
/// Runs AFTER `lower_current_attributes`, which consumes the
/// declarations on its own classes — so what reaches here is the
/// general shape only.
pub fn lower_delegates(app: &mut crate::App) {
    // A zero-arg forwarder for an admitted `def m(a, *)` is the arity
    // error the module header declines; leave those declarations visible.
    let nameless_rest: crate::det::HashSet<String> = app
        .library_classes
        .iter()
        .flat_map(|c| c.methods.iter())
        .filter(|m| m.unsupported_formals.is_none() && m.params.iter().any(crate::dialect::Param::is_nameless_rest))
        .map(|m| m.name.as_str().to_string())
        .collect();
    let mut generated: Vec<(usize, Vec<MethodDef>)> = Vec::new();
    for (i, lc) in app.library_classes.iter_mut().enumerate() {
        let had_definer = lc.unknown_calls.iter().any(forwardable_definer);
        let (methods, full, declined) = expand_delegates_declining(lc, &nameless_rest);
        if had_definer {
            drop_spent_forwardable_extend(lc);
        }
        if !methods.is_empty() {
            generated.push((i, methods));
        }
        if !full.is_empty() {
            app.ruby_delegate_forwarders.entry(lc.name.clone()).or_default().extend(full);
        }
        if !declined.is_empty() {
            app.ruby_declined_delegate_forwarders.entry(lc.name.clone()).or_default().extend(declined);
        }
    }
    for (i, methods) in generated {
        app.library_classes[i].methods.extend(methods);
    }
    for model in &mut app.models {
        lower_model_delegates(model, &nameless_rest, &mut app.ruby_delegate_forwarders, &mut app.ruby_declined_delegate_forwarders);
    }
}

/// A model's own `delegate :shop, to: :user`, expanded as a library
/// class's is: each declaration this pass reproduces becomes its
/// forwarder methods where it was written. One that needs argument
/// forwarding stays an `Unknown` body statement, and the Ruby target
/// writes its `(...)` forwarders (`App::ruby_declined_delegate_forwarders`,
/// which the model lowering's report consults).
fn lower_model_delegates(
    model: &mut crate::dialect::Model,
    decline: &crate::det::HashSet<String>,
    full_forwarders: &mut crate::det::HashMap<crate::ident::ClassId, Vec<MethodDef>>,
    declined_forwarders: &mut crate::det::HashMap<crate::ident::ClassId, Vec<MethodDef>>,
) {
    use crate::dialect::ModelBodyItem;
    let is_delegate = |e: &Expr| {
        matches!(&*e.node, ExprNode::Send { recv: None, method, .. } if method.as_str() == "delegate")
    };
    let calls: Vec<Expr> = model
        .body
        .iter()
        .filter_map(|item| match item {
            ModelBodyItem::Unknown { expr, .. } if is_delegate(expr) => Some(expr.clone()),
            _ => None,
        })
        .collect();
    if calls.is_empty() {
        return;
    }
    let mut lc = LibraryClass {
        conditional_definitions: Vec::new(),
        method_aliases: Vec::new(),
        top_level_constants: Vec::new(),
        name: model.name.clone(),
        is_module: false,
        parent: model.parent.clone(),
        parent_span: Default::default(),
        includes: Vec::new(),
        methods: model.methods().cloned().collect(),
        nullable_columns: Vec::new(),
        origin: None,
        constants: Vec::new(),
        unknown_calls: calls,
        class_ivar_initializers: Vec::new(),
        visibility_overrides: Vec::new(),
    };
    let (methods, full, declined) = expand_delegates_declining(&mut lc, decline);
    // The declarations the expansion consumed: those it left are declined.
    let left: Vec<crate::span::Span> = lc.unknown_calls.iter().map(|e| e.span).collect();
    let mut by_file_pos: Vec<MethodDef> = methods;
    let mut body = Vec::with_capacity(model.body.len());
    for item in std::mem::take(&mut model.body) {
        match item {
            ModelBodyItem::Unknown { expr, leading_comments, leading_blank_line }
                if is_delegate(&expr) && !left.contains(&expr.span) =>
            {
                // Each forwarder of this declaration, in the order written.
                let names = delegated_names(&expr);
                let mut first = true;
                for name in names {
                    let Some(at) = by_file_pos.iter().position(|m| m.name.as_str() == name && m.receiver == MethodReceiver::Instance) else { continue };
                    let method = by_file_pos.remove(at);
                    body.push(ModelBodyItem::Method {
                        method,
                        leading_comments: if first { leading_comments.clone() } else { Vec::new() },
                        leading_blank_line: first && leading_blank_line,
                    });
                    first = false;
                }
            }
            item => body.push(item),
        }
    }
    // Anything not matched by name (none expected) still lands in the class.
    body.extend(by_file_pos.into_iter().map(|method| ModelBodyItem::Method { method, leading_comments: Vec::new(), leading_blank_line: false }));
    model.body = body;
    if !full.is_empty() {
        full_forwarders.entry(model.name.clone()).or_default().extend(full);
    }
    if !declined.is_empty() {
        declined_forwarders.entry(model.name.clone()).or_default().extend(declined);
    }
}

/// The method names a `delegate` declaration defines, prefix applied.
fn delegated_names(call: &Expr) -> Vec<String> {
    let ExprNode::Send { args, .. } = &*call.node else { return Vec::new() };
    let mut names = Vec::new();
    let (mut to, mut prefix) = (None, false);
    for a in args {
        match &*a.node {
            ExprNode::Lit { value: Literal::Sym { value } } => names.push(value.as_str().to_string()),
            ExprNode::Hash { entries, .. } => {
                for (k, v) in entries {
                    match (&*k.node, &*v.node) {
                        (ExprNode::Lit { value: Literal::Sym { value: key } }, ExprNode::Lit { value: Literal::Sym { value } }) if key.as_str() == "to" => {
                            to = Some(value.as_str().to_string())
                        }
                        (ExprNode::Lit { value: Literal::Sym { value: key } }, ExprNode::Lit { value: Literal::Bool { value: true } }) if key.as_str() == "prefix" => {
                            prefix = true
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    match (prefix, to) {
        (true, Some(to)) => names.into_iter().map(|n| format!("{to}_{n}")).collect(),
        _ => names,
    }
}

/// A call to one of Forwardable's definers (or ActiveSupport's
/// same-named `delegate`).
fn forwardable_definer(e: &Expr) -> bool {
    matches!(&*e.node, ExprNode::Send { method, .. }
        if forwardable_kind(method.as_str()).is_some() || matches!(method.as_str(), "delegate" | "instance_delegate"))
}

/// `extend Forwardable` gives the class nothing but the definers
/// (`def_delegators`, `def_delegator`, `delegate`, `instance_delegate`
/// and their `def_instance_*` spellings). Once this pass has turned
/// every definer call the class makes into methods, the mixin has done
/// all it does in this body, and the call is spent rather than an
/// unmodelled one the emit would report as dropped. With any definer
/// call left in `unknown_calls`, the `extend` stays beside it.
fn drop_spent_forwardable_extend(lc: &mut LibraryClass) {
    let forwardable_extend = |e: &Expr| {
        matches!(&*e.node, ExprNode::Send { recv: None, method, args, block: None, .. }
            if method.as_str() == "extend"
                && matches!(args.as_slice(), [a] if matches!(&*a.node, ExprNode::Const { path }
                    if path.len() == 1 && path[0].as_str() == "Forwardable")))
    };
    if lc.unknown_calls.iter().any(forwardable_definer) {
        return;
    }
    lc.unknown_calls.retain(|e| !forwardable_extend(e));
}

/// `synthesized_source` produced Ruby Prism couldn't parse cleanly —
/// roundhouse's gap, not the app's. Record one survey entry naming
/// every delegated name in this class's declaration, attributed to the
/// real file when a delegated call's own span still resolves one.
fn record_synthesis_failure(delegates: &[Delegation], diags: &[crate::diagnostic::Diagnostic]) {
    let names = delegates
        .iter()
        .map(|d| d.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let file = delegates
        .iter()
        .map(|d| d.file.as_str())
        .find(|f| !f.is_empty())
        .unwrap_or("<delegate>")
        .to_string();
    super::survey::record_synthesis_failure(file, &format!("delegate forwarder for `{names}`"), diags);
}

/// The per-class body of `lower_delegates`, factored out so a caller
/// with a single `LibraryClass` in hand — rather than a whole `App` to
/// loop over — can drive the same expansion. `lower_controller_to_
/// library_class` is exactly that caller: a controller isn't a
/// `LibraryClass` at the point `lower_delegates` runs over
/// `app.library_classes` (ingest time; controllers only become one
/// per-target at emit time — see `lower::controller_to_library`), so a
/// `delegate` call in a controller body never reached this file at
/// all until the controller lowering started collecting it into its
/// own `unknown_calls` and calling this directly.
///
/// Returns the synthesized forwarder methods; does NOT append them to
/// `lc.methods` itself (`lower_delegates` above batches that across the
/// whole app; a single-class caller can just extend its own `methods`
/// with the result).
pub(crate) fn expand_delegates_in_class(lc: &mut LibraryClass) -> Vec<MethodDef> {
    expand_delegates_declining(lc, &crate::det::HashSet::default()).0
}

/// `expand_delegates_in_class`, leaving any delegation that names one
/// of `decline` in `unknown_calls`. The second list is the Ruby target's
/// version of each zero-arg instance forwarder: the same method forwarding
/// `(...)`, which is what Rails generates (see
/// `App::ruby_delegate_forwarders`). The third is the Ruby target's
/// `(...)` forwarder for each declaration left in `unknown_calls` because
/// it needs argument forwarding (`App::ruby_declined_delegate_forwarders`).
fn expand_delegates_declining(
    lc: &mut LibraryClass,
    decline: &crate::det::HashSet<String>,
) -> (Vec<MethodDef>, Vec<MethodDef>, Vec<MethodDef>) {
    let mut declined = Vec::new();
    let delegates = take_delegate_decls(lc, decline, &mut declined);
    let declined =
        if declined.is_empty() { Vec::new() } else { parse_forwarders(&synthesized_source(lc, &declined, true), &declined) };
    if delegates.is_empty() {
        return (Vec::new(), Vec::new(), declined);
    }
    let methods = parse_forwarders(&synthesized_source(lc, &delegates, false), &delegates);
    let zero_arg: Vec<&Delegation> = delegates.iter().filter(|d| forwards_zero_arg(d)).collect();
    let full = if zero_arg.is_empty() || methods.is_empty() {
        Vec::new()
    } else {
        parse_forwarders(&synthesized_source(lc, &delegates, true), &delegates)
            .into_iter()
            .filter(|m| zero_arg.iter().any(|d| d.name == m.name.as_str()))
            .collect()
    };
    (methods, full, declined)
}

/// The shape `synthesized_source` writes as a zero-arg forwarder: an
/// instance-side reader that is neither a setter nor an operator.
fn forwards_zero_arg(d: &Delegation) -> bool {
    !d.class_side && !d.method.as_str().ends_with('=') && operator_forwarder("t", d.method.as_str()).is_none()
}

fn parse_forwarders(src: &str, delegates: &[Delegation]) -> Vec<MethodDef> {
    // Isolated in its OWN scope — never the outer one that spans
    // the whole app's ingest — so a bug in `synthesized_source`
    // can't render its parse errors against an unrelated real
    // file (see `ingest::sources`'s module doc for how that
    // happened before this existed).
    let (parsed, diags) = crate::ingest::prism::scope(|| {
        crate::ingest::ingest_library_classes(src.as_bytes(), "<delegate>")
    });
    match parsed {
        Ok(classes) if diags.is_empty() => {
            classes.into_iter().flat_map(|c| c.methods).collect()
        }
        Ok(_) => {
            record_synthesis_failure(delegates, &diags);
            Vec::new()
        }
        Err(err) => {
            super::survey::record(&err);
            Vec::new()
        }
    }
}

/// Consume the declarations this pass can reproduce EXACTLY, leaving
/// every other shape in `unknown_calls` rather than half-expanded. A
/// declaration of a reproducible shape left there only because it needs
/// argument forwarding also goes to `declined`, for the Ruby target.
fn take_delegate_decls(
    lc: &mut LibraryClass,
    decline: &crate::det::HashSet<String>,
    declined: &mut Vec<Delegation>,
) -> Vec<Delegation> {
    let called_with_args = names_called_with_arguments(lc);
    let single_forwardable = extends_single_forwardable(lc);
    let mut out = Vec::new();
    lc.unknown_calls.retain(|call| {
        let ExprNode::Send { recv, method, args, block, .. } = &*call.node else { return true };
        let forwardable = forwardable_kind(method.as_str());
        if method.as_str() != "delegate" && forwardable.is_none() {
            return true;
        }
        let class_side = match recv {
            None => false,
            Some(recv) if matches!(&*recv.node, ExprNode::Send { recv: None, method, args, block: None, .. }
                if method.as_str() == "singleton_class" && args.is_empty()) => true,
            Some(_) => return true,
        };
        if let Some(kind) = forwardable {
            // `extend SingleForwardable` gives the class its own
            // `def_delegators`, defining singleton methods: a shape this
            // reading of Forwardable's instance side does not cover.
            if block.is_some() || (single_forwardable && !class_side) {
                return true;
            }
            let Some(delegations) = forwardable_delegations(kind, args) else { return true };
            let needs_forwarding = !class_side
                && delegations.iter().any(|(_, _, name)| {
                    (!name.ends_with('=') && called_with_args.contains(name.as_str())) || decline.contains(name.as_str())
                });
            let file = super::sources::path_of(call.span.file).unwrap_or_default();
            let into = if needs_forwarding { &mut *declined } else { &mut out };
            for (target, method, name) in delegations {
                into.push(Delegation { method, target, name, allow_nil: false, class_side, file: file.clone() });
            }
            return needs_forwarding;
        }
        let mut names: Vec<Symbol> = Vec::new();
        let (mut to, mut prefix, mut allow_nil) = (None, false, false);
        let mut unknown_option = false;
        for a in args {
            match &*a.node {
                ExprNode::Lit { value: Literal::Sym { value } } => names.push(value.clone()),
                ExprNode::Hash { entries, .. } => {
                    for (k, v) in entries {
                        let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else {
                            unknown_option = true;
                            continue;
                        };
                        match key.as_str() {
                            "to" => {
                                if let ExprNode::Lit { value: Literal::Sym { value } } = &*v.node {
                                    to = Some(value.clone());
                                } else {
                                    unknown_option = true;
                                }
                            }
                            "prefix" => {
                                prefix = matches!(&*v.node,
                                    ExprNode::Lit { value: Literal::Bool { value: true } });
                                if !prefix {
                                    unknown_option = true;
                                }
                            }
                            "allow_nil" => allow_nil = matches!(&*v.node,
                                ExprNode::Lit { value: Literal::Bool { value: true } }),
                            // `private:`, `prefix: :other_name` and the
                            // rest are shapes this does not reproduce.
                            _ => unknown_option = true,
                        }
                    }
                }
                _ => unknown_option = true,
            }
        }
        let Some(target) = to else { return true };
        if unknown_option || names.is_empty() {
            return true;
        }
        // Arguments at a call site mean the forwarder needs to forward
        // them — see the module header. A setter is exempt: Ruby's own
        // assignment syntax fixes its arity at exactly one, so
        // `self.behavior = v` calling `behavior=` WITH an argument is
        // not evidence this pass can't cover it — it's what every
        // setter call looks like, delegated or not.
        let needs_forwarding = !class_side
            && (names.iter().any(|n| !n.as_str().ends_with('=') && called_with_args.contains(n.as_str()))
                || names.iter().any(|n| decline.contains(n.as_str())));
        let file = super::sources::path_of(call.span.file).unwrap_or_default();
        let into = if needs_forwarding { &mut *declined } else { &mut out };
        for m in names {
            let name = if prefix {
                format!("{}_{}", target.as_str(), m.as_str())
            } else {
                m.as_str().to_string()
            };
            into.push(Delegation { method: m, target: target.clone(), name, allow_nil, class_side, file: file.clone() });
        }
        needs_forwarding
    });
    out
}

/// Which of Forwardable's instance-side definers a class-body call is.
#[derive(Clone, Copy)]
enum ForwardableKind {
    /// `def_delegators accessor, *methods`.
    Many,
    /// `def_delegator accessor, method, ali = method`.
    One,
}

fn forwardable_kind(method: &str) -> Option<ForwardableKind> {
    match method {
        "def_delegators" | "def_instance_delegators" => Some(ForwardableKind::Many),
        "def_delegator" | "def_instance_delegator" => Some(ForwardableKind::One),
        _ => None,
    }
}

/// The class (or module) mixes in `SingleForwardable`, whose
/// `def_delegators` defines singleton methods.
fn extends_single_forwardable(lc: &LibraryClass) -> bool {
    lc.unknown_calls.iter().any(|call| {
        matches!(&*call.node, ExprNode::Send { recv: None, method, args, .. }
            if method.as_str() == "extend"
                && args.iter().any(|a| matches!(&*a.node, ExprNode::Const { path }
                    if path.last().is_some_and(|s| s.as_str() == "SingleForwardable"))))
    })
}

/// A symbol or string literal's text: Forwardable takes either for the
/// accessor and the method names.
fn name_literal(e: &Expr) -> Option<String> {
    match &*e.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
        ExprNode::Lit { value: Literal::Str { value } } => Some(value.to_string()),
        _ => None,
    }
}

/// An accessor this pass can write as a receiver verbatim: an instance
/// variable (`:@validation`) or a method name (`:registration`). Ruby's
/// Forwardable compiles any accessor expression into the method body; a
/// richer one (`"self.class"`, `"@a.b"`) stays in `unknown_calls`.
fn simple_accessor(text: &str) -> bool {
    let ident = |s: &str| {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    match text.strip_prefix('@') {
        Some(ivar) => ident(ivar),
        None => ident(text.strip_suffix(['?', '!']).unwrap_or(text)),
    }
}

/// `(target, method, defined name)` for each forwarder a Forwardable
/// declaration defines, or `None` for a shape it does not reproduce.
/// Ruby's `def_delegators` skips `__send__` and `__id__`;
/// `def_delegator`'s third argument names the method it defines.
fn forwardable_delegations(kind: ForwardableKind, args: &[Expr]) -> Option<Vec<(Symbol, Symbol, String)>> {
    let (accessor, rest) = args.split_first()?;
    let accessor = name_literal(accessor).filter(|a| simple_accessor(a))?;
    let names: Vec<String> = rest.iter().map(name_literal).collect::<Option<_>>()?;
    let target = Symbol::from(accessor.as_str());
    let out: Vec<(Symbol, Symbol, String)> = match (kind, names.as_slice()) {
        (ForwardableKind::Many, _) => names
            .iter()
            .filter(|m| !matches!(m.as_str(), "__send__" | "__id__"))
            .map(|m| (target.clone(), Symbol::from(m.as_str()), m.clone()))
            .collect(),
        (ForwardableKind::One, [method]) => vec![(target, Symbol::from(method.as_str()), method.clone())],
        (ForwardableKind::One, [method, ali]) => vec![(target, Symbol::from(method.as_str()), ali.clone())],
        (ForwardableKind::One, _) => return None,
    };
    (!out.is_empty()).then_some(out)
}

/// Bare names this class calls WITH arguments, anywhere in its own
/// method bodies. A delegated name in this set needs the argument
/// forwarding this pass declines to synthesize.
fn names_called_with_arguments(lc: &LibraryClass) -> crate::det::HashSet<String> {
    let mut out = crate::det::HashSet::default();
    for m in &lc.methods {
        collect_calls_with_args(&m.body, &mut out);
    }
    out
}

fn collect_calls_with_args(expr: &Expr, out: &mut crate::det::HashSet<String>) {
    expr.node.for_each_child(&mut |c| collect_calls_with_args(c, out));
    let ExprNode::Send { recv: None, method, args, block, .. } = &*expr.node else { return };
    if !args.is_empty() || block.is_some() {
        out.insert(method.as_str().to_string());
    }
}

/// The forwarders, as Ruby source — parsed back through ingest so the
/// generated bodies are ordinary IR, indistinguishable from a method
/// the app wrote. Same construction `current_attributes` uses.
/// The receiver Rails writes for `to:`: `self.<to>` when the name is a
/// Ruby keyword (`to: :class`, a model's `return` association) or one of
/// the names its generated method uses itself, the name otherwise
/// (`DELEGATION_RESERVED_METHOD_NAMES` in active_support/delegation.rb).
fn receiver(target: &str) -> std::borrow::Cow<'_, str> {
    const RESERVED: &[&str] = &[
        "__ENCODING__", "__LINE__", "__FILE__", "alias", "and", "BEGIN", "begin", "break",
        "case", "class", "def", "defined?", "do", "else", "elsif", "END", "end", "ensure",
        "false", "for", "if", "in", "module", "next", "nil", "not", "or", "redo", "rescue",
        "retry", "return", "self", "super", "then", "true", "undef", "unless", "until", "when",
        "while", "yield", "_", "arg", "args", "block", "value", "key", "other", "__delegate_target",
    ];
    if RESERVED.contains(&target) {
        format!("self.{target}").into()
    } else {
        target.into()
    }
}

/// The forwarder for an operator method (`delegate :[], :<<, :==, to:
/// :@hash` — a collection wrapper's usual shape), or `None` for an
/// ordinary name. Unary operators (`!`, `-@`) forward as zero-arg sends
/// like any reader and are not listed.
fn operator_forwarder(t: &str, m: &str) -> Option<(&'static str, String)> {
    const BINARY: &[&str] = &[
        "==", "!=", "<", ">", "<=", ">=", "<=>", "===", "=~", "!~", "+", "-", "*", "/", "%", "**",
        "<<", ">>", "&", "|", "^",
    ];
    match m {
        "[]" => Some(("key", format!("{t}[key]"))),
        "[]=" => Some(("key, value", format!("{t}[key] = value"))),
        op if BINARY.contains(&op) => Some(("other", format!("{t} {op} other"))),
        _ => None,
    }
}

/// `full_forward` writes every reader forwarder as `(...)`, the Ruby
/// target's form; without it an instance reader is zero-arg (module doc).
fn synthesized_source(lc: &LibraryClass, delegates: &[Delegation], full_forward: bool) -> String {
    let defines = |name: &str, class_side: bool| {
        let receiver = if class_side { MethodReceiver::Class } else { MethodReceiver::Instance };
        lc.methods.iter().any(|m| m.receiver == receiver && m.name.as_str() == name)
    };
    let mut body = String::new();
    for d in delegates {
        if defines(&d.name, d.class_side) {
            continue;
        }
        let (target, m) = (receiver(d.target.as_str()), d.method.as_str());
        let t = if d.allow_nil { "__delegate_target" } else { &target };
        // A setter takes the one argument Ruby's own assignment syntax
        // supplies (with `prefix: true`, `d.name` is already
        // `<prefix>_name=`); an operator takes its fixed operands.
        let (params, call) = if let Some(forwarder) = operator_forwarder(t, m) {
            forwarder
        } else if let Some(attr) = m.strip_suffix('=') {
            ("value", format!("{t}.{attr} = value"))
        } else if d.class_side || full_forward {
            ("...", format!("{t}.{m}(...)"))
        } else {
            ("", format!("{t}.{m}"))
        };
        let name = if d.class_side { format!("self.{}", d.name) } else { d.name.clone() };
        let signature = if params.is_empty() {
            name
        } else {
            format!("{name}({params})")
        };
        // Rails evaluates the receiver once. Nil's own operators still run
        // under allow_nil (for example `nil == other` returns a Boolean).
        // An `if`, not `return nil if …`: it leaves the method ending in a
        // read, which is what the strict targets want of a non-void body.
        let nil_operator = matches!(m, "==" | "!=" | "===" | "=~" | "!~" | "&" | "|" | "^" | "!");
        let call = if d.allow_nil {
            let result = if nil_operator {
                call
            } else {
                format!("if {t}.nil?\n      nil\n    else\n      {call}\n    end")
            };
            format!("{t} = {target}\n    {result}")
        } else {
            call
        };
        body.push_str(&format!("  def {signature}\n    {call}\n  end\n\n"));
    }
    // The class name is irrelevant — only the METHODS are lifted out of
    // the parse — but a wrapper is needed for the bodies to be methods.
    format!("class {}\n{body}end\n", lc.name.0.as_str().replace("::", "__"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::AccessorKind;
    use crate::effect::EffectSet;
    use crate::span::Span;

    fn library_class(src: &str) -> LibraryClass {
        crate::ingest::ingest_library_class(src.as_bytes(), "app/lib/deprecation.rb")
            .unwrap()
            .unwrap()
    }

    /// `synthesized_source`'s output, re-parsed the same way
    /// `lower_delegates` re-parses it — the same round trip that
    /// catches Bug A (a setter `def` with no parameter and a bare
    /// `x.y=` call is two syntax errors, not zero).
    fn synthesized_methods(lc: &LibraryClass, delegates: &[Delegation]) -> Vec<MethodDef> {
        let src = synthesized_source(lc, delegates, false);
        crate::ingest::ingest_library_classes(src.as_bytes(), "<test>")
            .unwrap_or_else(|e| panic!("synthesized source failed to parse: {e}\n{src}"))
            .into_iter()
            .flat_map(|c| c.methods)
            .collect()
    }

    #[test]
    fn forwardable_definers_take_alias_and_skip_send() {
        let mut lc = library_class(
            "class Wrapper\n  extend Forwardable\n\n  def_delegators :@inner, :a, :__send__, :__id__, :b=\n  def_delegator :inner, :c, :d\n  def_delegator \"@inner\", :e\nend\n",
        );
        let delegates = take_delegate_decls(&mut lc, &Default::default(), &mut Vec::new());
        let got: Vec<(&str, &str, &str)> =
            delegates.iter().map(|d| (d.target.as_str(), d.method.as_str(), d.name.as_str())).collect();
        assert_eq!(got, [("@inner", "a", "a"), ("@inner", "b=", "b="), ("inner", "c", "d"), ("@inner", "e", "e")]);
        assert!(delegates.iter().all(|d| !d.class_side && !d.allow_nil));
        assert!(
            !lc.unknown_calls.iter().any(forwardable_definer),
            "every definer call should be consumed: {:?}",
            lc.unknown_calls
        );
    }

    #[test]
    fn forwardable_shapes_it_cannot_reproduce_stay_visible() {
        // A compiled accessor expression, and SingleForwardable's
        // singleton-side `def_delegators`.
        let mut lc = library_class(
            "class Wrapper\n  extend Forwardable\n\n  def_delegators \"self.class\", :name\n  def_delegator :@x, :a, :b, :c\nend\n",
        );
        assert!(take_delegate_decls(&mut lc, &Default::default(), &mut Vec::new()).is_empty());
        assert_eq!(lc.unknown_calls.iter().filter(|c| forwardable_definer(c)).count(), 2);
        let mut single =
            library_class("class Wrapper\n  extend SingleForwardable\n\n  def_delegators :@x, :a\nend\n");
        assert!(take_delegate_decls(&mut single, &Default::default(), &mut Vec::new()).is_empty());
    }

    #[test]
    fn forwardable_in_a_singleton_class_defines_class_methods() {
        let mut lc = library_class(
            "class Registry\n  class << self\n    extend Forwardable\n    def_delegators :store, :owners\n\n    def store = 1\n  end\nend\n",
        );
        let delegates = take_delegate_decls(&mut lc, &Default::default(), &mut Vec::new());
        assert_eq!(delegates.len(), 1);
        assert!(delegates[0].class_side, "a singleton-class def_delegators defines a class method");
    }

    #[test]
    fn a_delegated_getter_and_setter_pair_both_synthesize() {
        let mut lc = library_class(
            "class Deprecation\n  attr_accessor :deprecator\n\n  delegate :behavior, :behavior=, to: :deprecator\nend\n",
        );
        let delegates = take_delegate_decls(&mut lc, &Default::default(), &mut Vec::new());
        assert_eq!(delegates.len(), 2);
        assert!(lc.unknown_calls.is_empty(), "the declaration should be consumed");

        let methods = synthesized_methods(&lc, &delegates);
        let getter = methods
            .iter()
            .find(|m| m.name.as_str() == "behavior")
            .expect("getter should be synthesized");
        assert!(getter.params.is_empty(), "a getter forwarder takes no arguments");

        let setter = methods
            .iter()
            .find(|m| m.name.as_str() == "behavior=")
            .expect("setter should be synthesized");
        assert_eq!(setter.params.len(), 1, "a setter forwarder takes exactly the one argument Ruby's own assignment syntax supplies");
    }

    #[test]
    fn prefix_true_composes_with_the_setter_name_too() {
        let mut lc = library_class(
            "class Deprecation\n  attr_accessor :deprecator\n\n  delegate :behavior=, to: :deprecator, prefix: true\nend\n",
        );
        let delegates = take_delegate_decls(&mut lc, &Default::default(), &mut Vec::new());
        assert_eq!(delegates.len(), 1);
        assert_eq!(delegates[0].name, "deprecator_behavior=", "prefix composes ahead of the delegated name, trailing `=` intact");

        let methods = synthesized_methods(&lc, &delegates);
        let setter = methods
            .iter()
            .find(|m| m.name.as_str() == "deprecator_behavior=")
            .expect("prefixed setter should be synthesized under its prefixed name");
        assert_eq!(setter.params.len(), 1);
    }

    #[test]
    fn allow_nil_setter_ends_in_a_nil_guard() {
        let mut lc = library_class(
            "class Deprecation\n  attr_accessor :deprecator\n\n  delegate :behavior=, to: :deprecator, allow_nil: true\nend\n",
        );
        let delegates = take_delegate_decls(&mut lc, &Default::default(), &mut Vec::new());
        assert_eq!(delegates.len(), 1);
        assert!(delegates[0].allow_nil);

        let methods = synthesized_methods(&lc, &delegates);
        let setter = methods
            .iter()
            .find(|m| m.name.as_str() == "behavior=")
            .expect("allow_nil setter should still be synthesized");
        assert_eq!(setter.params.len(), 1);
        // The receiver is read once into a local, then a nil guard (an
        // `If`) rather than a bare Assign ends the body — the same "ends
        // in a read" shape the getter's own allow_nil branch keeps.
        let ExprNode::Seq { exprs } = &*setter.body.node else {
            panic!("expected receiver-then-guard, got {:?}", setter.body.node)
        };
        assert!(
            matches!(exprs.last().map(|e| &*e.node), Some(ExprNode::If { .. })),
            "expected the allow_nil guard to end the body, got {:?}",
            setter.body.node
        );
    }

    #[test]
    fn bracket_assign_forwards_both_operands() {
        // `[]=` ends in `=` without being a one-value writer: it takes
        // (key, value), and forwards both.
        let mut lc = library_class(
            "class Store\n  attr_accessor :backing\n\n  delegate :[]=, to: :backing\nend\n",
        );
        let delegates = take_delegate_decls(&mut lc, &Default::default(), &mut Vec::new());
        assert_eq!(delegates.len(), 1);
        let methods = synthesized_methods(&lc, &delegates);
        let setter = methods
            .iter()
            .find(|m| m.name.as_str() == "[]=")
            .expect("[]= should be synthesized");
        assert_eq!(setter.params.len(), 2, "[]= forwards its key and its value");
    }

    /// Ordinary Ruby can't actually produce a `Send { recv: None,
    /// method: "behavior=", .. }` node — `self.behavior = v` always
    /// carries an explicit receiver; a bare `behavior = v` is *always*
    /// local-variable assignment in Ruby, never a method send,
    /// delegated or not. So `names_called_with_arguments` (which only
    /// collects receiverless sends) can never actually hold a
    /// `"name="` key from real source today, and this guard's setter
    /// exemption is not reachable through the parser as things stand.
    /// It is still the right rule to state — the arity a `def name=`
    /// forwarder needs is fixed by Ruby's own assignment grammar, not
    /// by anything a call site can be observed doing — and cheap
    /// insurance against a future IR change (or a metaprogrammed call
    /// site this ingest doesn't model yet) making such a node
    /// possible. Pinned directly against hand-built IR since the
    /// parser has no way to hand us this shape.
    #[test]
    fn a_setter_name_is_exempt_from_the_called_with_arguments_guard() {
        let mut lc = library_class(
            "class Deprecation\n  attr_accessor :deprecator\n\n  delegate :behavior=, to: :deprecator\nend\n",
        );
        lc.methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name: Symbol::from("reset!"),
            receiver: MethodReceiver::Instance,
            params: Vec::new(),
            block_param: None,
            name_span: Span::synthetic(),
            body: Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from("behavior="),
                    args: vec![Expr::new(
                        Span::synthetic(),
                        ExprNode::Lit { value: Literal::Sym { value: Symbol::from("warn") } },
                    )],
                    block: None,
                    parenthesized: false,
                },
            ),
            signature: None,
            effects: EffectSet::default(),
            enclosing_class: None,
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
        });

        let delegates = take_delegate_decls(&mut lc, &Default::default(), &mut Vec::new());
        assert_eq!(
            delegates.len(),
            1,
            "a setter's fixed arity exempts it from the call-site-arguments heuristic"
        );
    }
}
