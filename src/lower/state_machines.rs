//! The state_machines gem's `state_machine` class macro on a model.
//!
//! `state_machine :financial_status, namespace: "payment" do ... end`
//! builds a `StateMachines::Machine` when the class body runs: its
//! states, events, transitions and callbacks, the helper methods
//! (`payment_pending?`, `pend_payment`, `can_pend_payment?`, ...), the
//! `with_*` scopes, the initial state and, through the matching
//! integration (state_machines-activemodel / -activerecord), the
//! validation, the save hook and the transaction around an event.
//! `audit_trail` in the block (state_machines-audit_trail) logs each
//! transition through the owner's `has_many` of `<Owner><Attr>Transition`.
//!
//! These gems are plain Ruby. The CRuby tree loads them natively from
//! the gem home, where the app requires them (Core's component entry
//! files, ledger), and their `require "active_record"` / `"active_model"`
//! answer from the runtime ports (`project::FRAMEWORK_FEATURE_PORTS`),
//! so the integration matches the runtime's `ActiveRecord::Base`. The
//! call is therefore carried as a class-body statement of the lowered
//! model (`gem_port_macro_calls`), in body order, and runs against the
//! loaded gem as it does in Rails. A class method of the model's own
//! named `state_machine` answers instead, so its call is not one.

use crate::dialect::{Model, ModelBodyItem};
use crate::expr::{Expr, ExprNode};

/// The macro's name.
pub(crate) const MACRO: &str = "state_machine";

/// Whether `expr` is a `state_machine` call on the class (bare or
/// `self.`), with or without a block.
pub(crate) fn is_state_machine_call(expr: &Expr) -> bool {
    let ExprNode::Send { recv, method, .. } = &*expr.node else { return false };
    method.as_str() == MACRO && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
}

/// `model`'s body statements that call `state_machine`.
pub(crate) fn state_machine_calls(model: &Model) -> impl Iterator<Item = &Expr> {
    model.body.iter().filter_map(|item| match item {
        ModelBodyItem::Unknown { expr, .. } if is_state_machine_call(expr) => Some(expr),
        _ => None,
    })
}
