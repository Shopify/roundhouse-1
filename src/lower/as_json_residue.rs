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
    super::for_each_hook_body(app, &mut walk);
    super::for_each_test_body(app, &mut walk);
}

fn walk(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut walk);
    rewrite_node(expr);
}

pub(crate) fn rewrite_node(expr: &mut Expr) {
    if expr.decisions & OBJECT_AS_JSON == 0 {
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
