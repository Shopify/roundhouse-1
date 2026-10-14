//! The controller actions an app's controller tests name.
//!
//! A controller's `process_action` dispatcher (the before/around/after
//! filter chain, `rescue_from` handlers and the case over its actions) is
//! synthesized for the actions a route reaches. An app whose routes are
//! not ingested (Core's `config/routes.rb` is a DSL over every component's
//! routes) has no routed action, so every controller is emitted without
//! one and its filters never run. `ActionController::TestCase#process`
//! drives that dispatcher, so an action a controller test requests
//! (`get :index`, `post(:create, params: …)`) counts as routed for the
//! controller the test class names (`Admin::OrderRisksControllerTest` →
//! `Admin::OrderRisksController`).

use crate::app::App;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};

/// The verbs ActionController::TestCase defines, and `process` itself.
const REQUEST_METHODS: [&str; 7] = ["get", "post", "patch", "put", "delete", "head", "process"];

/// `(controller, action)` for each action a controller test of the app
/// requests, in test order. A test class whose controller the app does
/// not define names nothing.
pub fn controller_test_actions(app: &App) -> Vec<(ClassId, Symbol)> {
    let mut out: Vec<(ClassId, Symbol)> = Vec::new();
    for tm in &app.test_modules {
        let Some(controller) = test_controller(app, &tm.name, tm.target.as_ref()) else {
            continue;
        };
        let mut names: Vec<Symbol> = Vec::new();
        let mut visit = |e: &Expr| collect(e, &mut names);
        if let Some(setup) = &tm.setup {
            visit(setup);
        }
        for t in &tm.tests {
            visit(&t.body);
        }
        for m in &tm.helpers {
            visit(&m.body);
        }
        for name in names {
            if !out.iter().any(|(c, a)| c == &controller && a == &name) {
                out.push((controller.clone(), name));
            }
        }
    }
    out
}

/// The app controller a test class exercises: Rails' default,
/// `determine_default_controller_class` — the class name without its
/// `Test` suffix — or the test's resolved target when that is a
/// controller.
fn test_controller(app: &App, test: &ClassId, target: Option<&ClassId>) -> Option<ClassId> {
    let is_controller = |id: &ClassId| app.controllers.iter().any(|c| &c.name == id);
    if let Some(target) = target {
        if is_controller(target) {
            return Some(target.clone());
        }
    }
    let stem = test.0.as_str().strip_suffix("Test")?;
    let named = ClassId(Symbol::from(stem));
    is_controller(&named).then_some(named)
}

fn collect(e: &Expr, names: &mut Vec<Symbol>) {
    if let ExprNode::Send { recv, method, args, .. } = &*e.node {
        let own_call = match recv {
            None => true,
            Some(r) => matches!(&*r.node, ExprNode::SelfRef),
        };
        if own_call && REQUEST_METHODS.contains(&method.as_str()) {
            if let Some(Expr { node, .. }) = args.first() {
                if let ExprNode::Lit { value: Literal::Sym { value } } = &**node {
                    names.push(value.clone());
                }
            }
        }
    }
    e.node.for_each_child(&mut |child| collect(child, names));
}
