//! A controller that `include`s an `app/helpers` module got a copy of
//! each of its methods (`splice_concerns_into_controllers`), and the copy
//! is typed as controller code. Views do not use it — they call the
//! helper module itself — so in a copy no controller calls, a `t` is the
//! view's, never the controller's: marked here
//! ([`crate::expr::HELPER_COPY_TRANSLATE`]) so it takes ActionView's
//! translate, as the helper module's own body does. mastodon's
//! `RegistrationHelper#terms_agreement_label` is the case: three
//! controllers include it for `allowed_registration?`, none calls the
//! label, and its `_html` translations were refused as controller code.
//!
//! Not dropped: the copy is where the helper's body is checked at all
//! (the module's own body is not reported).
//!
//! A copy counts as called when any controller body names it: a call, or
//! a String / Symbol literal (`before_action :x`, `helper_method :x`,
//! `send(:x)`), or a route's action.

use crate::det::HashSet;

use crate::app::App;
use crate::dialect::ControllerBodyItem;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;

pub(super) fn mark(app: &mut App) {
    let helpers: HashSet<_> = app.helper_method_index.values().cloned().collect();
    if helpers.is_empty() || app.concern_spliced_actions.is_empty() {
        return;
    }
    let routed: HashSet<String> =
        crate::lower::routes::flatten_routes_for_helpers(app).into_iter().map(|r| r.action.as_str().to_string()).collect();
    let named = names_in_controllers(app);
    let mut uncalled: Vec<(crate::ident::ClassId, Symbol)> = Vec::new();
    for (controller, spliced) in &app.concern_spliced_actions {
        for (method, module) in spliced {
            let name = method.as_str();
            if helpers.contains(module) && !named.contains(name) && !routed.contains(name) {
                uncalled.push((controller.clone(), method.clone()));
            }
        }
    }
    for (controller, method) in uncalled {
        let Some(c) = app.controllers.iter_mut().find(|c| c.name == controller) else { continue };
        for item in &mut c.body {
            if let ControllerBodyItem::Action { action, .. } = item
                && action.name == method
            {
                mark_translate(&mut action.body);
            }
        }
    }
}

fn mark_translate(e: &mut Expr) {
    e.node.for_each_child_mut(&mut |c| mark_translate(c));
    if let ExprNode::Send { recv: None, method, .. } = &*e.node
        && matches!(method.as_str(), "t" | "translate")
    {
        e.decisions |= crate::expr::HELPER_COPY_TRANSLATE;
    }
}

/// Every method name a controller body calls or spells as a literal.
fn names_in_controllers(app: &App) -> HashSet<String> {
    let mut out = HashSet::default();
    for c in &app.controllers {
        for item in &c.body {
            let exprs: Vec<&Expr> = match item {
                ControllerBodyItem::Action { action, .. } => {
                    let mut v = vec![&action.body];
                    v.extend(action.opt_params.iter().map(|(_, d)| d));
                    v.extend(action.kw_params.iter().filter_map(|(_, d)| d.as_ref()));
                    v
                }
                ControllerBodyItem::ClassMethod { method, .. } => vec![&method.body],
                ControllerBodyItem::ClassIvarInit { expr, .. } | ControllerBodyItem::Unknown { expr, .. } => vec![expr],
                ControllerBodyItem::Filter { filter, .. } => {
                    out.insert(filter.target.as_str().to_string());
                    out.extend(filter.if_cond.iter().chain(filter.unless_cond.iter()).map(|s| s.as_str().to_string()));
                    filter.if_cond_expr.iter().chain(filter.unless_cond_expr.iter()).chain(filter.block.iter()).collect()
                }
                _ => Vec::new(),
            };
            for e in exprs {
                collect(e, &mut out);
            }
        }
    }
    out
}

fn collect(e: &Expr, out: &mut HashSet<String>) {
    e.node.for_each_child(&mut |c| collect(c, out));
    match &*e.node {
        ExprNode::Send { method, .. } => {
            out.insert(method.as_str().to_string());
        }
        ExprNode::Lit { value: Literal::Sym { value } } => {
            out.insert(value.as_str().to_string());
        }
        ExprNode::Lit { value: Literal::Str { value } } => {
            out.insert(value.clone());
        }
        _ => {}
    }
}
