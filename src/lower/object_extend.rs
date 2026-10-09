//! `obj.extend Mod` on an INSTANCE — mixing a module into one live
//! object — is outside the subset every target compiles: the class
//! graph and each object's method table are fixed at compile time, and
//! spinel refuses the call outright ("Object#extend is not supported by
//! AOT compilation: mixing a module into a live object needs a
//! per-object method table"). Left in the emit it is a compile error on
//! the strict lanes, and on spinel one such line keeps a whole test
//! file from LINKING, charging every test in it — campfire's
//! `action_text_attachment_test` makes a `Room` attachable for one test
//! (`rooms(:pets).tap { |r| r.extend ActionText::Attachable }`) and the
//! file's other two tests were never run.
//!
//! So the site becomes a raise stub carrying the report, the same
//! `Expr.diagnostic` short-circuit the arel stub uses: the test that
//! wrote it fails at that line with the reason, the file links, and
//! the ledger names the construct. A WARNING, not an error — the strict
//! archive build refuses an error, and a test that reaches for a
//! dynamic shape is a gap to record, not a reason to withhold the
//! whole tree.
//!
//! In a TEST the stub is the whole test body, not the one site: what
//! follows the `extend` is written against the object it produced
//! (`rooms(:pets).attachable_sgid` — a method `Room` does not have,
//! which is what the `extend` was for), so it cannot type either, and
//! a raise at the first line leaves the rest to fail compilation one
//! line down. The test fails with the reason on every lane; the file
//! links. App code keeps the site-level stub: a raise there is the
//! right runtime answer, and what follows it is the app's own.
//!
//! Only the instance form. `extend Foo` in a class body (no receiver)
//! and `Foo.extend Bar` (a constant receiver) restructure a CLASS,
//! which the ingest reads as a body item and the strict targets model
//! statically; neither reaches this pass as a Send on an object.
//!
//! Nor does `base.extend(ClassMethods)` in a module's `included(base)`
//! hook (or `prepended(base)`, a class's `inherited(sub)`): Ruby passes
//! the includer — a class or module — as the hook's parameter, so this
//! too restructures a class, and it is how a concern's `ClassMethods`
//! reaches its includer. The Ruby family runs it; the targets without
//! mixins carry the same methods as the concern splice's static copies,
//! and `project::target_files` stubs the call for them
//! ([`stub_module_hook_extends`]).
//!
//! The same holds for a module's `extended(base)` hook, which Ruby calls
//! with the object `extend` was called on. In app code that is a class:
//! a class body's `extend M`, `Const.extend(M)`, another hook's
//! `base.extend(M)`. The instance form `obj.extend(M)` is itself the
//! stub here, so it raises before `M.extended` could run. Core's sharded
//! models get their lazy-loading verifier this way
//! (`ShardedTable::ClassMethods.extended(base)`:
//! `base.extend(Podding::Verifier::LazyLoadingConcern) unless
//! Rails.application.config.eager_load`); stubbing that call dropped
//! the verifier and raised in every sharded model's class body.

use crate::app::App;
use crate::det::HashSet;
use crate::dialect::MethodReceiver;
use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::Symbol;
use crate::span::Span;

const CONSTRUCT: &str = "Object#extend";
const DETAIL: &str = "mixing a module into one live object needs a per-object method \
                      table, which no compiled target has; the site raises";

pub fn apply_object_extend_stub(app: &mut App) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let class_extends = module_hook_extends(app);
    super::for_each_hook_body(app, &mut |e| stub_except(e, &class_extends, &mut diags));
    for view in &mut app.views {
        stub(&mut view.body, &mut diags);
    }
    for tm in &mut app.test_modules {
        if let Some(setup) = &mut tm.setup {
            stub(setup, &mut diags);
        }
        for t in &mut tm.tests {
            let before = diags.len();
            stub(&mut t.body, &mut diags);
            if diags.len() > before {
                t.body = stub_expr(t.body.span);
            }
        }
        for m in &mut tm.helpers {
            stub(&mut m.body, &mut diags);
        }
    }
    diags
}

fn stub(expr: &mut Expr, diags: &mut Vec<Diagnostic>) {
    stub_except(expr, &HashSet::default(), diags)
}

fn stub_except(expr: &mut Expr, class_extends: &HashSet<Span>, diags: &mut Vec<Diagnostic>) {
    expr.node.for_each_child_mut(&mut |c| stub_except(c, class_extends, diags));
    let ExprNode::Send { recv: Some(recv), method, .. } = &*expr.node else { return };
    if method.as_str() != "extend" || class_extends.contains(&expr.span) {
        return;
    }
    // A constant or `self` receiver is the class-level form.
    if matches!(&*recv.node, ExprNode::Const { .. } | ExprNode::SelfRef) {
        return;
    }
    let mut d = Diagnostic::unsupported(expr.span, None, CONSTRUCT, DETAIL);
    d.severity = Severity::Warning;
    diags.push(d);
    *expr = stub_expr(expr.span);
}

/// A nil carrying the report — the ruby emitter renders it as the raise.
fn stub_expr(span: crate::span::Span) -> Expr {
    let mut e = Expr::new(span, ExprNode::Lit { value: Literal::Nil });
    e.diagnostic = Some(DiagnosticKind::Unsupported {
        target: None,
        construct: Symbol::from(CONSTRUCT),
        detail: DETAIL.to_string(),
    });
    e
}

/// Each `param.extend(Const, …)` in a library module's `included(param)` /
/// `prepended(param)` / `extended(param)` hook or a class's
/// `inherited(param)` hook, by span: Ruby calls these hooks with the
/// class or module being extended, so the receiver is a class object.
/// (`extended(param)` gets whatever `extend` was called on; the instance
/// form `obj.extend(M)` is the raise stub, so it never reaches the hook.)
/// A body that reassigns the parameter proves nothing.
/// Also each `k.extend(Const, …)` on a local only ever assigned a
/// `Class.new` / `Module.new`, which is a class object as well.
fn module_hook_extends(app: &App) -> HashSet<Span> {
    fn assigns(expr: &Expr, name: &Symbol) -> bool {
        let here = match &*expr.node {
            ExprNode::Assign { target: LValue::Var { name: n, .. }, .. }
            | ExprNode::OpAssign { target: LValue::Var { name: n, .. }, .. } => n == name,
            _ => false,
        };
        let mut found = here;
        expr.node.for_each_child(&mut |c| found |= assigns(c, name));
        found
    }
    fn collect(expr: &Expr, name: &Symbol, out: &mut HashSet<Span>) {
        expr.node.for_each_child(&mut |c| collect(c, name, out));
        let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*expr.node else { return };
        if method.as_str() == "extend"
            && matches!(&*recv.node, ExprNode::Var { name: n, .. } if n == name)
            && !args.is_empty()
            && args.iter().all(|a| matches!(&*a.node, ExprNode::Const { .. }))
        {
            out.insert(expr.span);
        }
    }
    let mut out = HashSet::default();
    for lc in &app.library_classes {
        for m in &lc.methods {
            let hook = match m.name.as_str() {
                "included" | "prepended" | "extended" => lc.is_module,
                "inherited" => !lc.is_module,
                _ => false,
            };
            if !hook || m.receiver != MethodReceiver::Class {
                continue;
            }
            let [param] = m.params.as_slice() else { continue };
            if param.keyword || param.rest || param.forwarding || param.name.as_str().is_empty() {
                continue;
            }
            if assigns(&m.body, &param.name) {
                continue;
            }
            collect(&m.body, &param.name, &mut out);
        }
    }
    // `k = Class.new(Base)` then `k.extend(Forwardable)`: a local every
    // write of which is a fresh `Class.new` / `Module.new` holds a class
    // object, so its `extend` restructures a class too (Core's
    // `Checkouts::Core::Facade.define`, run at load by the constants
    // that build each facade).
    super::for_each_hook_body_ref(app, &mut |body| {
        let mut writes: Vec<(Symbol, bool)> = Vec::new();
        local_writes(body, &mut writes);
        let mut seen: Vec<&Symbol> = Vec::new();
        for (name, _) in &writes {
            if seen.contains(&name) {
                continue;
            }
            seen.push(name);
            if writes.iter().filter(|(n, _)| n == name).all(|(_, class_new)| *class_new) {
                collect(body, name, &mut out);
            }
        }
    });
    out
}

/// Every write of a local in `expr`, with whether its value is a
/// `Class.new(…)` / `Module.new(…)` call (optionally `::`-rooted, and
/// through a `#:` cast).
fn local_writes(expr: &Expr, out: &mut Vec<(Symbol, bool)>) {
    expr.node.for_each_child(&mut |c| local_writes(c, out));
    match &*expr.node {
        ExprNode::Assign { target: LValue::Var { name, .. }, value } => {
            // `k = Class.new(Base) #: singleton(Base)` reads as a cast.
            let mut value = value;
            while let ExprNode::Cast { value: inner, .. } = &*value.node {
                value = inner;
            }
            let class_new = matches!(&*value.node, ExprNode::Send { recv: Some(r), method, .. }
                if method.as_str() == "new"
                    && matches!(&*r.node, ExprNode::Const { path }
                        if matches!(path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().as_slice(),
                            ["Class"] | ["Module"])));
            out.push((name.clone(), class_new));
        }
        ExprNode::OpAssign { target: LValue::Var { name, .. }, .. } => out.push((name.clone(), false)),
        _ => {}
    }
}

/// For a target without mixins: the hook extends [`apply_object_extend_stub`]
/// kept become the raise stub there, and a hook that did nothing else —
/// the complete `def self.included(base) base.extend(ClassMethods) end`
/// bridge — is dropped instead, since the concern splice's copies already
/// are its whole effect and a hook that raised would fail the include.
pub fn stub_module_hook_extends(app: &mut App) -> Vec<Diagnostic> {
    let class_extends = module_hook_extends(app);
    if class_extends.is_empty() {
        return Vec::new();
    }
    let carriers: HashSet<String> = app.library_classes.iter()
        .filter(|lc| lc.is_module)
        .filter_map(|lc| lc.name.0.as_str().strip_suffix("::ClassMethods").map(str::to_string))
        .collect();
    // `ClassMethods` as written, or as constant qualification spelled it
    // (`Owner::ClassMethods`, optionally rooted).
    let names_carrier = |path: &[crate::ident::Symbol], owner: &str| {
        let written: Vec<&str> = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect();
        written == ["ClassMethods"] || written.join("::") == format!("{owner}::ClassMethods")
    };
    let bridge = |e: &Expr, owner: &str| {
        class_extends.contains(&e.span)
            && matches!(&*e.node, ExprNode::Send { args, .. } if args.iter().all(|a| matches!(&*a.node,
                ExprNode::Const { path } if names_carrier(path, owner))))
    };
    for lc in &mut app.library_classes {
        if !lc.is_module || !carriers.contains(lc.name.0.as_str()) {
            continue;
        }
        let owner = lc.name.0.as_str().to_string();
        lc.methods.retain(|m| {
            let only_bridge = match &*m.body.node {
                ExprNode::Seq { exprs } => !exprs.is_empty() && exprs.iter().all(|e| bridge(e, &owner)),
                _ => bridge(&m.body, &owner),
            };
            !(m.receiver == MethodReceiver::Class && m.name.as_str() == "included" && only_bridge)
        });
    }
    let mut diags = Vec::new();
    let mut f = |e: &mut Expr| {
        restub(e, &class_extends, &mut diags);
    };
    super::for_each_hook_body(app, &mut f);
    diags
}

fn restub(expr: &mut Expr, class_extends: &HashSet<Span>, diags: &mut Vec<Diagnostic>) {
    expr.node.for_each_child_mut(&mut |c| restub(c, class_extends, diags));
    if class_extends.contains(&expr.span) && matches!(&*expr.node, ExprNode::Send { .. }) {
        let mut d = Diagnostic::unsupported(expr.span, None, CONSTRUCT, DETAIL);
        d.severity = Severity::Warning;
        diags.push(d);
        *expr = stub_expr(expr.span);
    }
}
