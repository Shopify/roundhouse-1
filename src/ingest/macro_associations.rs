//! A class macro that runs `belongs_to` with the arguments it was given
//! (Core's `AcceleratedBelongsTo#accelerated_belongs_to(record, *args,
//! **kwargs)`, 470 `:shop` calls), directly or through another such
//! macro and with literal options merged on the way
//! (`readonly_belongs_to(name, scope = nil, **options)` passes
//! `**options.merge(autosave: false)`), declares the association its call
//! names: the model gains it as an Association item parsed from the macro
//! call as a `belongs_to` call, sharing the call's span. The call itself
//! stays in the body and replays at class load
//! (`model_to_library::macro_replay`), where the compiled class answers
//! its `belongs_to` and `reflect_on_association`
//! (`model_to_library::macro_associations`).

use crate::dialect::{MethodDef, ModelBodyItem};
use crate::expr::{Expr, ExprNode, LValue, Literal};
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
            let Some(merged) = forwarded_to_belongs_to(&side, def, 0) else { continue };
            if let Some(mut assoc) = association_of(app, expr, &model.name) {
                if !apply_merged(&mut assoc, &merged) {
                    continue;
                }
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

/// The literal options `def` merges into what it hands `belongs_to`,
/// innermost last, when it passes its own `(name, *args | scope = nil,
/// **options)` on to `belongs_to` on `self` (or a local holding it), or to
/// another class macro that does.
fn forwarded_to_belongs_to(
    side: &crate::lower::model_to_library::macro_replay::ClassSide<'_>,
    def: &MethodDef,
    depth: usize,
) -> Option<Vec<(Symbol, Expr)>> {
    let [record, second, options] = def.params.as_slice() else { return None };
    if record.rest || record.keyword || record.default.is_some() {
        return None;
    }
    let optional_nil = matches!(second.default.as_ref().map(|d| &*d.node), Some(ExprNode::Lit { value: Literal::Nil }));
    if second.keyword || !(second.rest || optional_nil) {
        return None;
    }
    let mut selves: Vec<Symbol> = Vec::new();
    let statements: Vec<&Expr> = match &*def.body.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![&def.body],
    };
    for stmt in statements {
        match &*stmt.node {
            ExprNode::Assign { target: LValue::Var { name, .. }, value } if is_self(value) => {
                selves.push(name.clone());
            }
            ExprNode::Send { recv, method, args, block: None, .. } => {
                let on_self = match recv {
                    None => true,
                    Some(r) => is_self(r) || matches!(&*r.node, ExprNode::Var { name, .. } if selves.contains(name)),
                };
                let [a_record, a_second, a_options] = args.as_slice() else { continue };
                let passes_second = if second.rest {
                    matches!(&*a_second.node, ExprNode::Splat { value } if is_var(value, &second.name))
                } else {
                    is_var(a_second, &second.name)
                };
                if !on_self || !is_var(a_record, &record.name) || !passes_second {
                    continue;
                }
                let Some(mut merged) = passed_options(a_options, &options.name) else { continue };
                if method.as_str() == "belongs_to" {
                    return Some(merged);
                }
                if depth < 4 {
                    let inner = side.macro_def(method.as_str())?;
                    let deeper = forwarded_to_belongs_to(side, inner, depth + 1)?;
                    merged.extend(deeper);
                    return Some(merged);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_self(expr: &Expr) -> bool {
    match &*expr.node {
        ExprNode::SelfRef => true,
        ExprNode::Cast { value, .. } => is_self(value),
        _ => false,
    }
}

fn is_var(expr: &Expr, name: &Symbol) -> bool {
    matches!(&*expr.node, ExprNode::Var { name: n, .. } if n == name)
}

/// `**name` or `**name.merge(key: literal, ...)`: the merged pairs.
fn passed_options(arg: &Expr, name: &Symbol) -> Option<Vec<(Symbol, Expr)>> {
    match &*arg.node {
        ExprNode::Var { name: n, .. } if n == name => Some(Vec::new()),
        ExprNode::KeywordSplat { value } => passed_options(value, name),
        ExprNode::Hash { entries, .. } => match entries.as_slice() {
            [(k, _)] => passed_options(k, name),
            _ => None,
        },
        ExprNode::Send { recv: Some(r), method, args, block: None, .. } if method.as_str() == "merge" && is_var(r, name) => {
            let [arg] = args.as_slice() else { return None };
            let ExprNode::Hash { entries, .. } = &*arg.node else { return None };
            entries
                .iter()
                .map(|(k, v)| match (&*k.node, &*v.node) {
                    (ExprNode::Lit { value: Literal::Sym { value: key } }, ExprNode::Lit { .. }) => Some((key.clone(), v.clone())),
                    _ => None,
                })
                .collect()
        }
        _ => None,
    }
}

/// Apply the merged options to the association parsed from the call;
/// false for one this does not read (the call stays refused).
fn apply_merged(assoc: &mut crate::dialect::Association, merged: &[(Symbol, Expr)]) -> bool {
    let crate::dialect::Association::BelongsTo { autosave, optional, declared_optional, .. } = assoc else { return false };
    for (key, value) in merged {
        let ExprNode::Lit { value: Literal::Bool { value } } = &*value.node else { return false };
        match key.as_str() {
            "autosave" => *autosave = Some(*value),
            "optional" => {
                *optional = *value;
                *declared_optional = Some(*value);
            }
            "required" => {
                *optional = !*value;
                *declared_optional = Some(!*value);
            }
            _ => return false,
        }
    }
    true
}
