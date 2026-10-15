//! `default_scope { where(is_not_deleted: true) }` and
//! `default_scope { extending(Mod) }`: the forms Core declares.
//!
//! Rails merges a class's default scope into every relation of it
//! (`all`, `where`, `find`, `count`, `exists?`, its associations), runs
//! the scope's `where` equalities through the writers of a new record
//! before the given attributes, and drops both under `unscoped`.
//!
//! The scope runs at RUNTIME, where every relation is built:
//! `ActiveRecord::Relation.new(Model)` (the compiled roots, the
//! association reads, the class finders) calls the model's
//! `_apply_default_scope(relation)`, which this pass emits as the
//! scope's chain on the relation. The relation's plain chain methods
//! spawn (Rails' immutable relations), so the chain is written with
//! their in-place bang forms (`where!`, `not!`, `order!`, …), which
//! mutate the relation under construction and answer it; the value is
//! not read. `Base` answers the identity for every other model, and
//! `_default_scoped?` lets the class finders that read the table
//! through the adapter (`count`, `find`, `exists?`, `last`,
//! `destroy_all`, the emitted `delete_all`) go through the relation
//! instead. `_default_scope_attributes` is the literal-keyed `where`
//! hash the new record takes (`Base#_seed_default_scope_attributes`,
//! called last by the model's `initialize`).
//!
//! The arel pass does not fold a query whose owner is default-scoped
//! (`ClassInfo::default_scoped`), so the query stays on the runtime
//! relation that applies it; a direct has_many reader of such a target
//! materializes that relation with `to_a`.
//!
//! An STI subclass's own default scope is emitted on the subclass
//! ([`push_subclass_methods`]), whose relations `sti_scope` roots on it
//! (`Sub.where(type: "Sub")`, `Sub.unscoped.where(type: "Sub")`), so its
//! `_apply_default_scope` runs the model's first through `super`.
//!
//! Rails' `Relation#new` is `build`. The runtime Relation has no `new`
//! (relation.rb: under spinel it is the constructor's C symbol), and an
//! `X.unscoped` chain stays untyped, out of `scope_chain`'s typed
//! `new` rewrite, so [`apply_unscoped_build`] calls its `build`.
//! A join onto a default-scoped model carries the model's conditions in
//! its ON clause (`scope_chain::join_spec_sql`, `Base._join_scope_sql`),
//! and the arel pass does not fold a query that preloads one
//! ([`scoped_preload`]).
//!
//! Reproduced: a block or lambda with no parameters whose body is a
//! chain of `where(...)`, `where.not(...)`, `order(...)`,
//! `extending(Const, ...)`, `preload(...)` and `includes(...)` on the
//! implicit self. A link may also be an argument-less call of one of the
//! model's own `scope`s without parameters whose body is such a chain
//! (`default_scope { active }`): Rails runs the scope on the default
//! relation, so its links are spliced in. Any other form (options such
//! as `all_queries:`, a scope with arguments, a conditional, an explicit
//! receiver) is not claimed and keeps its named refusal
//! (`model_to_library::unlowered_dsl_reason`).

use crate::dialect::{AccessorKind, LibraryClass, MethodDef, MethodReceiver, Model, ModelBodyItem, Param};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol, VarId};
use crate::span::Span;

/// One link of a default scope's chain: the relation method and its
/// arguments, as written (`where.not` is the relation's `not`).
#[derive(Clone, Debug)]
struct Link {
    method: &'static str,
    args: Vec<Expr>,
}

/// One `default_scope` declaration: its chain, and the load-time
/// condition it was declared under (`__when:`, below) when a concern's
/// `included do if cond; default_scope ...; end end` put one around it.
#[derive(Clone, Debug)]
struct Chain {
    links: Vec<Link>,
    guard: Option<Expr>,
}

/// The option a conditional default scope carries
/// ([`expand_conditional`]): not a Rails option, an internal marker the
/// concern splice writes for `default_scope ... if cond`.
const WHEN_OPTION: &str = "__when";

/// Whether a model-body statement is a `default_scope` this pass
/// reproduces (`model_to_library::MODEL_STATEMENT_CLAIMS`).
pub(crate) fn claims(expr: &Expr) -> bool {
    declaration(expr, None).is_some()
}

/// [`claims`] with `model`'s own scopes resolved (`default_scope {
/// active }`), the claim `model_to_library::claimed_in_model` asks.
pub(crate) fn claims_in(model: &Model, expr: &Expr) -> bool {
    declaration(expr, Some(model)).is_some()
}

/// A concern's `included do if cond; default_scope ...; scope :x, ...
/// else scope :x, ... end end`, which `Rails.configuration.platform_
/// essentials.yugabyte_testing?` makes Core's `GlobalDb::ShopIslandScoped`:
/// the includer's class body runs the branch its condition picks, once.
///
/// The condition is Ruby the emitted tree evaluates, so the branches are
/// kept and gated on it at run time rather than folded here. A branch
/// that declares only `default_scope`s and parameterless `scope`s is
/// rewritten into the items the model lowering already claims: each
/// `default_scope` gets the [`WHEN_OPTION`] marker (the condition, or its
/// negation under `else`), and each scope both arms declare becomes one
/// `Scope` whose body is `if cond; then_body; else; else_body; end`. A
/// scope only one arm declares has no meaning in the other, and any other
/// statement is not modelled: the item is returned unchanged for the
/// named refusal.
pub(crate) fn expand_conditional(item: ModelBodyItem) -> Vec<ModelBodyItem> {
    let ModelBodyItem::Unknown { expr, leading_comments, leading_blank_line } = &item else { return vec![item] };
    let ExprNode::If { cond, then_branch, else_branch } = &*expr.node else { return vec![item] };
    fn statements(branch: &Expr) -> Vec<&Expr> {
        match &*branch.node {
            ExprNode::Seq { exprs } => exprs.iter().collect(),
            ExprNode::Lit { value: Literal::Nil } => Vec::new(),
            _ => vec![branch],
        }
    }
    enum Declared<'a> {
        DefaultScope(&'a Expr),
        Scope { name: Symbol, body: &'a Expr },
    }
    fn declared(stmt: &Expr) -> Option<Declared<'_>> {
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*stmt.node else { return None };
        match (method.as_str(), args.as_slice()) {
            ("default_scope", _) => declaration(stmt, None).map(|_| Declared::DefaultScope(stmt)),
            ("scope", [name, body]) => {
                let ExprNode::Lit { value: Literal::Sym { value } } = &*name.node else { return None };
                Some(Declared::Scope { name: value.clone(), body: lambda_arg(body)? })
            }
            _ => None,
        }
    }
    let then_stmts: Option<Vec<Declared<'_>>> = statements(then_branch).into_iter().map(declared).collect();
    let else_stmts: Option<Vec<Declared<'_>>> = statements(else_branch).into_iter().map(declared).collect();
    let (Some(then_stmts), Some(else_stmts)) = (then_stmts, else_stmts) else { return vec![item] };
    if then_stmts.is_empty() && else_stmts.is_empty() {
        return vec![item];
    }
    // Every scope in an arm has its twin in the other.
    fn scope_bodies<'a>(stmts: &[Declared<'a>], name: &Symbol) -> Option<&'a Expr> {
        stmts.iter().find_map(|d| match d {
            Declared::Scope { name: n, body } if n == name => Some(*body),
            _ => None,
        })
    }
    let mut scope_names: Vec<&Symbol> = Vec::new();
    for d in then_stmts.iter().chain(&else_stmts) {
        if let Declared::Scope { name, .. } = d
            && !scope_names.contains(&name)
        {
            scope_names.push(name);
        }
    }
    if scope_names.iter().any(|n| scope_bodies(&then_stmts, n).is_none() || scope_bodies(&else_stmts, n).is_none()) {
        return vec![item];
    }

    let item_of = |expr: Expr| ModelBodyItem::Unknown {
        expr,
        leading_comments: leading_comments.clone(),
        leading_blank_line: *leading_blank_line,
    };
    let negated = || send(Some(cond.clone()), "!", vec![]);
    let guarded = |stmt: &Expr, guard: Expr| {
        let ExprNode::Send { recv, method, args, block, parenthesized } = &*stmt.node else { unreachable!("a default_scope") };
        let mut args = args.clone();
        let when = (synthetic(ExprNode::Lit { value: Literal::Sym { value: Symbol::from(WHEN_OPTION) } }), guard);
        match args.last_mut().map(|a| &mut *a.node) {
            Some(ExprNode::Hash { entries, .. }) => entries.push(when),
            _ => args.push(synthetic(ExprNode::Hash { entries: vec![when], kwargs: true })),
        }
        Expr::new(
            stmt.span,
            ExprNode::Send { recv: recv.clone(), method: method.clone(), args, block: block.clone(), parenthesized: *parenthesized },
        )
    };
    let mut out = Vec::new();
    for d in &then_stmts {
        if let Declared::DefaultScope(stmt) = d {
            out.push(item_of(guarded(stmt, cond.clone())));
        }
    }
    for d in &else_stmts {
        if let Declared::DefaultScope(stmt) = d {
            out.push(item_of(guarded(stmt, negated())));
        }
    }
    for name in scope_names {
        let (then_body, else_body) = (
            scope_bodies(&then_stmts, name).expect("checked"),
            scope_bodies(&else_stmts, name).expect("checked"),
        );
        out.push(ModelBodyItem::Scope {
            scope: crate::dialect::Scope {
                name: name.clone(),
                params: Vec::new(),
                body: Expr::new(
                    expr.span,
                    ExprNode::If { cond: cond.clone(), then_branch: then_body.clone(), else_branch: else_body.clone() },
                ),
            },
            leading_comments: leading_comments.clone(),
            leading_blank_line: *leading_blank_line,
        });
    }
    out
}

/// Whether `model` itself declares a reproduced default scope.
pub(crate) fn declares(model: &Model) -> bool {
    !chains(model).is_empty()
}

/// Whether `model` or a model ancestor declares one (an STI subclass
/// inherits its parent's default scope, and the class methods with it).
pub(crate) fn applies(model: &Model, models: &[Model]) -> bool {
    let mut cursor = Some(model);
    for _ in 0..16 {
        let Some(m) = cursor else { return false };
        if declares(m) {
            return true;
        }
        cursor = m.parent.as_ref().and_then(|p| models.iter().find(|o| &o.name == p));
    }
    false
}

/// Whether the STI subclass `lc` declares a reproduced default scope of
/// its own (`sti_scope` roots its relations on it and emits the class
/// methods, [`push_subclass_methods`]).
pub(crate) fn subclass_declares(lc: &LibraryClass) -> bool {
    lc.unknown_calls.iter().any(claims)
}

/// The reproduced default scopes `model` declares, in body order.
fn chains(model: &Model) -> Vec<Chain> {
    model
        .body
        .iter()
        .filter_map(|item| match item {
            ModelBodyItem::Unknown { expr, .. } | ModelBodyItem::ClassBody { expr, .. } => declaration(expr, Some(model)),
            _ => None,
        })
        .collect()
}

/// `default_scope { chain }`, `default_scope -> { chain }` or
/// `default_scope lambda { chain }`, and nothing else, with the options
/// Rails documents that change no relation this port builds
/// (`all_queries:` widens the scope to UPDATE/DELETE statements, which
/// the instance writers key by primary key) and the [`WHEN_OPTION`]
/// marker. `model` resolves the chain's calls of its scopes.
fn declaration(expr: &Expr, model: Option<&Model>) -> Option<Chain> {
    let ExprNode::Send { recv: None, method, args, block, .. } = &*expr.node else { return None };
    if method.as_str() != "default_scope" {
        return None;
    }
    let (args, options) = match args.split_last() {
        Some((last, rest)) if matches!(&*last.node, ExprNode::Hash { .. }) => (rest, Some(last)),
        _ => (args.as_slice(), None),
    };
    let mut guard = None;
    if let Some(options) = options {
        let ExprNode::Hash { entries, .. } = &*options.node else { return None };
        for (key, value) in entries {
            let ExprNode::Lit { value: Literal::Sym { value: key } } = &*key.node else { return None };
            match key.as_str() {
                "all_queries" if matches!(&*value.node, ExprNode::Lit { value: Literal::Bool { .. } }) => {}
                WHEN_OPTION => guard = Some(value.clone()),
                _ => return None,
            }
        }
    }
    let body = match (args, block) {
        ([], Some(block)) => lambda_body(block)?,
        ([arg], None) => lambda_arg(arg)?,
        _ => return None,
    };
    let links = chain(single(body), model, 0)?;
    (!links.is_empty()).then_some(Chain { links, guard })
}

/// A one-statement body's statement.
fn single(body: &Expr) -> &Expr {
    match &*body.node {
        ExprNode::Seq { exprs } if exprs.len() == 1 => &exprs[0],
        _ => body,
    }
}

/// The links of `model`'s parameterless `scope name`, when its body is
/// itself a reproduced chain (scopes calling scopes, a few deep).
fn scope_links(model: Option<&Model>, name: &str, depth: u8) -> Option<Vec<Link>> {
    if depth >= 4 {
        return None;
    }
    let scope = model?.scopes().find(|s| s.name.as_str() == name)?;
    if !scope.params.is_empty() {
        return None;
    }
    chain(single(&scope.body), model, depth + 1).filter(|links| !links.is_empty())
}

/// The body of a parameterless `-> { }` or `lambda { }`.
fn lambda_arg(arg: &Expr) -> Option<&Expr> {
    lambda_body(arg).or_else(|| match &*arg.node {
        ExprNode::Send { recv: None, method, args, block: Some(block), .. }
            if method.as_str() == "lambda" && args.is_empty() =>
        {
            lambda_body(block)
        }
        _ => None,
    })
}

fn lambda_body(expr: &Expr) -> Option<&Expr> {
    match &*expr.node {
        ExprNode::Lambda { params, rest_param: None, body, .. } if params.is_empty() => Some(body),
        _ => None,
    }
}

/// The links of an implicit-self relation chain, root first.
fn chain(expr: &Expr, model: Option<&Model>, depth: u8) -> Option<Vec<Link>> {
    let ExprNode::Send { recv, method, args, block: None, .. } = &*expr.node else { return None };
    // `where.not(...)`: a bare `where` receiver, then `not`.
    if method.as_str() == "not" {
        let Some(recv) = recv else { return None };
        let ExprNode::Send { recv: inner, method: w, args: wargs, block: None, .. } = &*recv.node else {
            return None;
        };
        if w.as_str() != "where" || !wargs.is_empty() || args.is_empty() {
            return None;
        }
        let mut links = match inner {
            None => Vec::new(),
            Some(inner) => chain(inner, model, depth)?,
        };
        links.push(Link { method: "not", args: args.clone() });
        return Some(links);
    }
    let name: &'static str = match method.as_str() {
        "where" => "where",
        "order" => "order",
        "extending" => "extending",
        "preload" => "preload",
        "includes" => "includes",
        scope if args.is_empty() => {
            let mut links = match recv {
                None => Vec::new(),
                Some(recv) => chain(recv, model, depth)?,
            };
            links.extend(scope_links(model, scope, depth)?);
            return Some(links);
        }
        _ => return None,
    };
    if args.is_empty() || args.iter().any(|a| matches!(&*a.node, ExprNode::Splat { .. } | ExprNode::ForwardArgs)) {
        return None;
    }
    if name == "extending" && !args.iter().all(|a| matches!(&*a.node, ExprNode::Const { .. })) {
        return None;
    }
    let mut links = match recv {
        None => Vec::new(),
        Some(recv) => chain(recv, model, depth)?,
    };
    links.push(Link { method: name, args: args.clone() });
    Some(links)
}

/// The `where` equalities a new record takes: symbol keys of a `where`
/// hash, a later scope's value winning. Array and range values (an IN or
/// a BETWEEN) are not assigned.
fn scope_attributes<'a>(chains: impl Iterator<Item = &'a Chain>) -> Vec<(Expr, Expr)> {
    let mut out: Vec<(Expr, Expr)> = Vec::new();
    for link in chains.flat_map(|c| &c.links) {
        if link.method != "where" {
            continue;
        }
        let [arg] = link.args.as_slice() else { continue };
        let ExprNode::Hash { entries, .. } = &*arg.node else { continue };
        for (k, v) in entries {
            let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else { continue };
            if matches!(&*v.node, ExprNode::Array { .. } | ExprNode::Range { .. } | ExprNode::Hash { .. }) {
                continue;
            }
            out.retain(|(ok, _)| !matches!(&*ok.node, ExprNode::Lit { value: Literal::Sym { value } } if value == key));
            out.push((k.clone(), v.clone()));
        }
    }
    out
}

fn synthetic(node: ExprNode) -> Expr {
    Expr::new(Span::synthetic(), node)
}

fn send(recv: Option<Expr>, method: &str, args: Vec<Expr>) -> Expr {
    synthetic(ExprNode::Send { recv, method: Symbol::from(method), args, block: None, parenthesized: true })
}

fn class_method(model: &ClassId, name: &str, params: Vec<Param>, body: Expr) -> MethodDef {
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: Span::synthetic(),
        name: Symbol::from(name),
        receiver: MethodReceiver::Class,
        params,
        body,
        signature: None,
        effects: crate::effect::EffectSet::default(),
        enclosing_class: Some(model.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

/// The class method that answers a guarded chain's condition: the
/// model's `included do if cond` ran once at load in Rails, so the
/// first answer is kept (`@x = (cond) ? true : false if @x.nil?`).
fn guard_method(model: &ClassId, name: &str, cond: &Expr) -> MethodDef {
    let ivar = || synthetic(ExprNode::Ivar { name: Symbol::from(name) });
    let truth = synthetic(ExprNode::If {
        cond: cond.clone(),
        then_branch: synthetic(ExprNode::Lit { value: Literal::Bool { value: true } }),
        else_branch: synthetic(ExprNode::Lit { value: Literal::Bool { value: false } }),
    });
    let store = synthetic(ExprNode::Assign { target: crate::expr::LValue::Ivar { name: Symbol::from(name) }, value: truth });
    let first = synthetic(ExprNode::If {
        cond: send(Some(ivar()), "nil?", vec![]),
        then_branch: store,
        else_branch: synthetic(ExprNode::Lit { value: Literal::Nil }),
    });
    class_method(model, name, vec![], synthetic(ExprNode::Seq { exprs: vec![first, ivar()] }))
}

/// `_default_scope_guard_<model>_<n>`: unique per declaring model and
/// declaration, since a subclass's own guard must not shadow its parent's.
fn guard_method_name(model: &ClassId, index: usize) -> String {
    let model: String = model.0.as_str().chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    format!("_default_scope_guard_{model}_{index}")
}

/// The default-scoped model's class methods (Ruby-emit seam, beside the
/// scope methods): `_default_scoped?`, `_apply_default_scope(relation)`
/// and `_default_scope_attributes`, each after its parent's (`super`),
/// as Rails applies a parent's default scopes first. Also guards the
/// emitted whole-table `delete_all`, which Rails scopes.
pub(crate) fn push_methods(methods: &mut Vec<MethodDef>, model: &Model) {
    push_chain_methods(methods, &model.name, &chains(model));
}

/// The same class methods on an STI subclass (a library class), whose
/// `super` is its model's: Rails applies the parent's default scope,
/// then the subclass's own.
pub(crate) fn push_subclass_methods(lc: &mut LibraryClass) {
    let chains: Vec<Chain> = lc.unknown_calls.iter().filter_map(|e| declaration(e, None)).collect();
    let name = lc.name.clone();
    push_chain_methods(&mut lc.methods, &name, &chains);
}

fn push_chain_methods(methods: &mut Vec<MethodDef>, name: &ClassId, chains: &[Chain]) {
    if chains.is_empty() {
        return;
    }
    let relation = Symbol::from("relation");
    let rel = || synthetic(ExprNode::Var { id: VarId(0), name: relation.clone() });

    // A guarded chain's condition is a class method per declaration,
    // evaluated once like Rails' class-body `if` (`guard_method`).
    let guard_names: Vec<Option<String>> = chains
        .iter()
        .enumerate()
        .map(|(i, c)| c.guard.as_ref().map(|_| guard_method_name(name, i)))
        .collect();
    let guard_call = |i: usize| send(None, guard_names[i].as_deref().expect("guarded chain"), vec![]);
    for (i, chain) in chains.iter().enumerate() {
        if let Some(guard) = &chain.guard {
            methods.push(guard_method(name, guard_names[i].as_deref().expect("guarded chain"), guard));
        }
    }

    // Applies when any chain does: an unguarded one always, a guarded
    // one when its condition held.
    let applies = if chains.iter().any(|c| c.guard.is_none()) {
        synthetic(ExprNode::Lit { value: Literal::Bool { value: true } })
    } else {
        let parent = synthetic(ExprNode::Super { args: None, block: None });
        let mut any = parent;
        for i in 0..chains.len() {
            any = synthetic(ExprNode::BoolOp {
                op: crate::expr::BoolOpKind::Or,
                surface: crate::expr::BoolOpSurface::Symbol,
                left: any,
                right: guard_call(i),
            });
        }
        any
    };
    methods.push(class_method(name, "_default_scoped?", vec![], applies));

    let mut stmts = vec![synthetic(ExprNode::Super { args: Some(vec![rel()]), block: None })];
    for (i, chain) in chains.iter().enumerate() {
        let mut call = rel();
        for link in &chain.links {
            call = send(Some(call), &format!("{}!", link.method), link.args.clone());
        }
        stmts.push(match chain.guard {
            None => call,
            Some(_) => synthetic(ExprNode::If {
                cond: guard_call(i),
                then_branch: call,
                else_branch: synthetic(ExprNode::Lit { value: Literal::Nil }),
            }),
        });
    }
    stmts.push(rel());
    methods.push(class_method(
        name,
        "_apply_default_scope",
        vec![Param::positional(relation.clone())],
        synthetic(ExprNode::Seq { exprs: stmts }),
    ));

    // The seeded attributes, in declaration order: a run of unguarded
    // chains is one `merge`, a guarded chain its own (its hash only when
    // its condition holds).
    let mut merged = synthetic(ExprNode::Super { args: None, block: None });
    let mut any_attributes = false;
    let mut i = 0;
    while i < chains.len() {
        let (group, guard) = match chains[i].guard {
            Some(_) => (i..i + 1, Some(i)),
            None => (i..chains[i..].iter().position(|c| c.guard.is_some()).map_or(chains.len(), |p| i + p), None),
        };
        i = group.end;
        let attributes = scope_attributes(chains[group].iter());
        if attributes.is_empty() {
            continue;
        }
        any_attributes = true;
        let hash = synthetic(ExprNode::Hash { entries: attributes, kwargs: false });
        let hash = match guard {
            None => hash,
            Some(g) => synthetic(ExprNode::If {
                cond: guard_call(g),
                then_branch: hash,
                else_branch: synthetic(ExprNode::Hash { entries: vec![], kwargs: false }),
            }),
        };
        merged = send(Some(merged), "merge", vec![hash]);
    }
    if any_attributes {
        methods.push(class_method(name, "_default_scope_attributes", vec![], merged));
    }

    // `Model.delete_all` is the scoped DELETE under a default scope.
    if let Some(delete_all) = methods
        .iter_mut()
        .find(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "delete_all" && m.params.is_empty())
    {
        let scoped = send(
            Some(send(
                Some(synthetic(ExprNode::Const { path: vec![Symbol::from("ActiveRecord"), Symbol::from("Relation")] })),
                "new",
                vec![synthetic(ExprNode::SelfRef)],
            )),
            "delete_all",
            vec![],
        );
        let guard = synthetic(ExprNode::If {
            cond: send(None, "_default_scope_applies?", vec![]),
            then_branch: synthetic(ExprNode::Seq {
                exprs: vec![scoped, synthetic(ExprNode::Return { value: synthetic(ExprNode::Lit { value: Literal::Nil }) })],
            }),
            else_branch: synthetic(ExprNode::Lit { value: Literal::Nil }),
        });
        let body = std::mem::replace(&mut delete_all.body, synthetic(ExprNode::Lit { value: Literal::Nil }));
        let mut exprs = vec![guard];
        match *body.node {
            ExprNode::Seq { exprs: rest } => exprs.extend(rest),
            node => exprs.push(Expr::new(body.span, node)),
        }
        delete_all.body = synthetic(ExprNode::Seq { exprs });
    }
}

/// Whether `owner`, or a class it inherits from, is a default-scoped
/// model in the arel pass's registry: its query is left to the runtime
/// relation, which applies the scope.
pub(crate) fn scoped_owner(
    registry: &crate::det::HashMap<ClassId, crate::analyze::ClassInfo>,
    owner: &ClassId,
) -> bool {
    let mut cursor = Some(owner);
    for _ in 0..16 {
        let Some(info) = cursor.and_then(|c| registry.get(c)) else { return false };
        if info.default_scoped {
            return true;
        }
        cursor = info.parent.as_ref();
    }
    false
}

/// Whether a folded query preloads a default-scoped model: the folded
/// batch reads the target table directly, so the query stays on the
/// runtime relation, whose preload applies the target's scope.
pub(crate) fn scoped_preload(
    op: &crate::lower::arel::ArelOp,
    registry: &crate::det::HashMap<ClassId, crate::analyze::ClassInfo>,
) -> bool {
    matches!(op, crate::lower::arel::ArelOp::Select(sel)
        if sel.preloads.iter().any(|p| scoped_owner(registry, &p.target_class)))
}

/// Relation methods an `X.unscoped` chain keeps through to its `new`.
const UNSCOPED_CHAIN: &[&str] = &[
    "where", "not", "order", "reorder", "joins", "left_joins", "left_outer_joins", "includes", "preload",
    "eager_load", "limit", "offset", "select", "distinct", "group", "having", "extending", "all",
];

/// `X.unscoped.where(name: "u").new(attrs)` -> `X.unscoped.where(name:
/// "u").build(attrs)`, in an app with a reproduced default scope.
pub fn apply_unscoped_build(app: &mut crate::app::App) {
    if !app.models.iter().any(declares) && !app.library_classes.iter().any(subclass_declares) {
        return;
    }
    super::for_each_hook_body(app, &mut rename_unscoped_new);
    for view in &mut app.views {
        rename_unscoped_new(&mut view.body);
    }
    super::for_each_test_body(app, &mut rename_unscoped_new);
}

fn rename_unscoped_new(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut rename_unscoped_new);
    let ExprNode::Send { recv: Some(r), method, block: None, .. } = &mut *expr.node else { return };
    if method.as_str() == "new" && unscoped_chain(r) {
        *method = Symbol::from("build");
    }
}

fn unscoped_chain(expr: &Expr) -> bool {
    let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &*expr.node else { return false };
    if method.as_str() == "unscoped" {
        return args.is_empty() && matches!(&*r.node, ExprNode::Const { .. });
    }
    UNSCOPED_CHAIN.contains(&method.as_str()) && unscoped_chain(r)
}

/// A direct has_many reader of a default-scoped target: its
/// `Target.where(fk: @id)` stays a runtime relation (the arel pass does
/// not fold it), which the reader materializes, `.to_a`, so it still
/// answers the Array its signature states.
pub(crate) fn materialize_reader(reader: Option<&mut MethodDef>, target: &ClassId, models: &[Model], through: bool) {
    let Some(reader) = reader else { return };
    if through || !models.iter().find(|m| &m.name == target).is_some_and(|t| applies(t, models)) {
        return;
    }
    let ExprNode::Seq { exprs } = &mut *reader.body.node else { return };
    let Some(query) = exprs.pop() else { return };
    exprs.push(send(Some(query), "to_a", vec![]));
}
