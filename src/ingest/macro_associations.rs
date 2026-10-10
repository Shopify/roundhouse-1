//! A class macro that runs `belongs_to` (or, on the PostgreSQL runtime,
//! `has_one`/`has_many`: Core's `readonly_has_one`/`readonly_has_many`,
//! whose scope goes through `build_readonly_scope`) with the arguments
//! it was given
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
    let postgresql = app.carries_postgresql_runtime();
    for (i, model) in app.models.iter().enumerate() {
        let side = crate::lower::model_to_library::macro_replay::ClassSide::of_model(model, app);
        let mut items = Vec::new();
        for (at, item) in model.body.iter().enumerate() {
            let ModelBodyItem::Unknown { expr, .. } = item else { continue };
            let ExprNode::Send { recv: None, method, block: None, .. } = &*expr.node else { continue };
            let Some(def) = side.macro_def(method.as_str()) else { continue };
            let Some(forward) = forwarded_to_association(&side, def, 0) else { continue };
            // A `has_one`/`has_many` reflection is answered from the
            // reflection classes the PostgreSQL runtime carries
            // (`emit::ruby::library::apply_association_reflections`).
            if forward.kind != "belongs_to" && !postgresql {
                continue;
            }
            if let Some(mut assoc) = association_of(app, expr, &model.name, forward.kind) {
                if !apply_merged(&mut assoc, &forward.merged) || !apply_scope_wrapper(&mut assoc, forward.scope_wrapper.as_ref()) {
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

/// The macro call parsed as the `belongs_to`/`has_one`/`has_many` call
/// it makes. (The sources are drained into `app.sources` by now.)
fn association_of(
    app: &crate::App,
    call: &Expr,
    owner: &crate::ident::ClassId,
    kind: &str,
) -> Option<crate::dialect::Association> {
    let source = crate::ide::source(app, call.span.file)?;
    let parsed = ruby_prism::parse(source.text.as_bytes());
    let root = parsed.node();
    let node = super::define_method_loops::node_at(&root, call.span.start as usize, call.span.end as usize, |n| {
        n.as_call_node().is_some()
    })?;
    super::model::parse_association(&node.as_call_node()?, owner, kind, &source.path)
}

/// What a class macro hands the association DSL it ends in.
struct Forward {
    /// `belongs_to`, `has_one` or `has_many`.
    kind: &'static str,
    /// The literal options merged on the way, innermost last.
    merged: Vec<(Symbol, Expr)>,
    /// The relation chain a scope-wrapping helper puts ahead of the
    /// call's scope (Core's `build_readonly_scope`: `readonly`).
    scope_wrapper: Option<Expr>,
}

/// The association DSL `def` ends in, with the literal options it
/// merges into what it hands it, when it passes its own `(name, *args |
/// scope = nil, **options)` on to `belongs_to`, `has_one` or `has_many`
/// on `self` (or a local holding it), or to another class macro that
/// does. The scope may go through a helper that wraps it in a relation
/// chain (`readonly_scope = build_readonly_scope(scope)`,
/// [`scope_wrapper`]), and an option may be a local array the macro
/// builds (`extend: extend_option`), which [`apply_merged`] reads.
fn forwarded_to_association(
    side: &crate::lower::model_to_library::macro_replay::ClassSide<'_>,
    def: &MethodDef,
    depth: usize,
) -> Option<Forward> {
    let [record, second, options] = def.params.as_slice() else { return None };
    if record.rest || record.keyword || record.default.is_some() {
        return None;
    }
    let optional_nil = matches!(second.default.as_ref().map(|d| &*d.node), Some(ExprNode::Lit { value: Literal::Nil }));
    if second.keyword || !(second.rest || optional_nil) {
        return None;
    }
    let mut selves: Vec<Symbol> = Vec::new();
    // Locals holding the scope wrapped by a helper, with the wrapper.
    let mut wrapped: Vec<(Symbol, Expr)> = Vec::new();
    let statements: Vec<&Expr> = match &*def.body.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![&def.body],
    };
    for stmt in statements {
        match &*stmt.node {
            ExprNode::Assign { target: LValue::Var { name, .. }, value } if is_self(value) => {
                selves.push(name.clone());
            }
            ExprNode::Assign { target: LValue::Var { name, .. }, value } if !second.rest => {
                if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*value.node {
                    if let ([arg], Some(helper)) = (args.as_slice(), side.macro_def(method.as_str())) {
                        if is_var(arg, &second.name) {
                            if let Some(wrapper) = scope_wrapper(helper) {
                                wrapped.push((name.clone(), wrapper));
                            }
                        }
                    }
                }
            }
            ExprNode::Send { recv, method, args, block, .. } => {
                let on_self = match recv {
                    None => true,
                    Some(r) => is_self(r) || matches!(&*r.node, ExprNode::Var { name, .. } if selves.contains(name)),
                };
                let [a_record, a_second, a_options] = args.as_slice() else { continue };
                let mut scope_wrapper = None;
                let passes_second = if second.rest {
                    matches!(&*a_second.node, ExprNode::Splat { value } if is_var(value, &second.name))
                } else if is_var(a_second, &second.name) {
                    true
                } else if let Some((_, w)) = wrapped.iter().find(|(n, _)| is_var(a_second, n)) {
                    scope_wrapper = Some(w.clone());
                    true
                } else {
                    false
                };
                // A block passed on is the macro's own `&extension`
                // (the association-extension block the call gives).
                let passes_block = match block {
                    None => true,
                    Some(b) => def.block_param.as_ref().is_some_and(|p| is_var(b, &p.name)),
                };
                if !on_self || !is_var(a_record, &record.name) || !passes_second || !passes_block {
                    continue;
                }
                let Some(mut merged) = passed_options(a_options, &options.name) else { continue };
                if let kind @ ("belongs_to" | "has_one" | "has_many") = method.as_str() {
                    let kind = match kind {
                        "belongs_to" => "belongs_to",
                        "has_one" => "has_one",
                        _ => "has_many",
                    };
                    return Some(Forward { kind, merged, scope_wrapper });
                }
                if depth < 4 && scope_wrapper.is_none() {
                    let inner = side.macro_def(method.as_str())?;
                    let deeper = forwarded_to_association(side, inner, depth + 1)?;
                    merged.extend(deeper.merged);
                    return Some(Forward { kind: deeper.kind, merged, scope_wrapper: deeper.scope_wrapper });
                }
            }
            _ => {}
        }
    }
    None
}

/// The relation chain a scope helper `def helper(scope)` puts ahead of
/// the scope when every lambda it can answer runs the chain and then
/// the scope against it (`-> { readonly.instance_exec(&scope) }`,
/// `->(*args) { readonly.instance_exec(*args, &scope) }`) or, for no
/// scope, is the chain alone (`-> { readonly }`): Core's
/// `build_readonly_scope`. The chain is receiverless calls with literal
/// arguments; anything else is not read.
fn scope_wrapper(helper: &MethodDef) -> Option<Expr> {
    let [scope] = helper.params.as_slice() else { return None };
    if scope.rest || scope.keyword {
        return None;
    }
    let mut lambdas = Vec::new();
    answers(&helper.body, &mut lambdas)?;
    let mut chain: Option<Expr> = None;
    for body in lambdas {
        let body = uncast(body);
        let this = match &*body.node {
            ExprNode::Send { recv: Some(r), method, block: Some(b), .. } if method.as_str() == "instance_exec" && is_var(b, &scope.name) => {
                r.clone()
            }
            _ => body.clone(),
        };
        if !literal_chain(&this) {
            return None;
        }
        match &chain {
            None => chain = Some(this),
            Some(c) if same_chain(c, &this) => {}
            Some(_) => return None,
        }
    }
    chain
}

/// The lambda bodies `expr` answers through `if`/`elsif`/`else` and
/// statement sequences; `None` when it can answer anything else.
fn answers<'e>(expr: &'e Expr, out: &mut Vec<&'e Expr>) -> Option<()> {
    match &*expr.node {
        ExprNode::Lambda { body, .. } => {
            out.push(body);
            Some(())
        }
        ExprNode::If { then_branch, else_branch, .. } => {
            answers(then_branch, out)?;
            answers(else_branch, out)
        }
        ExprNode::Seq { exprs } => answers(exprs.last()?, out),
        _ => None,
    }
}

/// The expression under inline casts, and after a leading `#: self as
/// T` statement (Core's lambdas open with one).
fn uncast(expr: &Expr) -> &Expr {
    match &*expr.node {
        ExprNode::Cast { value, .. } => uncast(value),
        ExprNode::Seq { exprs } => match exprs.split_last() {
            Some((last, rest)) if rest.iter().all(is_self) => uncast(last),
            _ => expr,
        },
        _ => expr,
    }
}

/// `readonly`, `readonly.where(x: 1)`: receiverless-rooted calls with
/// literal arguments and no blocks.
fn literal_chain(expr: &Expr) -> bool {
    match &*expr.node {
        ExprNode::Send { recv, args, block: None, .. } => {
            args.iter().all(|a| matches!(&*a.node, ExprNode::Lit { .. })) && recv.as_ref().is_none_or(literal_chain)
        }
        _ => false,
    }
}

fn same_chain(a: &Expr, b: &Expr) -> bool {
    match (&*a.node, &*b.node) {
        (
            ExprNode::Send { recv: ra, method: ma, args: aa, .. },
            ExprNode::Send { recv: rb, method: mb, args: ab, .. },
        ) => {
            ma == mb
                && aa.len() == ab.len()
                && aa.iter().zip(ab).all(|(x, y)| match (&*x.node, &*y.node) {
                    (ExprNode::Lit { value: vx }, ExprNode::Lit { value: vy }) => vx == vy,
                    _ => false,
                })
                && match (ra, rb) {
                    (None, None) => true,
                    (Some(x), Some(y)) => same_chain(x, y),
                    _ => false,
                }
        }
        _ => false,
    }
}

/// Put the wrapper's chain ahead of the association's scope (the scope
/// then runs against it, as `instance_exec` on the wrapped relation
/// does): `-> { order("id DESC") }` wrapped in `readonly` reads
/// `readonly.order("id DESC")`, and no scope reads `readonly`. False
/// for a scope this does not graft onto (the call stays refused).
fn apply_scope_wrapper(assoc: &mut crate::dialect::Association, wrapper: Option<&Expr>) -> bool {
    use crate::dialect::Association;
    let Some(wrapper) = wrapper else { return true };
    let scope = match assoc {
        Association::HasMany { scope, .. } | Association::HasOne { scope, .. } => scope,
        _ => return false,
    };
    match scope.take() {
        None => {
            *scope = Some(wrapper.clone());
            true
        }
        Some(mut body) => {
            let grafted = graft(&mut body, wrapper);
            *scope = Some(body);
            grafted
        }
    }
}

/// Root the receiverless call at the bottom of `chain` on `base`.
fn graft(chain: &mut Expr, base: &Expr) -> bool {
    match &mut *chain.node {
        ExprNode::Send { recv: Some(r), .. } => graft(r, base),
        ExprNode::Send { recv: recv @ None, .. } => {
            *recv = Some(base.clone());
            true
        }
        _ => false,
    }
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
                    // `extend: extend_option`, a local the macro builds;
                    // `apply_merged` decides.
                    (ExprNode::Lit { value: Literal::Sym { value: key } }, ExprNode::Var { .. }) if key.as_str() == "extend" => {
                        Some((key.clone(), v.clone()))
                    }
                    _ => None,
                })
                .collect()
        }
        _ => None,
    }
}

/// Apply the merged options to the association parsed from the call;
/// false for one this does not read (the call stays refused).
///
/// A has_many's merged `extend:` modules (Core's
/// `ReadOnlyAssociationExtension`, whose mutators raise) are not applied
/// to the compiled collection, whose reader answers an Array; the
/// reflection's `options[:extend]` carries them when the macro's
/// `has_many` runs, and `macro_associations::push_reflection_methods`'s
/// class-side `has_many` names the gap.
fn apply_merged(assoc: &mut crate::dialect::Association, merged: &[(Symbol, Expr)]) -> bool {
    use crate::dialect::Association;
    for (key, value) in merged {
        if key.as_str() == "extend" && matches!(assoc, Association::HasMany { .. }) {
            continue;
        }
        let ExprNode::Lit { value: Literal::Bool { value } } = &*value.node else { return false };
        match (&mut *assoc, key.as_str()) {
            (Association::BelongsTo { autosave, .. }, "autosave") => *autosave = Some(*value),
            (Association::HasMany { autosave, .. } | Association::HasOne { autosave, .. }, "autosave") => *autosave = *value,
            (Association::BelongsTo { optional, declared_optional, .. }, "optional") => {
                *optional = *value;
                *declared_optional = Some(*value);
            }
            (Association::BelongsTo { optional, declared_optional, .. }, "required") => {
                *optional = !*value;
                *declared_optional = Some(!*value);
            }
            _ => return false,
        }
    }
    true
}
