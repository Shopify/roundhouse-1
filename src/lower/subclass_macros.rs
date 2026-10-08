//! A model subclass's own `belongs_to` / `has_many` / `has_one` and
//! `scope` declarations, defined on the subclass as Rails defines them.
//! A concrete model's subclass (STI or not) is a model to Rails and a
//! library class to ingest, whose body kept the declarations as unknown
//! calls; ingest re-reads them as the model body walk does
//! (`Model::subclass_declarations`). Each subclass gets a view of its
//! model — the model's table and columns, the subclass's name and only
//! its own declarations — that the model synthesizers run on: the
//! association readers and writers (`emit::ruby::library`, ahead of the
//! scope lowering), the scope class methods, their relation entry
//! points and the relation delegates (`apply_scope_lowering`,
//! `scope_chain::build_scope_registry`). An STI subclass with scopes of
//! its own roots its relations on itself (`lower::sti_scope`), so a
//! scope chained mid-relation dispatches to the subclass; its bare scope
//! call carries the type condition in the scope's default relation.
//! Ruby family; models without such subclasses are untouched.

use crate::App;
use crate::det::HashMap;
use crate::dialect::{Model, ModelBodyItem};
use crate::expr::Expr;
use crate::ident::ClassId;

/// Subclass -> its view model, for each subclass of an app model (STI,
/// `sti_scope::sti_bases`, or plain, `sti_scope::plain_model_subclasses`)
/// with declarations of its own.
pub(crate) fn views(app: &App) -> HashMap<ClassId, Model> {
    let mut out = HashMap::default();
    if app.models.iter().all(|m| m.subclass_declarations.is_empty()) {
        return out;
    }
    let sti = super::sti_scope::sti_bases(app);
    let plain = super::sti_scope::plain_model_subclasses(app);
    for model in &app.models {
        for (sub, items) in &model.subclass_declarations {
            if !sti.contains_key(sub) && !plain.contains_key(sub) {
                continue;
            }
            let mut view = model.clone();
            view.name = sub.clone();
            view.body = items.clone();
            view.subclass_declarations = Vec::new();
            out.insert(sub.clone(), view);
        }
    }
    out
}

/// Does `sub` declare a scope of its own?
pub(crate) fn declares_scopes(app: &App, sub: &ClassId) -> bool {
    app.models.iter().any(|m| {
        m.subclass_declarations
            .iter()
            .any(|(s, items)| s == sub && items.iter().any(|i| matches!(i, ModelBodyItem::Scope { .. })))
    })
}

/// Is `call` (a subclass body call) one of its lowered declarations?
/// Associations by span; scopes by name.
pub(crate) fn lowers(app: &App, sub: &ClassId, call: &Expr) -> bool {
    use crate::expr::{ExprNode, Literal};
    let Some(items) = app
        .models
        .iter()
        .flat_map(|m| m.subclass_declarations.iter())
        .find(|(s, _)| s == sub)
        .map(|(_, items)| items)
    else {
        return false;
    };
    let ExprNode::Send { recv: None, method, args, .. } = &*call.node else { return false };
    if method.as_str() == "scope" {
        let Some(ExprNode::Lit { value: Literal::Sym { value } }) = args.first().map(|a| &*a.node) else { return false };
        return items.iter().any(|i| matches!(i, ModelBodyItem::Scope { scope, .. } if &scope.name == value));
    }
    items.iter().any(|i| matches!(i, ModelBodyItem::Association { span, .. } if *span == call.span))
}
