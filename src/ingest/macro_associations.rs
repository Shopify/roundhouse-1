//! A class macro that runs `belongs_to` with the arguments it was given
//! (Core's `AcceleratedBelongsTo#accelerated_belongs_to(record, *args,
//! **kwargs)`, 470 `:shop` calls) declares the association its call
//! names: the model gains it as an Association item parsed from the macro
//! call as a `belongs_to` call, sharing the call's span. The call itself
//! stays in the body and replays at class load
//! (`model_to_library::macro_replay`), where the compiled class answers
//! its `belongs_to` and `reflect_on_association`
//! (`model_to_library::macro_associations`).

use crate::dialect::{MethodDef, ModelBodyItem};
use crate::expr::{Expr, ExprNode, LValue};
use crate::ident::Symbol;

/// Add the associations the models' class macro calls declare; answers
/// whether there were any.
pub(super) fn declare(app: &mut crate::App) -> bool {
    let models: crate::det::HashSet<String> = app.models.iter().map(|m| m.name.0.as_str().to_string()).collect();
    let mut adds: Vec<(usize, Vec<(usize, ModelBodyItem)>)> = Vec::new();
    for (i, model) in app.models.iter().enumerate() {
        let side = crate::lower::model_to_library::macro_replay::ClassSide::of_model(model, app);
        let mut items = Vec::new();
        for (at, item) in model.body.iter().enumerate() {
            let ModelBodyItem::Unknown { expr, .. } = item else { continue };
            let ExprNode::Send { recv: None, method, block: None, .. } = &*expr.node else { continue };
            let Some(def) = side.macro_def(method.as_str()) else { continue };
            if !forwards_to_belongs_to(def) {
                continue;
            }
            if let Some(mut assoc) = association_of(app, expr, &model.name) {
                if let crate::dialect::Association::BelongsTo { target, .. } = &mut assoc {
                    super::app::qualify_target(&model.name, target, &models);
                }
                items.push((at, ModelBodyItem::Association { assoc, leading_comments: Vec::new(), leading_blank_line: false, span: expr.span }));
            }
        }
        if !items.is_empty() {
            adds.push((i, items));
        }
    }
    let any = !adds.is_empty();
    for (i, items) in adds {
        let body = &mut app.models[i].body;
        for (at, item) in items.into_iter().rev() {
            body.insert(at, item);
        }
    }
    any
}

/// The macro call parsed as the `belongs_to` call it makes.
/// (The sources are drained into `app.sources` by now.)
fn association_of(app: &crate::App, call: &Expr, owner: &crate::ident::ClassId) -> Option<crate::dialect::Association> {
    let source = crate::ide::source(app, call.span.file)?;
    let parsed = ruby_prism::parse(source.text.as_bytes());
    let root = parsed.node();
    let node = super::define_method_loops::node_at(&root, call.span.start as usize, call.span.end as usize, |n| {
        n.as_call_node().is_some()
    })?;
    super::model::parse_association(&node.as_call_node()?, owner, "belongs_to", &source.path)
}

/// Does `def` (`def m(record, *args, **kwargs)`) pass its parameters to
/// `belongs_to` as it received them, on `self` or a local holding it?
pub(crate) fn forwards_to_belongs_to(def: &MethodDef) -> bool {
    let [record, rest, options] = def.params.as_slice() else { return false };
    if record.rest || record.keyword || record.default.is_some() || !rest.rest {
        return false;
    }
    let names = [&record.name, &rest.name, &options.name];
    let mut selves: Vec<Symbol> = Vec::new();
    let statements: Vec<&Expr> = match &*def.body.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![&def.body],
    };
    statements.into_iter().any(|stmt| match &*stmt.node {
        ExprNode::Assign { target: LValue::Var { name, .. }, value } => {
            if is_self(value) {
                selves.push(name.clone());
            }
            false
        }
        ExprNode::Send { recv, method, args, block: None, .. } if method.as_str() == "belongs_to" => {
            let on_self = match recv {
                None => true,
                Some(r) => is_self(r) || matches!(&*r.node, ExprNode::Var { name, .. } if selves.contains(name)),
            };
            on_self
                && args.len() == names.len()
                && args.iter().zip(names).all(|(arg, name)| mentions_only(arg, name))
        }
        _ => false,
    })
}

fn is_self(expr: &Expr) -> bool {
    match &*expr.node {
        ExprNode::SelfRef => true,
        ExprNode::Cast { value, .. } => is_self(value),
        _ => false,
    }
}

/// `arg` is `name`, `*name` or `**name`.
fn mentions_only(arg: &Expr, name: &Symbol) -> bool {
    match &*arg.node {
        ExprNode::Var { name: n, .. } => n == name,
        ExprNode::Splat { value } | ExprNode::KeywordSplat { value } => mentions_only(value, name),
        ExprNode::Hash { entries, .. } => {
            matches!(entries.as_slice(), [(k, _)] if mentions_only(k, name))
        }
        _ => false,
    }
}
