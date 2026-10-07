//! A native `...` is sound only when its destination still has its source
//! argument contract. Library optional keywords/keyword-rest are sometimes
//! flattened; model named rest/block declarations are not fully retained.
//! Do not silently forward into those approximations or an unknown callee.

use crate::det::{HashMap, HashSet};

use crate::App;
use crate::diagnostic::Diagnostic;
use crate::dialect::{Association, LibraryClass, MethodDef, MethodReceiver};
use crate::expr::{Expr, ExprNode, LValue};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;
use crate::ty::Ty;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let contracts = SourceContractIndex::new(app);
    let scoped = if app
        .models
        .iter()
        .any(|m| m.methods().any(|m| m.params.iter().any(|p| p.forwarding)))
    {
        let scopes = crate::lower::scope_chain::build_scope_registry(&app.models);
        let assocs = crate::lower::scope_chain::build_assoc_registry(&app.models);
        crate::lower::scope_chain::survey_assoc_class_methods(app, &assocs, &scopes).1
    } else {
        vec![]
    };
    for (owner, method) in methods(app) {
        if let Some(formal) = method.unsupported_formals {
            out.push(Diagnostic::unsupported(
                method.name_span,
                None,
                "parameter declaration",
                formal.description(),
            ));
        } else if method.params.iter().any(crate::dialect::Param::is_nameless_rest)
            && contracts.fragments.get(owner).copied().unwrap_or(0) > 1
        {
            // A reopened class's fragments are not shown to be emitted
            // together, so its admitted `*` is not shown to run.
            out.push(Diagnostic::unsupported(
                method.name_span,
                None,
                "anonymous positional rest declaration",
                "a nameless rest in a reopened class fragment is not verified",
            ));
        }
        if method.params.iter().any(|p| p.forwarding) {
            // Ingest applies these rewrites regardless of receiver ownership,
            // including user-defined methods. Refuse the declaration itself:
            // surveying the rewritten calls cannot prove their source dispatch.
            // Supporting these selectors needs separate dispatch provenance.
            let reason = if matches!(method.name.as_str(), "reverse_merge" | "exists?") {
                Some(
                    "full declarations of this method overlap an ingest-time call rewrite whose source dispatch is not retained",
                )
            } else if !contracts.verified_hierarchy(owner, &mut HashSet::default()) {
                Some("forwarding declarations in reopened class fragments are not verified")
            } else if method.receiver == MethodReceiver::Class
                && app.models.iter().any(|m| &m.name == owner)
                && (crate::lower::scope_chain::mentions_bare_chain_start(&method.body)
                    || scoped
                        .iter()
                        .any(|d| &d.model == owner && d.method == method.name))
            {
                Some("full forwarding cannot use the relation-threading argument ABI")
            } else if contracts.unretained.contains(&method.name_span) {
                Some("model method synthesis does not preserve this source declaration")
            } else {
                None
            };
            if let Some(reason) = reason {
                out.push(Diagnostic::unsupported(
                    method.name_span,
                    None,
                    "full argument forwarding",
                    reason,
                ));
            }
        }
        walk(app, &contracts, owner, method, &method.body, &mut out);
        for default in method.params.iter().filter_map(|p| p.default.as_ref()) {
            walk(app, &contracts, owner, method, default, &mut out);
        }
    }
    out.extend(keyword_splats_into_nameless_rest_with_index(app, &contracts));
    for (span, policy) in keyword_calls_with_index(app, &contracts) {
        if matches!(policy, KeywordPolicy::Refuse | KeywordPolicy::RefuseOrdinarySuper) {
            out.push(keyword_refusal(span, policy));
        }
    }
    out
}

/// The ordinary `**value` lowering passes the hash positionally, which a
/// rest destination takes as one more positional. Refuse it where the
/// selector can name an admitted nameless rest: the method itself, `new`
/// for `initialize`, and the dynamic dispatchers. A call whose source
/// destination resolves exactly (`destination`: a constant naming its
/// class, a verified hierarchy, every self-dispatch override) to methods
/// without one is admitted. So is `Const.m(**h)` / `Const.new(**h)`
/// whose receiver's Ruby method lookup provably cannot reach any rest
/// owner of the selector, even when its destination is an unmodeled
/// ancestor ([`RestReach`]). Anything less stays refused by selector.
/// Reifying such a method (`method(:m)`) is refused outright.
/// An implicit `super` in an override of such a selector with keyword
/// parameters (kept or flattened) passes the override's lowered
/// keywords as positionals, so it is refused too.
pub(crate) fn keyword_splats_into_nameless_rest(app: &App) -> Vec<Diagnostic> {
    keyword_splats_into_nameless_rest_with_index(app, &SourceContractIndex::new(app))
}

fn keyword_splats_into_nameless_rest_with_index(app: &App, contracts: &SourceContractIndex<'_>) -> Vec<Diagnostic> {
    const DISPATCH: [&str; 8] =
        ["send", "public_send", "__send__", "call", "[]", "===", "curry", "instance_exec"];
    /// The call's destination, and every override a self-send may reach,
    /// are source methods none of which takes a nameless rest.
    fn proven_without_rest(
        app: &App,
        contracts: &SourceContractIndex<'_>,
        context: Option<(&ClassId, &MethodDef)>,
        e: &Expr,
    ) -> bool {
        let rest = |m: &MethodDef| m.params.iter().any(crate::dialect::Param::is_nameless_rest);
        destination(app, contracts, context, e).is_some_and(|(m, _)| !rest(m))
            && virtual_destinations(contracts, context, e).is_ok_and(|v| v.iter().all(|(m, _)| !rest(m)))
    }
    #[allow(clippy::too_many_arguments)]
    fn visit(
        app: &App,
        contracts: &SourceContractIndex<'_>,
        reach: &RestReach<'_>,
        selectors: &HashSet<&Symbol>,
        context: Option<(&ClassId, &MethodDef)>,
        runtime_super: bool,
        e: &Expr,
        out: &mut Vec<Diagnostic>,
    ) {
        let enclosing = context.map(|(_, m)| m);
        if matches!(&*e.node, ExprNode::Super { args: None })
            && !runtime_super
            && enclosing.is_some_and(|m| selectors.contains(&m.name) && m.params.iter().any(|p| p.keyword || p.from_keyword || p.from_kwrest))
        {
            out.push(Diagnostic::unsupported(
                e.span,
                None,
                "implicit super into an anonymous positional rest",
                "the override's lowered parameters are not shown to pass as written",
            ));
        }
        let named = |s: &Symbol| {
            selectors.contains(s)
                || DISPATCH.contains(&s.as_str())
                || (s.as_str() == "new" && selectors.iter().any(|s| s.as_str() == "initialize"))
        };
        let (reaches, args) = match &*e.node {
            ExprNode::Send { method, args, .. } => (named(method), args.as_slice()),
            ExprNode::Super { args: Some(args) } => (enclosing.is_some_and(|m| named(&m.name)), args.as_slice()),
            _ => (false, [].as_slice()),
        };
        // A reified method reaches callers (`yield`, `&block`) no
        // selector names, and `yield` keeps no keyword splat to check.
        // A reifier or a dispatcher can itself be reified or dispatched
        // (`public_send(:method, :m)`, `instance_method(:send)`), and a
        // name computed at run time, or splatted, may be any of these,
        // so each is taken to reach the rest.
        const REIFY: [&str; 4] = ["method", "public_method", "instance_method", "public_instance_method"];
        const SEND: [&str; 3] = ["send", "public_send", "__send__"];
        fn literal(a: &Expr) -> Option<&str> {
            match &*a.node {
                ExprNode::Lit { value: crate::expr::Literal::Sym { value } } => Some(value.as_str()),
                ExprNode::Lit { value: crate::expr::Literal::Str { value } } => Some(value.as_str()),
                _ => None,
            }
        }
        let meta = |n: &str| REIFY.contains(&n) || SEND.contains(&n);
        let reaching = |n: &str| meta(n) || selectors.iter().any(|s| s.as_str() == n);
        let may_reach = |a: &Expr| literal(a).is_none_or(reaching);
        let reified = match &*e.node {
            ExprNode::MethodRef { name, .. } => reaching(name.as_str()),
            ExprNode::Send { method, args, .. } if REIFY.contains(&method.as_str()) => {
                args.first().is_some_and(may_reach)
            }
            ExprNode::Send { method, args, .. } if SEND.contains(&method.as_str()) => {
                args.first().is_some_and(|a| literal(a).is_none_or(meta))
                    && (args.iter().any(|a| matches!(&*a.node, ExprNode::Splat { .. }))
                        || args.iter().skip(1).any(may_reach))
            }
            _ => false,
        };
        if reified {
            out.push(Diagnostic::unsupported(
                e.span,
                None,
                "method object of an anonymous positional rest",
                "the callers a reified method reaches are not shown to pass as written",
            ));
        }
        if reaches
            && args.iter().any(|a| matches!(&*a.node, ExprNode::KeywordSplat { .. }))
            && !proven_without_rest(app, contracts, context, e)
            && !reach.constant_receiver_reaches_none(contracts, e)
        {
            out.push(Diagnostic::unsupported(
                e.span,
                None,
                "keyword splat into an anonymous positional rest",
                "the keyword splat lowering would pass the hash as a positional",
            ));
        }
        e.node.for_each_child(&mut |c| visit(app, contracts, reach, selectors, context, runtime_super, c, out));
    }
    let rests: Vec<(&ClassId, &Symbol)> = methods(app)
        .filter(|(_, m)| m.params.iter().any(crate::dialect::Param::is_nameless_rest))
        .map(|(owner, m)| (owner, &m.name))
        .collect();
    let selectors: HashSet<&Symbol> = rests.iter().map(|(_, name)| *name).collect();
    let mut out = Vec::new();
    if selectors.is_empty() {
        return out;
    }
    // A `Struct.new`/`Data.define` body class that includes nothing has
    // the runtime factory class as its next ancestor, so an implicit
    // `super` there reaches the runtime's method (`Data#initialize`
    // takes the members as keywords), not an app nameless rest -- unless
    // the app itself reopens one of those runtime ancestors with one.
    const FACTORY_ANCESTORS: [&str; 5] = ["Struct", "Data", "Object", "Kernel", "BasicObject"];
    let runtime_super_owners: HashSet<&ClassId> = app
        .library_classes
        .iter()
        .filter(|c| {
            matches!(c.origin, Some(crate::dialect::LibraryClassOrigin::FactoryClass { .. }))
                && c.includes.is_empty()
        })
        .map(|c| &c.name)
        .collect();
    let runtime_super = |owner: &ClassId, m: &MethodDef| {
        runtime_super_owners.contains(owner)
            && m.receiver == MethodReceiver::Instance
            && !rests
                .iter()
                .any(|(o, n)| **n == m.name && FACTORY_ANCESTORS.contains(&o.0.as_str()))
    };
    let reach = RestReach::new(app);
    for (owner, method) in methods(app) {
        let runtime = runtime_super(owner, method);
        visit(app, contracts, &reach, &selectors, Some((owner, method)), runtime, &method.body, &mut out);
        for default in method.params.iter().filter_map(|p| p.default.as_ref()) {
            visit(app, contracts, &reach, &selectors, Some((owner, method)), runtime, default, &mut out);
        }
    }
    crate::lower::for_each_forwarding_body_ref(app, &mut |e| {
        visit(app, contracts, &reach, &selectors, None, false, e, &mut out)
    });
    // `delegate` writes a zero-arg forwarder, or none (a string name, a
    // test class); the library-class pass leaves such a declaration in
    // `unknown_calls`, and a controller's stays a body item, so refuse
    // it here by name. `delegate_missing_to` names no method at all.
    let delegations = classes(app).flat_map(|c| c.unknown_calls.iter()).chain(
        app.controllers.iter().flat_map(|c| c.body.iter()).filter_map(|item| match item {
            crate::dialect::ControllerBodyItem::Unknown { expr, .. } => Some(expr),
            _ => None,
        }),
    );
    for call in delegations {
        if let ExprNode::Send { recv: None, method, args, .. } = &*call.node
            && (method.as_str() == "delegate_missing_to"
                || method.as_str() == "delegate"
                    && args.iter().any(|a| match &*a.node {
                        ExprNode::Lit { value: crate::expr::Literal::Sym { value } } => selectors.contains(value),
                        ExprNode::Lit { value: crate::expr::Literal::Str { value } } => {
                            selectors.iter().any(|s| s.as_str() == value)
                        }
                        _ => false,
                    }))
        {
            out.push(Diagnostic::unsupported(
                call.span,
                None,
                "delegate to an anonymous positional rest",
                "no forwarder is written with the rest's arity",
            ));
        }
    }
    out
}

/// Proof that `Const.m(.., **h)` cannot reach an admitted nameless rest,
/// from Ruby's method lookup rather than from a resolved destination.
///
/// `C.new` looks `new` up on C's singleton class (the singleton classes
/// of C's superclass chain, the modules extended into them, then `Class`,
/// `Module`, `Object`, `Kernel`, `BasicObject`), and `Class#new` looks
/// `initialize` up on C's ancestors: C's superclass chain and the modules
/// mixed into it. `C.m` is the first lookup alone. So a rest owner of the
/// selector (`new` and `initialize` for `new`) can be reached only as
///
/// * a module. Mixins are also applied by calls this index does not see
///   (hedwig's `bulk_process_by` runs `include(Hedwig::Bulk::Performer)`,
///   Core's `include(mod)` takes a computed module), so a module owner
///   may sit in any lookup and is never excluded;
/// * `Object`, `BasicObject`, `Kernel`, `Module` or `Class`, which every
///   lookup reaches;
/// * a class in C's superclass chain: the only classes among C's
///   ancestors, since a module's ancestors are modules. Owners and the
///   chain's written superclasses are compared by last segment, so a
///   relative spelling (`< Base` inside `module A`) cannot hide `A::Base`;
/// * a class beyond a superclass the app does not model. Gem code cannot
///   name a fresh app class, so an app-local owner is not there. The
///   owners gem code can name are excluded only while the chain is
///   closed: classes of installed gem source the app compiles (`vendor/`,
///   `gems/`, `<gem>-<version>/`), which another gem may subclass; core,
///   stdlib and RBI-declared names the app reopens; and the top-level
///   `Application*` bases engines refer to.
///
/// A reopened owner (more than one fragment) is refused: its fragments
/// are not shown to be one class. A written superclass that is an app
/// constant rather than a modeled class (`Base = build_class`) is app
/// code the index cannot see, so no proof is given. A chain root with no
/// written superclass is `Object`'s child unless it is a core, stdlib or
/// RBI-declared name, a factory class or a `Struct.new` superclass,
/// whose real superclass is not the app's declaration: then the chain is
/// open, as it is for a superclass whose top-level spelling is unmodeled.
///
/// Like `destination`, this takes a modeled class's recorded superclass
/// as its superclass; a superclass expression the ingest cannot record
/// (other than `Struct.new`) reads as none. `send`-family selectors are
/// computed and `instance_exec` runs a block, so neither is admitted.
struct RestReach<'a> {
    app: &'a App,
    rests: Vec<(&'a ClassId, &'a MethodDef)>,
    /// Modeled classes and modules: `true` when any fragment is a module.
    module: HashMap<&'a ClassId, bool>,
    /// Factory classes and `Struct.new` superclasses.
    synthesized: HashSet<&'a ClassId>,
    /// Last segments of app constants that are not modeled classes.
    constants: HashSet<&'a str>,
    /// `<gem>-<version>` directory names of the locked gems.
    installed: HashSet<String>,
}

fn last_segment(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

impl<'a> RestReach<'a> {
    fn new(app: &'a App) -> Self {
        let rests = methods(app)
            .filter(|(_, m)| m.params.iter().any(crate::dialect::Param::is_nameless_rest))
            .collect();
        let mut module: HashMap<&ClassId, bool> = HashMap::default();
        let mut synthesized = HashSet::default();
        let mut constants = HashSet::default();
        for class in classes(app) {
            *module.entry(&class.name).or_insert(false) |= class.is_module;
            if matches!(
                class.origin,
                Some(
                    crate::dialect::LibraryClassOrigin::FactoryClass { .. }
                        | crate::dialect::LibraryClassOrigin::StructSuperclass { .. }
                )
            ) {
                synthesized.insert(&class.name);
            }
            constants.extend(class.constants.iter().map(|(name, _)| last_segment(name.as_str())));
        }
        let assigned = |e: &'a Expr| match &*e.node {
            ExprNode::Assign { target: LValue::Const { path }, .. } => path.last().map(|s| s.as_str()),
            _ => None,
        };
        for model in &app.models {
            module.entry(&model.name).or_insert(false);
            constants.extend(model.body.iter().filter_map(|item| match item {
                crate::dialect::ModelBodyItem::Unknown { expr, .. } => assigned(expr),
                _ => None,
            }));
        }
        for controller in &app.controllers {
            constants.extend(controller.body.iter().filter_map(|item| match item {
                crate::dialect::ControllerBodyItem::Unknown { expr, .. } => assigned(expr),
                _ => None,
            }));
        }
        for test in &app.test_modules {
            constants.extend(test.constants.iter().map(|(name, _)| last_segment(name.as_str())));
        }
        let installed = app
            .gem_lock
            .iter()
            .flat_map(|lock| lock.specs.iter().map(|(name, version)| format!("{name}-{version}")))
            .collect();
        Self { app, rests, module, synthesized, constants, installed }
    }

    /// `Const.m(.., **h)`: no rest owner of the selector is reachable
    /// from the constant's class (see the type's documentation).
    fn constant_receiver_reaches_none(&self, contracts: &SourceContractIndex<'_>, e: &Expr) -> bool {
        let ExprNode::Send { recv: Some(recv), method, .. } = &*e.node else { return false };
        if matches!(method.as_str(), "send" | "public_send" | "__send__" | "instance_exec") {
            return false;
        }
        let (ExprNode::Const { .. }, Some(Ty::Class { id, .. })) = (&*recv.node, recv.ty.as_ref()) else {
            return false;
        };
        if !constant_names_class(recv, id) || !self.module.contains_key(id) {
            return false;
        }
        let Some((chain, open)) = self.superclass_chain(contracts, id) else { return false };
        let new = method.as_str() == "new";
        self.rests
            .iter()
            .filter(|(_, m)| m.name == *method || (new && m.name.as_str() == "initialize"))
            .all(|(owner, m)| self.excluded(contracts, owner, m, &chain, open))
    }

    fn excluded(
        &self,
        contracts: &SourceContractIndex<'_>,
        owner: &ClassId,
        method: &MethodDef,
        chain: &[String],
        open: bool,
    ) -> bool {
        let name = owner.0.as_str();
        !matches!(name, "Object" | "BasicObject" | "Kernel" | "Module" | "Class")
            && contracts.fragments.get(owner).copied() == Some(1)
            && self.module.get(owner) == Some(&false)
            && !chain.iter().any(|n| last_segment(n) == last_segment(name))
            && !(open && self.gem_nameable(name, method.name_span))
    }

    /// A name gem code can refer to, so a gem class may descend from it.
    fn gem_nameable(&self, name: &str, span: Span) -> bool {
        self.foreign(name) || (!name.contains("::") && name.starts_with("Application")) || self.gem_source(span)
    }

    /// Ruby's, its standard library's, or a gem RBI's name.
    fn foreign(&self, name: &str) -> bool {
        use super::registry::gem_boundary::{RUBY_CORE, RUBY_STDLIB};
        let top = name.split("::").next().unwrap_or(name);
        RUBY_CORE.contains(&top) || RUBY_STDLIB.contains(&top) || self.app.gem_boundary.gem_of(name).is_some()
    }

    /// Declared in installed gem source the app compiles. A span with no
    /// file has no provenance and counts as gem source.
    fn gem_source(&self, span: Span) -> bool {
        let Some(file) = (span.file.0 as usize).checked_sub(1).and_then(|i| self.app.sources.get(i)) else {
            return true;
        };
        file.path
            .split(['/', '\\'])
            .any(|part| part == "vendor" || part == "gems" || self.installed.contains(part))
    }

    /// The receiver's superclass chain as modeled and as written, and
    /// whether it continues past what the app models. `None` when a
    /// written superclass may be an unmodeled app constant.
    fn superclass_chain(&self, contracts: &SourceContractIndex<'_>, receiver: &ClassId) -> Option<(Vec<String>, bool)> {
        let mut names = Vec::new();
        let mut open = false;
        let mut pending = vec![receiver.clone()];
        let mut seen = HashSet::default();
        while let Some(node) = pending.pop() {
            if !seen.insert(node.clone()) {
                continue;
            }
            names.push(node.0.as_str().to_string());
            if self.synthesized.contains(&node) {
                open = true;
            }
            let Some(parent) = contracts.parent(&node) else {
                if self.foreign(node.0.as_str()) {
                    open = true;
                }
                continue;
            };
            let written = parent.0.as_str().trim_start_matches("::");
            if self.constants.contains(last_segment(written)) {
                return None;
            }
            names.push(written.to_string());
            // Ruby resolves the spelling innermost scope first; follow
            // every modeled candidate. The chain is open unless the
            // top-level spelling itself is modeled: a compact `class
            // A::B < P` resolves `P` at the top level only.
            let mut scope: Vec<&str> = node.0.as_str().split("::").collect();
            scope.pop();
            let mut candidates = Vec::new();
            while !scope.is_empty() {
                candidates.push(ClassId(Symbol::from(format!("{}::{written}", scope.join("::")))));
                scope.pop();
            }
            let top = ClassId(Symbol::from(written));
            if !contracts.fragments.contains_key(&top) {
                open = true;
            }
            candidates.push(top);
            pending.extend(candidates.into_iter().filter(|c| contracts.fragments.contains_key(c)));
        }
        Some((names, open))
    }
}

fn classes(app: &App) -> impl Iterator<Item = &LibraryClass> {
    app.library_classes
        .iter()
        .chain(app.rails_application.iter())
        .chain(app.test_modules.iter().flat_map(|t| t.inner_classes.iter()))
}

pub(crate) fn methods(app: &App) -> impl Iterator<Item = (&ClassId, &MethodDef)> {
    classes(app)
        .flat_map(|c| c.methods.iter().map(move |m| (&c.name, m)))
        .chain(app.models.iter().flat_map(|c| {
            c.methods()
                .chain(c.associations().flat_map(|a| match a {
                    Association::HasMany { extension, .. } => extension.as_slice().iter(),
                    _ => [].iter(),
                }))
                .map(move |m| (&c.name, m))
        }))
        .chain(
            app.test_modules
                .iter()
                .flat_map(|t| t.helpers.iter().map(move |m| (&t.name, m))),
        )
}

fn walk(
    app: &App,
    contracts: &SourceContractIndex<'_>,
    owner: &ClassId,
    enclosing: &MethodDef,
    e: &Expr,
    out: &mut Vec<Diagnostic>,
) {
    let forwards_keywords = match &*e.node {
        ExprNode::Send { args, .. } | ExprNode::Super { args: Some(args) } => args
            .iter()
            .any(|a| matches!(&*a.node, ExprNode::ForwardKeywords)),
        _ => false,
    };
    if forwards_keywords {
        let source_ok = enclosing.params.iter().any(|p| {
            p.keyword && p.rest && p.name.as_str().is_empty() && !p.forwarding
        });
        let resolved = destination(app, contracts, Some((owner, enclosing)), e);
        let reason = if !source_ok {
            Some("anonymous keyword forwarding has no enclosing anonymous keyword-rest declaration")
        } else if enclosing.unsupported_formals.is_some()
            || contracts.unretained.contains(&enclosing.name_span)
            || !contracts.verified_hierarchy(owner, &mut HashSet::default())
        {
            Some("anonymous keyword forwarding source declaration cannot be verified")
        } else {
            keyword_contract_error(Some((owner, enclosing)), e, resolved, contracts)
        };
        if let Some(reason) = reason {
            out.push(Diagnostic::unsupported(
                e.span,
                None,
                "anonymous keyword forwarding",
                reason,
            ));
        }
    }
    let forwards = match &*e.node {
        ExprNode::Send { args, .. } => has_forwarding(args),
        ExprNode::Super { args } => args
            .as_ref()
            .map_or(enclosing.params.iter().any(|p| p.forwarding), |a| {
                has_forwarding(a)
            }),
        _ => false,
    };
    if forwards {
        let reason = if enclosing.unsupported_formals.is_some() {
            Some("forwarding source has an unrepresented parameter declaration")
        } else if !enclosing.params.iter().any(|p| p.forwarding) {
            Some("forwarding call has no preserved forwarding declaration")
        } else if native_core_destination(app, e) {
            None
        } else {
            contract_error(
                Some((owner, enclosing)),
                e,
                destination(app, contracts, Some((owner, enclosing)), e),
                contracts,
            )
        };
        if let Some(reason) = reason {
            out.push(Diagnostic::unsupported(
                e.span,
                None,
                "full argument forwarding",
                reason,
            ));
        }
    }
    e.node
        .for_each_child(&mut |c| walk(app, contracts, owner, enclosing, c, out));
}

/// `recv.m(...)` where `recv` is typed as a Ruby core value (a Hash,
/// Array, String, Symbol, Integer or Float) and the app reopens that
/// class with no method `m`: the destination is the core method itself,
/// whose argument contract is Ruby's own, so `...` hands it exactly
/// what the caller passed. (`delegate :fetch, to: :hash` on a Hash
/// target is the shape.)
fn native_core_destination(app: &App, e: &Expr) -> bool {
    let ExprNode::Send { recv: Some(recv), method, .. } = &*e.node else { return false };
    let class = match recv.ty.as_ref() {
        Some(Ty::Hash { .. }) => "Hash",
        Some(Ty::Array { .. }) => "Array",
        Some(Ty::Str) => "String",
        Some(Ty::Sym) => "Symbol",
        Some(Ty::Int) => "Integer",
        Some(Ty::Float) => "Float",
        _ => return false,
    };
    !app.library_classes
        .iter()
        .filter(|c| c.name.0.as_str() == class)
        .any(|c| c.methods.iter().any(|m| &m.name == method))
}

fn accepts_keywords(method: &MethodDef) -> bool {
    method.params.iter().any(|p| p.forwarding || p.keyword)
}

fn keyword_contract_error(
    context: Option<(&ClassId, &MethodDef)>,
    call: &Expr,
    resolved: Option<(&MethodDef, bool)>,
    contracts: &SourceContractIndex<'_>,
) -> Option<&'static str> {
    let Some((method, _)) = resolved else {
        return Some("forwarding destination's declaration cannot be verified");
    };
    if method.unsupported_formals.is_some() {
        return Some("forwarding destination has an unrepresented parameter declaration");
    }
    if method.params.iter().any(|p| p.from_keyword || p.from_kwrest) {
        return Some("forwarding destination has flattened keyword parameters");
    }
    if contracts.unretained.contains(&method.name_span) {
        return Some("model method synthesis does not preserve this source declaration");
    }
    let Ok(destinations) = virtual_destinations(contracts, context, call) else {
        return Some("forwarding destination's declaration cannot be verified");
    };
    if !accepts_keywords(method)
        || destinations.iter().any(|(candidate, _)| {
            !accepts_keywords(candidate)
                || candidate.unsupported_formals.is_some()
                || candidate.params.iter().any(|p| p.from_keyword || p.from_kwrest)
                || contracts.unretained.contains(&candidate.name_span)
        })
    {
        return Some("forwarding destination has no verified keyword parameter ABI");
    }
    None
}

fn destination<'a>(
    app: &'a App,
    contracts: &SourceContractIndex<'a>,
    context: Option<(&ClassId, &MethodDef)>,
    e: &Expr,
) -> Option<(&'a MethodDef, bool)> {
    let owner_dependent = match &*e.node {
        ExprNode::Send { recv: None, .. } | ExprNode::Super { .. } => true,
        ExprNode::Send { recv: Some(r), .. } => matches!(&*r.node, ExprNode::SelfRef),
        _ => false,
    };
    if owner_dependent
        && context.is_some_and(|(owner, method)| association_method(app, owner, method))
    {
        return None;
    }
    match &*e.node {
        ExprNode::Send { recv, method, .. } => {
            let receiver = match recv.as_ref().map(|r| &*r.node) {
                None | Some(ExprNode::SelfRef) => context?.1.receiver,
                Some(ExprNode::Const { .. }) => MethodReceiver::Class,
                _ => MethodReceiver::Instance,
            };
            let class = match recv {
                None => context.map(|(owner, _)| owner),
                Some(r) if matches!(&*r.node, ExprNode::SelfRef) => context.map(|(owner, _)| owner),
                Some(r) => match (&*r.node, r.ty.as_ref()) {
                    (ExprNode::Const { .. }, Some(Ty::Class { id, .. })) => {
                        // Constructor monomorphization can retain the base's
                        // inferred type on a newly named subclass receiver.
                        // Do not admit the base contract for that source name.
                        constant_names_class(r, id).then_some(id)
                    }
                    (_, Some(Ty::Class { id, .. })) => Some(id),
                    _ => None,
                },
            };
            class.and_then(|c| {
                // Ty::Class conflates class objects with instances. Do
                // not trust an instance or a copied constructor-result type
                // without matching source-side receiver provenance.
                let ambiguous = recv.as_ref().is_some_and(|r| {
                    !matches!(&*r.node, ExprNode::Const { .. } | ExprNode::SelfRef)
                });
                if ambiguous
                    && !constructed_instance(
                        contracts,
                        context.map(|(_, m)| m),
                        recv.as_ref().unwrap(),
                        c,
                    )
                {
                    return None;
                }
                // A source-defined class .new wins over constructor
                // dispatch; an instance method called new is ordinary.
                contracts.effective_call(c, method, receiver)
            })
        }
        ExprNode::Super { .. } => {
            let (owner, enclosing) = context?;
            contracts.ancestors(
                owner,
                &enclosing.name,
                enclosing.receiver,
                &mut HashSet::default(),
            )
        }
        _ => None,
    }
}

fn association_method(app: &App, owner: &ClassId, method: &MethodDef) -> bool {
    app.models.iter().filter(|m| &m.name == owner).any(|m| {
        m.associations().any(|a| match a {
            Association::HasMany { extension, .. } => {
                extension.iter().any(|e| std::ptr::eq(e, method))
            }
            _ => false,
        })
    })
}

fn declaration_error((method, _model): (&MethodDef, bool)) -> Option<&'static str> {
    // An admitted nameless rest is kept only as written on the CRuby
    // tree; forwarding into it is not part of that admission.
    if method.unsupported_formals.is_some()
        || method.params.iter().any(crate::dialect::Param::is_nameless_rest)
    {
        Some("forwarding destination has an unrepresented parameter declaration")
    } else if method.has_anonymous_block {
        Some("forwarding destination's anonymous block binding is not preserved hygienically")
    } else if method
        .params
        .iter()
        .any(|p| p.from_keyword || p.from_kwrest)
    {
        Some("forwarding destination has flattened keyword parameters")
    } else {
        None
    }
}

fn contract_error(
    context: Option<(&ClassId, &MethodDef)>,
    call: &Expr,
    resolved: Option<(&MethodDef, bool)>,
    contracts: &SourceContractIndex<'_>,
) -> Option<&'static str> {
    let Some(resolved) = resolved else {
        return Some("forwarding destination's declaration cannot be verified");
    };
    if let Some(error) = declaration_error(resolved) {
        return Some(error);
    }
    if contracts.unretained.contains(&resolved.0.name_span) {
        return Some("model method synthesis does not preserve this source declaration");
    }
    // A self-send in a base method dispatches on the actual subclass.
    // Verify reachable overrides too, not merely the lexical base's method.
    let candidates = match virtual_destinations(contracts, context, call) {
        Ok(candidates) => candidates,
        Err(reason) => return Some(reason),
    };
    for candidate in candidates {
        if declaration_error(candidate).is_some()
            || contracts.unretained.contains(&candidate.0.name_span)
        {
            return Some("forwarding self-dispatch may reach an unpreserved subclass contract");
        }
    }
    None
}

fn virtual_destinations<'a>(
    contracts: &SourceContractIndex<'a>,
    context: Option<(&ClassId, &MethodDef)>,
    call: &Expr,
) -> Result<Vec<(&'a MethodDef, bool)>, &'static str> {
    let (Some((owner, enclosing)), ExprNode::Send { recv, method, .. }) = (context, &*call.node)
    else {
        return Ok(vec![]);
    };
    if recv
        .as_ref()
        .is_some_and(|r| !matches!(&*r.node, ExprNode::SelfRef))
    {
        return Ok(vec![]);
    }
    fn descends(
        contracts: &SourceContractIndex<'_>,
        child: &ClassId,
        owner: &ClassId,
        seen: &mut HashSet<ClassId>,
    ) -> bool {
        child == owner
            || (seen.insert(child.clone())
                && (contracts
                    .parent(child)
                    .is_some_and(|p| descends(contracts, p, owner, seen))
                    || contracts
                        .includes(child)
                        .iter()
                        .any(|m| descends(contracts, m, owner, seen))))
    }
    let mut candidates = Vec::new();
    for child in
        contracts.virtual_owners.iter().copied().filter(|child| {
            *child != owner && descends(contracts, child, owner, &mut HashSet::default())
        })
    {
        // Completeness depends on the descendant's entire reachable lookup
        // chain, including reopened included modules.
        if !contracts.verified_hierarchy(child, &mut HashSet::default()) {
            return Err(
                "forwarding virtual dispatch through reopened fragments cannot be verified",
            );
        }
        if let Some(candidate) = contracts.effective_call(child, method, enclosing.receiver) {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeywordPolicy {
    Native,
    Legacy,
    Refuse,
    RefuseOrdinarySuper,
}

pub(crate) fn keyword_refusal(span: Span, policy: KeywordPolicy) -> Diagnostic {
    if policy == KeywordPolicy::RefuseOrdinarySuper {
        return Diagnostic::unsupported(
            span,
            None,
            "keyword splat in ordinary super",
            "super destination's native or lowered argument ABI cannot be verified",
        );
    }
    Diagnostic::unsupported(
        span,
        None,
        "keyword splat into full argument forwarding",
        "keyword producer's full forwarding destination cannot be verified",
    )
}

/// Classify before projection. Source copies sharing a span must agree:
/// a cloned concern body can have different contracts in two includers.
pub(crate) fn keyword_calls(app: &App) -> HashMap<Span, KeywordPolicy> {
    let contracts = SourceContractIndex::new(app);
    keyword_calls_with_index(app, &contracts)
}

fn keyword_calls_with_index(
    app: &App,
    contracts: &SourceContractIndex<'_>,
) -> HashMap<Span, KeywordPolicy> {
    fn visit(
        app: &App,
        contracts: &SourceContractIndex<'_>,
        context: Option<(&ClassId, &MethodDef)>,
        e: &Expr,
        plans: &mut HashMap<Span, KeywordPolicy>,
        fallback: bool,
    ) {
        let args = match &*e.node {
            ExprNode::Send { args, .. } => Some(args.as_slice()),
            ExprNode::Super { args: Some(args) } => Some(args.as_slice()),
            _ => None,
        };
        if args.is_some_and(|a| {
            a.iter()
                .any(|a| matches!(&*a.node, ExprNode::KeywordSplat { .. }))
        }) && !(fallback && plans.contains_key(&e.span))
        {
            let policy = if !possible_full_destination(contracts, context, e) {
                // Unrelated selectors cannot reach a full contract. Avoid
                // scanning receiver provenance and hierarchies for each of
                // their keyword calls in a large application.
                if matches!(&*e.node, ExprNode::Super { .. })
                    && destination(app, contracts, context, e).is_none()
                {
                    // An absent full selector does not prove that erasing **
                    // matches an external parent's native keyword ABI.
                    KeywordPolicy::RefuseOrdinarySuper
                } else {
                    KeywordPolicy::Legacy
                }
            } else {
                let resolved = destination(app, contracts, context, e);
                if resolved.is_some_and(|(m, _)| m.params.iter().any(|p| p.forwarding)) {
                    if contract_error(context, e, resolved, contracts).is_none() {
                        KeywordPolicy::Native
                    } else {
                        KeywordPolicy::Refuse
                    }
                } else if resolved.is_none()
                    || virtual_destinations(contracts, context, e).map_or(true, |v| {
                        v.iter().any(|(m, _)| m.params.iter().any(|p| p.forwarding))
                    })
                {
                    // Ordinary lexical method, full virtual override: neither
                    // legacy expansion nor native-only admission proves both.
                    KeywordPolicy::Refuse
                } else {
                    // Deliberately preserve the old ordinary-callee lowering;
                    // this prerequisite does not fix its missing/extra-key paths.
                    KeywordPolicy::Legacy
                }
            };
            plans
                .entry(e.span)
                .and_modify(|previous| {
                    if *previous != policy {
                        *previous = KeywordPolicy::Refuse;
                    }
                })
                .or_insert(policy);
        }
        e.node
            .for_each_child(&mut |c| visit(app, contracts, context, c, plans, fallback));
    }
    let mut plans = HashMap::default();
    for (owner, method) in methods(app) {
        visit(
            app,
            contracts,
            Some((owner, method)),
            &method.body,
            &mut plans,
            false,
        );
        for default in method.params.iter().filter_map(|p| p.default.as_ref()) {
            visit(
                app,
                contracts,
                Some((owner, method)),
                default,
                &mut plans,
                false,
            );
        }
    }
    crate::lower::for_each_forwarding_body_ref(app, &mut |e| {
        visit(app, contracts, None, e, &mut plans, true)
    });
    plans
}

fn possible_full_destination(
    contracts: &SourceContractIndex<'_>,
    context: Option<(&ClassId, &MethodDef)>,
    call: &Expr,
) -> bool {
    let method = match &*call.node {
        ExprNode::Send { method, .. } => method,
        // Super uses the enclosing selector, not every full selector in the
        // app. Unrelated framework parents keep their ordinary keyword ABI.
        ExprNode::Super { .. } => match context {
            Some((_, enclosing)) => &enclosing.name,
            None => return true,
        },
        _ => return true,
    };
    // No declaration proof: refuse if this selector can name a source
    // full forwarder. Unrelated catalog/framework calls keep legacy lowering.
    contracts.full_selectors.contains(method)
        || (method.as_str() == "new"
            && contracts
                .full_selectors
                .contains(&Symbol::from("initialize")))
}

fn has_forwarding(args: &[Expr]) -> bool {
    args.iter()
        .any(|a| matches!(&*a.node, ExprNode::ForwardArgs))
}

fn constant_names_class(expr: &Expr, class: &ClassId) -> bool {
    let ExprNode::Const { path } = &*expr.node else {
        return false;
    };
    let spelling = path
        .iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join("::");
    class.0.as_str() == spelling || class.0.as_str().ends_with(&format!("::{spelling}"))
}

/// Narrow source-side provenance: a built-in constructor, or its single
/// unconditional local binding before this read. Arbitrary factories, joins,
/// reassignments and closures are deliberately not an instance proof.
fn constructed_instance(
    contracts: &SourceContractIndex<'_>,
    method: Option<&MethodDef>,
    recv: &Expr,
    class: &ClassId,
) -> bool {
    let built_in_new = |value: &Expr| {
        let ExprNode::Send {
            recv: Some(r),
            method,
            ..
        } = &*value.node
        else {
            return false;
        };
        constant_names_class(r, class)
            && method.as_str() == "new"
            && matches!(r.ty.as_ref(), Some(Ty::Class { id, .. }) if id == class)
            && contracts
                .declaration(
                    class,
                    &Symbol::from("new"),
                    MethodReceiver::Class,
                    &mut HashSet::default(),
                )
                .is_none()
    };
    if built_in_new(recv) {
        return true;
    }
    let Some(method) = method else { return false };
    let ExprNode::Var { name, .. } = &*recv.node else {
        return false;
    };
    let ExprNode::Seq { exprs } = &*method.body.node else {
        return false;
    };
    let binding = exprs.iter().any(|e| {
        matches!(&*e.node, ExprNode::Assign { target: LValue::Var { name: n, .. }, value }
            if n == name && !e.span.is_synthetic() && e.span.file == recv.span.file
                && e.span.end <= recv.span.start && built_in_new(value))
    });
    fn writes(e: &Expr, name: &Symbol, count: &mut usize, closure: &mut bool) {
        match &*e.node {
            ExprNode::Assign {
                target: LValue::Var { name: n, .. },
                ..
            }
            | ExprNode::OpAssign {
                target: LValue::Var { name: n, .. },
                ..
            } if n == name => *count += 1,
            ExprNode::MultiAssign { targets, .. } => {
                *count += targets
                    .iter()
                    .filter(|t| matches!(t, LValue::Var { name: n, .. } if n == name))
                    .count();
            }
            ExprNode::Lambda { .. } => *closure = true,
            _ => {}
        }
        e.node
            .for_each_child(&mut |c| writes(c, name, count, closure));
    }
    let mut count = 0;
    let mut closure = false;
    writes(&method.body, name, &mut count, &mut closure);
    binding && count == 1 && !closure
}

/// Source declarations, not inferred arity: inference does not retain the
/// flattening provenance. Own method, last-included mixin, then superclass.
/// The inferred ClassInfo resolver cannot supply this contract, and the
/// tree-shaker's conservative runtime lookup intentionally conflates receiver
/// sides and aliases. Neither is an admission check for source semantics.
struct SourceContractIndex<'a> {
    parents: HashMap<ClassId, &'a ClassId>,
    includes: HashMap<ClassId, Vec<ClassId>>,
    fragments: HashMap<ClassId, usize>,
    instance: HashMap<(ClassId, Symbol), (&'a MethodDef, bool)>,
    class: HashMap<(ClassId, Symbol), (&'a MethodDef, bool)>,
    virtual_owners: Vec<&'a ClassId>,
    full_selectors: HashSet<Symbol>,
    unretained: HashSet<Span>,
}

impl<'a> SourceContractIndex<'a> {
    fn new(app: &'a App) -> Self {
        let mut index = Self {
            parents: HashMap::default(),
            includes: HashMap::default(),
            fragments: HashMap::default(),
            instance: HashMap::default(),
            class: HashMap::default(),
            virtual_owners: Vec::new(),
            full_selectors: HashSet::default(),
            unretained: HashSet::default(),
        };
        let mut class_owners = HashSet::default();
        for class in classes(app) {
            class_owners.insert(class.name.clone());
            index.add_fragment(
                &class.name,
                class.parent.as_ref(),
                class.includes.iter().cloned(),
            );
            index.add_methods(&class.name, class.methods.iter(), false);
            index.virtual_owners.push(&class.name);
        }
        for model in &app.models {
            index.add_fragment(
                &model.name,
                model.parent.as_ref(),
                super::model_includes(model).into_iter(),
            );
            if !class_owners.contains(&model.name) {
                index.add_methods(&model.name, model.methods(), true);
            }
            index.virtual_owners.push(&model.name);
        }
        for module in &app.test_modules {
            index.add_fragment(
                &module.name,
                module.parent.as_ref(),
                module.includes.iter().cloned(),
            );
            if !class_owners.contains(&module.name)
                && !app.models.iter().any(|model| model.name == module.name)
            {
                index.add_methods(&module.name, module.helpers.iter(), false);
            }
        }
        // Includes association-extension methods, matching the public method
        // inventory used by the former conservative selector scan.
        index.full_selectors.extend(
            methods(app)
                .filter(|(_, method)| method.params.iter().any(|p| p.forwarding))
                .map(|(_, method)| method.name.clone()),
        );
        if !index.full_selectors.is_empty() {
            // Ordinary inherited contracts matter only when a packet is sent
            // to them. Looking up every unrelated selector for every model
            // makes large-app transpilation quadratic in the source inventory.
            let mut names = index.full_selectors.clone();
            fn collect(e: &Expr, names: &mut HashSet<Symbol>) {
                if let ExprNode::Send { method, args, .. } = &*e.node
                    && has_forwarding(args)
                {
                    names.insert(method.clone());
                    if method.as_str() == "new" {
                        names.insert(Symbol::from("initialize"));
                    }
                }
                e.node.for_each_child(&mut |child| collect(child, names));
            }
            for (_, method) in methods(app) {
                collect(&method.body, &mut names);
                for default in method.params.iter().filter_map(|p| p.default.as_ref()) {
                    collect(default, &mut names);
                }
            }
            index.unretained = crate::lower::model_to_library::unretained_model_contracts(app, |model| {
                let mut inherited = Vec::new();
                for name in &names {
                    for receiver in [MethodReceiver::Instance, MethodReceiver::Class] {
                        if let Some((method, _)) = index.declaration(
                            &model.name, name, receiver, &mut HashSet::default(),
                        ) && !model.methods().any(|own| std::ptr::eq(own, method)) {
                            inherited.push(method);
                        }
                    }
                }
                inherited
            });
        }
        index
    }

    fn add_fragment(
        &mut self,
        owner: &ClassId,
        parent: Option<&'a ClassId>,
        includes: impl Iterator<Item = ClassId>,
    ) {
        *self.fragments.entry(owner.clone()).or_default() += 1;
        if let Some(parent) = parent {
            self.parents.entry(owner.clone()).or_insert(parent);
        }
        self.includes
            .entry(owner.clone())
            .or_default()
            .extend(includes);
    }

    fn add_methods(
        &mut self,
        owner: &ClassId,
        methods: impl Iterator<Item = &'a MethodDef>,
        model: bool,
    ) {
        for method in methods {
            let declarations = match method.receiver {
                MethodReceiver::Instance => &mut self.instance,
                MethodReceiver::Class => &mut self.class,
            };
            declarations.insert((owner.clone(), method.name.clone()), (method, model));
        }
    }

    fn parent(&self, owner: &ClassId) -> Option<&'a ClassId> {
        self.parents.get(owner).copied()
    }

    fn includes(&self, owner: &ClassId) -> &[ClassId] {
        self.includes.get(owner).map_or(&[], Vec::as_slice)
    }

    fn verified_hierarchy(&self, owner: &ClassId, seen: &mut HashSet<ClassId>) -> bool {
        if !seen.insert(owner.clone()) {
            return true;
        }
        self.fragments.get(owner).copied().unwrap_or(0) <= 1
            && self
                .parent(owner)
                .is_none_or(|parent| self.verified_hierarchy(parent, seen))
            && self
                .includes(owner)
                .iter()
                .all(|module| self.verified_hierarchy(module, seen))
    }

    fn declaration(
        &self,
        owner: &ClassId,
        name: &Symbol,
        receiver: MethodReceiver,
        seen: &mut HashSet<ClassId>,
    ) -> Option<(&'a MethodDef, bool)> {
        if !self.verified_hierarchy(owner, &mut HashSet::default()) || !seen.insert(owner.clone()) {
            return None;
        }
        let declarations = match receiver {
            MethodReceiver::Instance => &self.instance,
            MethodReceiver::Class => &self.class,
        };
        declarations
            .get(&(owner.clone(), name.clone()))
            .copied()
            .or_else(|| self.ancestors(owner, name, receiver, seen))
    }

    fn ancestors(
        &self,
        owner: &ClassId,
        name: &Symbol,
        receiver: MethodReceiver,
        seen: &mut HashSet<ClassId>,
    ) -> Option<(&'a MethodDef, bool)> {
        if receiver == MethodReceiver::Instance {
            for module in self.includes(owner).iter().rev() {
                if let Some(method) = self.declaration(module, name, receiver, seen) {
                    return Some(method);
                }
            }
        }
        self.parent(owner)
            .and_then(|parent| self.declaration(parent, name, receiver, seen))
    }

    fn effective_call(
        &self,
        owner: &ClassId,
        name: &Symbol,
        receiver: MethodReceiver,
    ) -> Option<(&'a MethodDef, bool)> {
        self.declaration(owner, name, receiver, &mut HashSet::default())
            .or_else(|| {
                (name.as_str() == "new" && receiver == MethodReceiver::Class)
                    .then(|| {
                        self.declaration(
                            owner,
                            &Symbol::from("initialize"),
                            MethodReceiver::Instance,
                            &mut HashSet::default(),
                        )
                    })
                    .flatten()
            })
    }
}
