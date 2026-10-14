//! A column's storage ivar kept apart from the model's own ivars.
//!
//! Rails keeps attribute values in `@attributes`, so a model's own
//! `@requested_payment_method` is unrelated to the column of that name:
//!
//! ```ruby
//! def requested_payment_method
//!   @requested_payment_method ||= PaymentMethod.build(super)
//! end
//! ```
//!
//! memoizes a `PaymentMethod` over the String `super` reads. The lowering
//! stores a column's value in the ivar named after it
//! (`schema::col_storage_name`), so that memo read the column's raw String
//! back and `||=` never built the object. Here, for the columns whose
//! storage ivar a method the model's source defines also writes or reads
//! (an authored `@<col>`), every compiled use of the storage moves to
//! `@__attr_<col>`: the synthesized methods whole, and the compiled reads
//! inside an authored method (a synthetic-span `@<col>`, which the source
//! never wrote). The authored `@<col>` keeps being the model's own.
//!
//! A table's models share the rename (an STI child's own `@<col>` renames
//! the parent's storage too, as both compile the same columns). A column
//! no authored code names keeps its storage ivar, so the emit is
//! unchanged for it. Ruby-family only: the strict targets declare a
//! field per name and have no second ivar to collide with.

use std::collections::{BTreeMap, BTreeSet};

use crate::App;
use crate::dialect::LibraryClass;
use crate::expr::{Expr, ExprNode, LValue};
use crate::ident::Symbol;
use crate::lower::model_to_library::schema::col_storage_name;

fn slot_name(storage: &str) -> Symbol {
    Symbol::from(format!("__attr_{storage}"))
}

/// Every `@name` an authored (source-spanned) expression reads or writes.
fn authored_ivars(e: &Expr, out: &mut BTreeSet<Symbol>) {
    let named = |lv: &LValue, out: &mut BTreeSet<Symbol>| {
        if let LValue::Ivar { name } = lv {
            out.insert(name.clone());
        }
    };
    if !e.span.is_synthetic() {
        match &*e.node {
            ExprNode::Ivar { name } => {
                out.insert(name.clone());
            }
            ExprNode::Assign { target, .. } | ExprNode::OpAssign { target, .. } => named(target, out),
            ExprNode::MultiAssign { targets, .. } => targets.iter().for_each(|t| named(t, out)),
            _ => {}
        }
    }
    e.node.for_each_child(&mut |c| authored_ivars(c, out));
}

/// Move `@<storage>` to its slot under `renames`; with `only_compiled`,
/// only the uses the lowering synthesized (a synthetic span).
fn rename(e: &mut Expr, renames: &BTreeMap<Symbol, Symbol>, only_compiled: bool) {
    let applies = !only_compiled || e.span.is_synthetic();
    let swap = |lv: &mut LValue| {
        if let LValue::Ivar { name } = lv {
            if let Some(to) = renames.get(name) {
                *name = to.clone();
            }
        }
    };
    if applies {
        match &mut *e.node {
            ExprNode::Ivar { name } => {
                if let Some(to) = renames.get(name) {
                    *name = to.clone();
                }
            }
            ExprNode::Assign { target, .. } | ExprNode::OpAssign { target, .. } => swap(target),
            ExprNode::MultiAssign { targets, .. } => targets.iter_mut().for_each(swap),
            _ => {}
        }
    }
    e.node.for_each_child_mut(&mut |c| rename(c, renames, only_compiled));
}

pub(crate) fn apply(lcs: &mut [LibraryClass], app: &App) {
    // Per table: the storage ivars of its columns an authored method names.
    let mut colliding: BTreeMap<&str, BTreeSet<Symbol>> = BTreeMap::new();
    for model in &app.models {
        let Some(table) = app.schema.tables.get(&model.table.0) else { continue };
        let Some(lc) = lcs.iter().find(|lc| lc.name == model.name) else { continue };
        let storage: BTreeSet<Symbol> = table.columns.iter().map(col_storage_name).collect();
        let mut authored = BTreeSet::new();
        for m in lc.methods.iter().filter(|m| !m.name_span.is_synthetic()) {
            authored_ivars(&m.body, &mut authored);
        }
        let hit: Vec<Symbol> = authored.intersection(&storage).cloned().collect();
        if !hit.is_empty() {
            colliding.entry(model.table.0.as_str()).or_default().extend(hit);
        }
    }
    for model in &app.models {
        let Some(names) = colliding.get(model.table.0.as_str()) else { continue };
        let Some(lc) = lcs.iter_mut().find(|lc| lc.name == model.name) else { continue };
        let renames: BTreeMap<Symbol, Symbol> = names.iter().map(|n| (n.clone(), slot_name(n.as_str()))).collect();
        for m in lc.methods.iter_mut() {
            let compiled = m.name_span.is_synthetic();
            rename(&mut m.body, &renames, !compiled);
        }
    }
}
