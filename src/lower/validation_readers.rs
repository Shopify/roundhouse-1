//! A validation reads its attribute through the reader, as ActiveModel's
//! `read_attribute_for_validation` is `send(attr)`. The model lowering reads
//! a column's slot (`@domain`), which is the same value until a module the
//! model includes redefines the reader: Core's `Shop::Extension` has `def
//! domain; super || permanent_domain; end`, so `Shop.create!(permanent_domain:
//! ...)` is valid in Rails, and the slot read rejected it ("Domain can't be
//! blank"). In the `validate` of a model that includes such a module, the
//! slot read of each name an included app module defines becomes the reader
//! call.

use crate::App;
use crate::dialect::{LibraryClass, MethodReceiver};
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};

pub(crate) fn apply(lcs: &mut [LibraryClass], app: &App) {
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        if lc.includes.is_empty() {
            continue;
        }
        let mut defined: crate::det::HashSet<Symbol> = crate::det::HashSet::default();
        let mut seen: Vec<ClassId> = Vec::new();
        let mut pending: Vec<ClassId> = lc.includes.clone();
        while let Some(name) = pending.pop() {
            if seen.contains(&name) {
                continue;
            }
            let wanted = name.0.as_str().trim_start_matches("::");
            for module in app.library_classes.iter().filter(|c| c.is_module && c.name.0.as_str().trim_start_matches("::") == wanted) {
                defined.extend(
                    module
                        .methods
                        .iter()
                        .filter(|m| m.receiver == MethodReceiver::Instance && model.attributes.fields.contains_key(&m.name))
                        .map(|m| m.name.clone()),
                );
                pending.extend(module.includes.iter().cloned());
            }
            seen.push(name);
        }
        if defined.is_empty() {
            continue;
        }
        let Some(validate) = lc.methods.iter_mut().find(|m| m.name.as_str() == "validate" && m.receiver == MethodReceiver::Instance)
        else {
            continue;
        };
        read_through_reader(&mut validate.body, &defined);
    }
}

fn read_through_reader(e: &mut Expr, readers: &crate::det::HashSet<Symbol>) {
    if let ExprNode::Ivar { name } = &*e.node {
        // Numericality reads the raw value (`prepare_value_for_validation`),
        // not a reader over it (Core's `money_bags` answers a MoneyBag).
        if readers.contains(name) && e.decisions & crate::expr::RAW_ATTRIBUTE_READ == 0 {
            let reader = Expr::new(
                e.span,
                ExprNode::Send {
                    recv: Some(Expr::new(e.span, ExprNode::SelfRef)),
                    method: name.clone(),
                    args: Vec::new(),
                    block: None,
                    parenthesized: false,
                },
            );
            let ty = e.ty.clone();
            *e = reader;
            e.ty = ty;
        }
        return;
    }
    e.node.for_each_child_mut(&mut |c| read_through_reader(c, readers));
}
