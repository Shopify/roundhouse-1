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
//! scope's chain on the relation. The relation chain methods mutate in
//! place and return self, so the chain needs no spawn and its value is
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
//! implicit self. Any other form (options such as `all_queries:`, a
//! scope call, a conditional, an explicit receiver) is not claimed and
//! keeps its named refusal (`model_to_library::unlowered_dsl_reason`).

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

/// Whether a model-body statement is a `default_scope` this pass
/// reproduces (`model_to_library::MODEL_STATEMENT_CLAIMS`).
pub(crate) fn claims(expr: &Expr) -> bool {
    declaration(expr).is_some()
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
fn chains(model: &Model) -> Vec<Vec<Link>> {
    model
        .body
        .iter()
        .filter_map(|item| match item {
            ModelBodyItem::Unknown { expr, .. } | ModelBodyItem::ClassBody { expr, .. } => declaration(expr),
            _ => None,
        })
        .collect()
}

/// `default_scope { chain }`, `default_scope -> { chain }` or
/// `default_scope lambda { chain }`, and nothing else.
fn declaration(expr: &Expr) -> Option<Vec<Link>> {
    let ExprNode::Send { recv: None, method, args, block, .. } = &*expr.node else { return None };
    if method.as_str() != "default_scope" {
        return None;
    }
    let body = match (args.as_slice(), block) {
        ([], Some(block)) => lambda_body(block)?,
        ([arg], None) => lambda_body(arg).or_else(|| match &*arg.node {
            ExprNode::Send { recv: None, method, args, block: Some(block), .. }
                if method.as_str() == "lambda" && args.is_empty() =>
            {
                lambda_body(block)
            }
            _ => None,
        })?,
        _ => return None,
    };
    let body = match &*body.node {
        ExprNode::Seq { exprs } if exprs.len() == 1 => &exprs[0],
        _ => body,
    };
    let links = chain(body)?;
    (!links.is_empty()).then_some(links)
}

fn lambda_body(expr: &Expr) -> Option<&Expr> {
    match &*expr.node {
        ExprNode::Lambda { params, rest_param: None, body, .. } if params.is_empty() => Some(body),
        _ => None,
    }
}

/// The links of an implicit-self relation chain, root first.
fn chain(expr: &Expr) -> Option<Vec<Link>> {
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
            Some(inner) => chain(inner)?,
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
        Some(recv) => chain(recv)?,
    };
    links.push(Link { method: name, args: args.clone() });
    Some(links)
}

/// The `where` equalities a new record takes: symbol keys of a `where`
/// hash, a later scope's value winning. Array and range values (an IN or
/// a BETWEEN) are not assigned.
fn scope_attributes(chains: &[Vec<Link>]) -> Vec<(Expr, Expr)> {
    let mut out: Vec<(Expr, Expr)> = Vec::new();
    for link in chains.iter().flatten() {
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
    let chains: Vec<Vec<Link>> = lc.unknown_calls.iter().filter_map(declaration).collect();
    let name = lc.name.clone();
    push_chain_methods(&mut lc.methods, &name, &chains);
}

fn push_chain_methods(methods: &mut Vec<MethodDef>, name: &ClassId, chains: &[Vec<Link>]) {
    if chains.is_empty() {
        return;
    }
    let relation = Symbol::from("relation");
    let rel = || synthetic(ExprNode::Var { id: VarId(0), name: relation.clone() });

    methods.push(class_method(name, "_default_scoped?", vec![], synthetic(ExprNode::Lit { value: Literal::Bool { value: true } })));

    let mut stmts = vec![synthetic(ExprNode::Super { args: Some(vec![rel()]), block: None })];
    for links in chains {
        let mut call = rel();
        for link in links {
            call = send(Some(call), link.method, link.args.clone());
        }
        stmts.push(call);
    }
    stmts.push(rel());
    methods.push(class_method(
        name,
        "_apply_default_scope",
        vec![Param::positional(relation.clone())],
        synthetic(ExprNode::Seq { exprs: stmts }),
    ));

    let attributes = scope_attributes(chains);
    if !attributes.is_empty() {
        let parent = synthetic(ExprNode::Super { args: None, block: None });
        let hash = synthetic(ExprNode::Hash { entries: attributes, kwargs: false });
        methods.push(class_method(name, "_default_scope_attributes", vec![], send(Some(parent), "merge", vec![hash])));
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
