//! `value.as_json(options)` that no static method answers →
//! `ActiveSupport::JSON::Encoding.as_json(value, options)`.
//!
//! ActiveSupport answers `as_json` on every object by reopening Object,
//! the core classes, Time / Date / DateTime, Struct, Enumerable and the
//! rest (core_ext/object/json.rb). No tree reopens them: the CRuby tree
//! does not load ActiveSupport, and the other targets cannot. The
//! analyzer flags each send whose receiver's own table has no `as_json`
//! (`expr::OBJECT_AS_JSON`, `analyze::body`'s `as_json_residue`), and
//! this pass makes the value an ARGUMENT of the port's function
//! (runtime/spinel/active_support_json.rb), which is the same dispatch
//! written out: an object with an `as_json` of its own (a record, a
//! serializer, an app value class) is asked, everything else follows the
//! table. `tests/active_support_json_runtime.rb` holds it to the pinned
//! activesupport, value by value.
//!
//! A bare `as_json(...)` sends to `self`. `project` refuses the call on
//! a tree that does not carry the port (`analyze::runtime_reads`); the
//! CRuby and JRuby trees carry it wherever emitted app code names it.

use crate::app::App;
use crate::expr::{Expr, ExprNode, OBJECT_AS_JSON};
use crate::ident::Symbol;

/// The port's receiver, as emitted.
pub const ENCODING_PATH: [&str; 3] = ["ActiveSupport", "JSON", "Encoding"];

pub fn apply_as_json_residue_lowering(app: &mut App) {
    // Only the PostgreSQL CRuby tree carries the port for every receiver;
    // any other tree answers a Hash, Array or value read out of one with
    // `ActiveSupport::JSON.jsonable` (`symbolize_keys`), which Spinel can run.
    let port_for_containers = app.primary_database_adapter() == Some(&crate::database::DatabaseAdapter::Postgresql);
    let mut walk = |expr: &mut Expr| walk(expr, port_for_containers);
    super::for_each_hook_body(app, &mut walk);
    super::for_each_test_body(app, &mut walk);
}

fn walk(expr: &mut Expr, port_for_containers: bool) {
    expr.node.for_each_child_mut(&mut |child| walk(child, port_for_containers));
    rewrite_node(expr, port_for_containers);
}

pub(crate) fn rewrite_node(expr: &mut Expr, port_for_containers: bool) {
    if expr.decisions & OBJECT_AS_JSON == 0 {
        return;
    }
    if !port_for_containers && jsonable_container(expr) {
        return;
    }
    let span = expr.span;
    let ExprNode::Send { recv, method, args, block, parenthesized } = &mut *expr.node else { return };
    if method.as_str() != "as_json" || block.is_some() || args.len() > 1 {
        return;
    }
    let receiver = match recv.take() {
        Some(r) => r,
        None => Expr::new(span, ExprNode::SelfRef),
    };
    *recv = Some(Expr::new(span, ExprNode::Const {
        path: ENCODING_PATH.iter().map(|s| Symbol::from(*s)).collect(),
    }));
    args.insert(0, receiver);
    *parenthesized = true;
    expr.decisions &= !OBJECT_AS_JSON;
}

/// An `as_json` another pass answers off the PostgreSQL tree: `symbolize_keys`
/// with `ActiveSupport::JSON.jsonable`, or the params class the controller
/// lowering synthesizes an `as_json` on.
fn jsonable_container(expr: &Expr) -> bool {
    let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &*expr.node else { return false };
    method.as_str() == "as_json"
        && args.is_empty()
        && (matches!(r.ty.as_ref(), Some(crate::ty::Ty::Hash { .. } | crate::ty::Ty::Array { .. }))
            || super::symbolize_keys::is_request_params(r)
            || super::symbolize_keys::is_open_value(r)
            || matches!(r.ty.as_ref(), Some(crate::ty::Ty::Class { id, .. }) if id.0.as_str() == "ActionController::Parameters"))
}
