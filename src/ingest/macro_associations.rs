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
//!
//! A macro that declares an association of a fixed name with options
//! picked from its keywords (Core's `belongs_to_shop_through :order,
//! foreign_key: :shop_id`, which runs `accelerated_belongs_to(:shop,
//! **kwargs.extract!(:primary_key, :foreign_key, :inverse_of))`)
//! declares that association with the options the call passes on
//! ([`fixed_association`]).

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
            let Some(forward) = forwarded_to_association(&side, def, 0) else {
                if let Some(fixed) = fixed_association(&side, def, 0) {
                    if fixed.kind != "belongs_to" && !postgresql {
                        continue;
                    }
                    if let Some(mut assoc) = fixed_association_of(app, expr, &model.name, &fixed) {
                        if let crate::dialect::Association::BelongsTo { target, .. } = &mut assoc {
                            super::app::qualify_target(&model.name, target, &models);
                        }
                        items.push((at, ModelBodyItem::Association { assoc, leading_comments: Vec::new(), leading_blank_line: false, span: expr.span }));
                    }
                }
                continue;
            };
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
    for model in &mut app.models {
        drop_redeclared(&mut model.body);
    }
    super::model::forget_merged_calls();
    any
}

/// A second declaration of an association name replaces the first, as
/// Rails' `add_reflection` replaces the reflection of that name (Core's
/// Shipping::Label runs `accelerated_belongs_to :shop` and then
/// `belongs_to_shop_through :order`, which runs it again): the earlier
/// Association items of a redeclared name are dropped, so the compiled
/// readers and the `association`/`_preload_dispatch` case arms are
/// declared once (a duplicate `when` warns, and Core's strict warnings
/// make that fatal at load).
fn drop_redeclared(body: &mut Vec<ModelBodyItem>) {
    let mut seen: crate::det::HashSet<Symbol> = crate::det::HashSet::default();
    let mut keep = vec![true; body.len()];
    for (at, item) in body.iter().enumerate().rev() {
        if let ModelBodyItem::Association { assoc, .. } = item {
            keep[at] = seen.insert(assoc.name().clone());
        }
    }
    let mut keep = keep.into_iter();
    body.retain(|_| keep.next().unwrap_or(true));
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
    // A call a `with_options` block carries is read from the copy of the
    // file with the block's options merged in.
    let merged = super::model::merged_call_source(call.span.file, call.span.start);
    let text: &[u8] = merged.as_deref().unwrap_or(source.text.as_bytes());
    let parsed = ruby_prism::parse(text);
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
    let [record, second, middle @ .., options] = def.params.as_slice() else { return None };
    if record.rest || record.keyword || record.default.is_some() {
        return None;
    }
    // Keyword parameters between the scope and `**options` (Core's
    // `belongs_to_with_query_constraints(name, scope = nil,
    // query_constraints:, foreign_key:, primary_key:, **options)`) take
    // their values from the call, which the association is parsed from.
    if !middle.iter().all(|p| (p.keyword || p.from_keyword) && !p.rest) {
        return None;
    }
    let mut bound: Vec<Symbol> = middle.iter().map(|p| p.name.clone()).collect();
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
            // `query_constraints = options.delete(:query_constraints)`
            // (Core's `has_many_with_query_constraints`): the call's own
            // keyword, which the association is parsed from, as with a
            // keyword parameter.
            ExprNode::Assign { target: LValue::Var { name, .. }, value } if takes_call_option(value, &options.name, &bound) => {
                bound.push(name.clone());
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
                let Some(mut merged) = passed_options_with(a_options, &options.name, &bound) else { continue };
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

/// `options.delete(:key)`, possibly through `Array(...)` or an inline
/// cast or a local already bound this way: the call's own keyword.
fn takes_call_option(expr: &Expr, options: &Symbol, bound: &[Symbol]) -> bool {
    match &*expr.node {
        ExprNode::Send { recv: Some(r), method, args, block: None, .. } if method.as_str() == "delete" => {
            is_var(r, options) && matches!(args.as_slice(), [key] if matches!(&*key.node, ExprNode::Lit { value: Literal::Sym { .. } }))
        }
        ExprNode::Send { recv: None, method, args, block: None, .. } if method.as_str() == "Array" => {
            matches!(args.as_slice(), [inner] if takes_call_option(inner, options, bound))
        }
        ExprNode::Var { name, .. } => bound.contains(name),
        ExprNode::Cast { value, .. } => takes_call_option(value, options, bound),
        _ => false,
    }
}

/// What a class macro does to its keyword options on the way to the
/// association DSL, in order.
enum Step {
    /// The macro's own keyword parameters take these out of `**kwargs`.
    Drop(Vec<Symbol>),
    /// `**kwargs.merge(key: literal)`.
    Merge(Vec<(Symbol, Expr)>),
    /// `**kwargs.extract!(:key, ...)`: only these pass on.
    Extract(Vec<Symbol>),
}

/// A class macro that declares an association of a fixed name, whatever
/// the call names, with options computed from the call's keywords.
struct Fixed {
    kind: &'static str,
    name: Symbol,
    steps: Vec<Step>,
}

/// The association `def(*args, **kwargs)` declares when it calls
/// `belongs_to`/`has_one`/`has_many` on `self` with a literal name and
/// its keywords (`**kwargs`, `**kwargs.merge(...)` or
/// `**kwargs.extract!(:a, :b)`), directly or through a macro that passes
/// its own arguments on ([`forwarded_to_association`]): Core's
/// `belongs_to_shop_through(*args, **kwargs)` runs
/// `accelerated_belongs_to(:shop, **kwargs.extract!(:primary_key,
/// :foreign_key, :inverse_of))`. Also through a macro that passes its
/// positionals and keywords on to such a one
/// (`readonly_belongs_to_shop_through(name, *args, clone: false,
/// **kwargs)`, which merges `autosave: false`). The call's positionals
/// are the macro's own (`define_denormalized_foreign_key` reads them),
/// not the association's.
fn fixed_association(
    side: &crate::lower::model_to_library::macro_replay::ClassSide<'_>,
    def: &MethodDef,
    depth: usize,
) -> Option<Fixed> {
    let (kwrest, params) = def.params.split_last()?;
    if !((kwrest.keyword && kwrest.rest) || kwrest.from_kwrest) || kwrest.name.as_str().is_empty() {
        return None;
    }
    let is_keyword = |p: &crate::dialect::Param| (p.keyword || p.from_keyword) && !p.rest;
    let bound: Vec<Symbol> = params.iter().filter(|p| is_keyword(p)).map(|p| p.name.clone()).collect();
    let positionals: Vec<&crate::dialect::Param> = params.iter().filter(|p| !is_keyword(p)).collect();
    let statements: Vec<&Expr> = match &*def.body.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![&def.body],
    };
    for stmt in statements {
        let ExprNode::Send { recv, method, args, block: None, .. } = &*stmt.node else { continue };
        if !recv.as_ref().is_none_or(is_self) {
            continue;
        }
        let (options, lead) = match args.split_last() {
            Some((last, lead)) if matches!(&*last.node, ExprNode::KeywordSplat { .. } | ExprNode::Hash { .. }) => (Some(last), lead),
            _ => (None, args.as_slice()),
        };
        let mut steps = vec![Step::Drop(bound.clone())];
        match options {
            None => steps.push(Step::Extract(Vec::new())),
            Some(o) => {
                if let Some(keys) = extracted_keys(o, &kwrest.name) {
                    steps.push(Step::Extract(keys));
                } else if let Some(merged) = passed_options(o, &kwrest.name) {
                    steps.push(Step::Merge(merged));
                } else {
                    continue;
                }
            }
        }
        if let [name] = lead {
            if let ExprNode::Lit { value: Literal::Sym { value: name } } = &*name.node {
                if let kind @ ("belongs_to" | "has_one" | "has_many") = method.as_str() {
                    let kind = match kind {
                        "belongs_to" => "belongs_to",
                        "has_one" => "has_one",
                        _ => "has_many",
                    };
                    return Some(Fixed { kind, name: name.clone(), steps });
                }
                if depth >= 4 {
                    return None;
                }
                let forward = forwarded_to_association(side, side.macro_def(method.as_str())?, depth + 1)?;
                if forward.scope_wrapper.is_some() {
                    return None;
                }
                steps.push(Step::Merge(forward.merged));
                return Some(Fixed { kind: forward.kind, name: name.clone(), steps });
            }
        }
        // `inner(name, *args, **kwargs...)`: the macro's positionals as
        // they came.
        let passes_positionals = lead.iter().all(|a| {
            let a = match &*a.node {
                ExprNode::Splat { value } => value,
                _ => a,
            };
            positionals.iter().any(|p| is_var(a, &p.name))
        });
        if !passes_positionals || depth >= 4 {
            continue;
        }
        let Some(inner) = side.macro_def(method.as_str()) else { continue };
        let deeper = fixed_association(side, inner, depth + 1)?;
        steps.extend(deeper.steps);
        return Some(Fixed { steps, ..deeper });
    }
    None
}

/// `**name.extract!(:a, :b)`: the keys.
fn extracted_keys(arg: &Expr, name: &Symbol) -> Option<Vec<Symbol>> {
    match &*arg.node {
        ExprNode::KeywordSplat { value } => extracted_keys(value, name),
        ExprNode::Hash { entries, .. } => match entries.as_slice() {
            [(k, _)] => extracted_keys(k, name),
            _ => None,
        },
        ExprNode::Send { recv: Some(r), method, args, block: None, .. } if method.as_str() == "extract!" && is_var(r, name) => args
            .iter()
            .map(|a| match &*a.node {
                ExprNode::Lit { value: Literal::Sym { value } } => Some(value.clone()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// The association a [`Fixed`] macro's call declares: the call's
/// keywords run through the macro's steps, parsed as `belongs_to
/// :name, ...` (the call's positionals are not the association's). A
/// keyword reaching the association with a value other than a
/// literal, or a call with a `**splat`, is not read (the call stays
/// refused).
fn fixed_association_of(
    app: &crate::App,
    call: &Expr,
    owner: &crate::ident::ClassId,
    fixed: &Fixed,
) -> Option<crate::dialect::Association> {
    enum Value {
        /// A literal in the call, as written.
        Written(String),
        Merged(Expr),
    }
    let source = crate::ide::source(app, call.span.file)?;
    let parsed = ruby_prism::parse(source.text.as_bytes());
    let root = parsed.node();
    let node = super::define_method_loops::node_at(&root, call.span.start as usize, call.span.end as usize, |n| {
        n.as_call_node().is_some()
    })?;
    let node = node.as_call_node()?;
    if node.block().is_some() {
        return None;
    }
    let mut options: Vec<(Symbol, Value)> = Vec::new();
    for arg in node.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default() {
        let Some(hash) = arg.as_keyword_hash_node() else {
            if arg.as_splat_node().is_some() || arg.as_block_argument_node().is_some() {
                return None;
            }
            continue;
        };
        for element in hash.elements().iter() {
            let pair = element.as_assoc_node()?;
            let key = super::util::symbol_value(&pair.key())?;
            let value = pair.value();
            let literal = value.as_symbol_node().is_some()
                || value.as_string_node().is_some()
                || value.as_true_node().is_some()
                || value.as_false_node().is_some()
                || value.as_nil_node().is_some()
                || value.as_integer_node().is_some();
            let written = if literal {
                let loc = value.location();
                String::from_utf8_lossy(&source.text.as_bytes()[loc.start_offset()..loc.end_offset()]).into_owned()
            } else {
                String::new()
            };
            options.push((Symbol::from(key.as_str()), Value::Written(written)));
        }
    }
    for step in &fixed.steps {
        match step {
            Step::Drop(keys) => options.retain(|(k, _)| !keys.contains(k)),
            Step::Merge(pairs) => {
                for (key, value) in pairs {
                    options.retain(|(k, _)| k != key);
                    options.push((key.clone(), Value::Merged(value.clone())));
                }
            }
            Step::Extract(keys) => options.retain(|(k, _)| keys.contains(k)),
        }
    }
    let mut text = format!("{} :\"{}\"", fixed.kind, fixed.name.as_str());
    let mut merged = Vec::new();
    for (key, value) in options {
        match value {
            Value::Written(written) if written.is_empty() => return None,
            Value::Written(written) => text.push_str(&format!(", {}: {written}", key.as_str())),
            Value::Merged(value) => merged.push((key, value)),
        }
    }
    let parsed = ruby_prism::parse(text.as_bytes());
    let program = parsed.node();
    let statement = program.as_program_node()?.statements().body().iter().next()?;
    let mut assoc = super::model::parse_association(&statement.as_call_node()?, owner, fixed.kind, &source.path)?;
    apply_merged(&mut assoc, &merged).then_some(assoc)
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

/// [`passed_options`], or `**{ key: literal, own_kw: own_kw, ... }.merge(name)`
/// (what `belongs_to(name, scope, foreign_key: query_constraints,
/// optional: true, **options)` parses to): the literal pairs. A pair
/// valued by the macro's own keyword parameter `bound` is the call's own
/// keyword, which the association is parsed from. The literals are
/// taken to win over the call's `**name`, which holds only what the macro
/// has not consumed (Core's `options.delete(:optional)`).
fn passed_options_with(arg: &Expr, name: &Symbol, bound: &[Symbol]) -> Option<Vec<(Symbol, Expr)>> {
    let ExprNode::KeywordSplat { value } = &*arg.node else { return passed_options(arg, name) };
    let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*value.node else {
        return passed_options(arg, name);
    };
    let (ExprNode::Hash { entries, .. }, [passed]) = (&*recv.node, args.as_slice()) else { return passed_options(arg, name) };
    if method.as_str() != "merge" || !is_var(passed, name) {
        return passed_options(arg, name);
    }
    entries
        .iter()
        .filter_map(|(k, v)| {
            let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else { return Some(None) };
            match &*v.node {
                ExprNode::Lit { .. } => Some(Some((key.clone(), v.clone()))),
                ExprNode::Var { name: n, .. } if bound.contains(n) => None,
                _ => Some(None),
            }
        })
        .collect()
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
