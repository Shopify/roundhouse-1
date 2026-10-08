//! A model subclass's own `belongs_to` / `has_many` / `has_one`,
//! `scope` and callback declarations, defined on the subclass as Rails
//! defines them.
//! A concrete model's subclass (STI or not) is a model to Rails and a
//! library class to ingest, whose body kept the declarations as unknown
//! calls; ingest re-reads them as the model body walk does
//! (`Model::subclass_declarations`). Each subclass gets a view of its
//! model — the model's table and columns, the subclass's name and only
//! its own declarations — that the model synthesizers run on: the
//! association readers and writers, the `dependent: :destroy` cascade,
//! the required `belongs_to` checks (after the inherited `validate`),
//! the lifecycle hooks (after the inherited ones, `super` first) and
//! the transaction-callback chains (an ancestor's entries first, as
//! Rails copies the chain; [`lineage`]) in `emit::ruby::library`, ahead
//! of the scope lowering; the scope class methods, their relation entry
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
    let mut lc_parent: HashMap<&ClassId, &Option<ClassId>> = HashMap::default();
    for lc in &app.library_classes {
        lc_parent.entry(&lc.name).or_insert(&lc.parent);
    }
    for model in &app.models {
        for (sub, items) in &model.subclass_declarations {
            if !sti.contains_key(sub) && !plain.contains_key(sub) {
                continue;
            }
            let mut view = model.clone();
            view.name = sub.clone();
            view.parent = lc_parent.get(sub).and_then(|p| (*p).clone());
            view.body = items.clone();
            view.subclass_declarations = Vec::new();
            out.insert(sub.clone(), view);
        }
    }
    out
}

/// The classes above `view` the transaction-callback chain copies from,
/// nearest first: the subclass views between it and its model, then the
/// model and the model's own model ancestors. Empty when `view` declares
/// no transaction callback.
pub(crate) fn lineage(app: &App, views: &HashMap<ClassId, Model>, view: &Model) -> Vec<Model> {
    let transactional =
        view.body.iter().any(|i| matches!(i, ModelBodyItem::Callback { callback, .. } if callback.hook.is_transaction()));
    let mut out: Vec<Model> = Vec::new();
    if !transactional {
        return out;
    }
    let mut cursor = view.parent.clone();
    for _ in 0..16 {
        let Some(name) = cursor else { break };
        let next = match views.get(&name) {
            Some(v) => v.clone(),
            None => match app.models.iter().find(|m| m.name == name) {
                Some(m) => m.clone(),
                None => break,
            },
        };
        if out.iter().any(|m| m.name == next.name) {
            break;
        }
        cursor = next.parent.clone();
        out.push(next);
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
/// Associations and callbacks by span; scopes by name.
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
    items.iter().any(|i| match i {
        ModelBodyItem::Association { span, .. } | ModelBodyItem::Callback { span, .. } => *span == call.span,
        // `after_initialize` (`model_to_library::symbol_after_initialize`
        // or its block form), folded by the view's callback pass.
        ModelBodyItem::Unknown { expr, .. } => expr.span == call.span,
        _ => false,
    })
}

/// The spans of every subclass callback declaration lowered here, which
/// the bare-symbol fold (`lower::sti_subclass_callbacks`) leaves alone.
pub(crate) fn callback_spans(app: &App) -> crate::det::HashSet<crate::span::Span> {
    app.models
        .iter()
        .flat_map(|m| m.subclass_declarations.iter())
        .flat_map(|(_, items)| items.iter())
        .filter_map(|i| match i {
            ModelBodyItem::Callback { span, .. } => Some(*span),
            _ => None,
        })
        .collect()
}
