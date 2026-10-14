//! The `attribute` declarations a subclass of a TABLELESS model makes.
//!
//! ```text
//! class Task                       # tableless: includes ActiveModel::Attributes
//!   include ActiveModel::Attributes
//!   include ActiveModel::Validations
//! end
//! class CloseTask < Task
//!   attribute :shop_id, :integer
//!   attribute :note, :string, default: "none"
//! end
//! ```
//!
//! A subclass declares no table, so ingest keeps it as a library class and
//! its class-body statements as expressions in `unknown_calls`. For a model
//! with a table the declarations are columns or retypes (`analyze::
//! retypable_attributes`); a tableless base has no columns, so an `attribute`
//! in a subclass was dropped and the class had no `shop_id=`.
//!
//! `ActiveModel::Attributes::ClassMethods#attribute` (activemodel/lib/
//! active_model/attributes.rb) defines the reader and writer of the name and
//! casts what the writer is given with the declared type
//! (`ActiveModel::Type.lookup(:integer).cast`). The reader answers the
//! default (cast the same way) until the attribute is written. This pass
//! gives the subclass the same two methods:
//!
//! ```text
//! def shop_id = (instance_variable_defined?(:@shop_id) ? @shop_id : <default>)
//! def shop_id=(value) = @shop_id = ActiveModel::Type.lookup(:integer).cast(value)
//! ```
//!
//! Only the plain form is read: a symbol name, an optional symbol type, an
//! optional literal `default:`. Any other option or a computed type stays in
//! `unknown_calls` and is reported as before.

use crate::app::App;
use crate::dialect::{LibraryClass, MethodReceiver};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;
use crate::lower::sti_subclass_validations::{instance_method, param, Lineage, model_ancestry};
use crate::span::Span;

/// One `attribute :name[, :type][, default: literal]` declaration.
struct Declaration {
    name: Symbol,
    type_name: Option<Symbol>,
    default: Option<Expr>,
}

fn sym(expr: &Expr) -> Option<&Symbol> {
    match &*expr.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value),
        _ => None,
    }
}

fn is_literal(expr: &Expr) -> bool {
    matches!(&*expr.node, ExprNode::Lit { .. })
}

fn read(call: &Expr) -> Option<Declaration> {
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &*call.node else { return None };
    if method.as_str() != "attribute" {
        return None;
    }
    let name = sym(args.first()?)?.clone();
    let mut rest = &args[1..];
    let mut default = None;
    if let Some(last) = rest.last() {
        if let ExprNode::Hash { entries, kwargs: true } = &*last.node {
            // `default:` is the one option read; anything else is not ours.
            let [(key, value)] = entries.as_slice() else { return None };
            if sym(key)?.as_str() != "default" || !is_literal(value) {
                return None;
            }
            default = Some(value.clone());
            rest = &rest[..rest.len() - 1];
        }
    }
    let type_name = match rest {
        [] => None,
        [t] => Some(sym(t)?.clone()),
        _ => return None,
    };
    Some(Declaration { name, type_name, default })
}

/// Is `call`, in the body of the library class `lc`, an `attribute`
/// declaration this pass lowers: the class descends from a tableless model?
pub(crate) fn claims(app: &App, lc: &LibraryClass, call: &Expr) -> bool {
    read(call).is_some() && tableless_ancestor(app, lc)
}

/// The PostgreSQL Ruby-family tree runs a subclass's `attribute` against
/// the ActiveModel::Attributes port its tableless base includes
/// (`active_model_model::keep_attributes_include`), so the declaration
/// is replayed as written (`model_to_library::macro_replay`) with the
/// port's registry, `attributes` and `attribute_names`, not lowered here.
fn lowered_here(app: &App, lineage: &Lineage, lc: &LibraryClass) -> bool {
    model_ancestry(lc, lineage)
        .is_some_and(|(model, _)| model.parent.is_none() && !crate::lower::active_model_model::runs_attributes_at_run_time(app, model))
}

fn tableless_ancestor(app: &App, lc: &LibraryClass) -> bool {
    lowered_here(app, &Lineage::new(app), lc)
}

pub fn apply_sti_subclass_attributes(app: &mut App) {
    let lineage = Lineage::new(app);
    let plans: Vec<usize> = app
        .library_classes
        .iter()
        .enumerate()
        .filter(|(_, lc)| lc.unknown_calls.iter().any(|c| read(c).is_some()))
        .filter(|(_, lc)| lowered_here(app, &lineage, lc))
        .map(|(i, _)| i)
        .collect();
    for index in plans {
        let lc = &mut app.library_classes[index];
        let declarations: Vec<Declaration> = lc.unknown_calls.iter().filter_map(read).collect();
        for d in declarations {
            define(lc, &d);
        }
    }
}

fn define(lc: &mut LibraryClass, d: &Declaration) {
    let s = Span::synthetic();
    let node = |n| Expr::new(s, n);
    let var = |n: &str| node(ExprNode::Var { id: crate::ident::VarId(0), name: Symbol::from(n) });
    let defines = |lc: &LibraryClass, n: &str| lc.methods.iter().any(|m| m.name.as_str() == n && m.receiver == MethodReceiver::Instance);
    // `ActiveModel::Type.lookup(:type).cast(value)`; no type is `Type::Value`, which casts nothing.
    let cast = |value: Expr| match &d.type_name {
        None => value,
        Some(t) => {
            let lookup = node(ExprNode::Send {
                recv: Some(node(ExprNode::Const { path: vec![Symbol::from("ActiveModel"), Symbol::from("Type")] })),
                method: Symbol::from("lookup"),
                args: vec![node(ExprNode::Lit { value: Literal::Sym { value: t.clone() } })],
                block: None,
                parenthesized: true,
            });
            node(ExprNode::Send { recv: Some(lookup), method: Symbol::from("cast"), args: vec![value], block: None, parenthesized: true })
        }
    };
    let name = d.name.as_str();
    if !defines(lc, name) {
        let current = node(ExprNode::Ivar { name: d.name.clone() });
        let body = match &d.default {
            None => current,
            Some(default) => {
                let set = node(ExprNode::Send {
                    recv: Some(node(ExprNode::SelfRef)),
                    method: Symbol::from("instance_variable_defined?"),
                    args: vec![node(ExprNode::Lit { value: Literal::Sym { value: Symbol::from(format!("@{name}")) } })],
                    block: None,
                    parenthesized: true,
                });
                node(ExprNode::If { cond: set, then_branch: current, else_branch: cast(default.clone()) })
            }
        };
        lc.methods.push(instance_method(&lc.name, name, Vec::new(), body));
    }
    let writer = format!("{name}=");
    if !defines(lc, &writer) {
        let body = node(ExprNode::Assign { target: crate::expr::LValue::Ivar { name: d.name.clone() }, value: cast(var("value")) });
        lc.methods.push(instance_method(&lc.name, &writer, vec![param("value", None)], body));
    }
}
