//! `normalizes :email_address, with: ->(e) { e.strip.downcase }` (Rails
//! 7.1) — the declaration the Rails 8 authentication generator puts on
//! every `User`.
//!
//! Rails applies the normalization in two places, and both matter for a
//! sign-in form:
//!
//!   - on assignment: `User.new(email_address: " A@B.C ").email_address`
//!     is `"a@b.c"`, and so is what gets saved;
//!   - to the matching keyword of a finder: `User.find_by(email_address:
//!     " A@B.C ")` finds that row, which is what lets a user sign in with
//!     the capitalization they typed.
//!
//! The lambda becomes one synthesized class method,
//! `Model._normalize_<attr>(value)`, with the lambda's body inlined over
//! `value` and nil passed through (Rails' `apply_to_nil: false`
//! default). The column writer calls it, and the finder rewrite below
//! wraps the keyword's value in it — one copy of the body, and the
//! value expression evaluated once.
//!
//! DIVERGENCE: hydration assigns through the same column writer, so a
//! row loaded from the database is normalized too, where Rails leaves an
//! existing row's value alone until it is reassigned. The two agree for
//! every row written by the app after the declaration.
//!
//! Not reproduced, and left alone rather than half-applied: a `with:`
//! that is not a one-parameter lambda literal, and `apply_to_nil: true`.

use crate::det::HashMap;

use indexmap::IndexMap;

use crate::app::App;
use crate::dialect::{AccessorKind, MethodDef, MethodReceiver, Model, ModelBodyItem, Param};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol, VarId};
use crate::span::Span;
use crate::ty::Ty;

/// The normalizing class method's name for `attr`.
pub(crate) fn normalizer_name(attr: &Symbol) -> Symbol {
    Symbol::from(format!("_normalize_{}", attr.as_str()))
}

/// The finder-side normalizing class method's name for `attr`: the
/// normalizer applied to each element of an Array value (`where(handle:
/// handles)` normalizes every handle, as Rails' predicate builder does),
/// and to anything else as `_normalize_<attr>` would.
pub(crate) fn query_normalizer_name(attr: &Symbol) -> Symbol {
    Symbol::from(format!("_query_{}_normalized", attr.as_str()))
}

/// Every attribute `model` declares a reproducible normalization for,
/// with the lambda's parameter and body, in declaration order (an
/// `IndexMap`: a repeated `normalizes :attr` keeps its first position
/// but the later declaration's body wins, same as a Ruby `Hash`).
pub(crate) fn normalizations(model: &Model) -> IndexMap<Symbol, (Symbol, Expr)> {
    let mut out = IndexMap::new();
    for item in &model.body {
        let ModelBodyItem::Unknown { expr, .. } = item else { continue };
        let Some((attrs, with)) = declaration(expr) else { continue };
        for attr in attrs {
            out.insert(attr, with.clone());
        }
    }
    out
}

/// Whether a model-body statement is a `normalizes` declaration this
/// pass reproduces (`model_to_library::unclaimed_unknowns` skips it).
pub(crate) fn claims(expr: &Expr) -> bool {
    declaration(expr).is_some()
}

/// A reproducible `normalizes :a, :b, with: ->(v) { ... }`: its
/// attributes, and the lambda's parameter and body.
fn declaration(expr: &Expr) -> Option<(Vec<Symbol>, (Symbol, Expr))> {
    let ExprNode::Send { recv: None, method, args, .. } = &*expr.node else { return None };
    if method.as_str() != "normalizes" {
        return None;
    }
    let mut attrs = Vec::new();
    let mut with = None;
    let mut supported = true;
    for arg in args {
        match &*arg.node {
            ExprNode::Lit { value: Literal::Sym { value } } => attrs.push(value.clone()),
            ExprNode::Hash { entries, .. } => {
                for (k, v) in entries {
                    let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else {
                        supported = false;
                        continue;
                    };
                    match (key.as_str(), &*v.node) {
                        ("with", ExprNode::Lambda { params, rest_param: None, body, signature, extra_params, .. })
                            if params.len() == 1 && signature.is_empty() && extra_params.is_empty() =>
                        {
                            with = Some((params[0].clone(), body.clone()));
                        }
                        ("apply_to_nil", ExprNode::Lit { value: Literal::Bool { value: false } }) => {}
                        _ => supported = false,
                    }
                }
            }
            _ => supported = false,
        }
    }
    let (Some(with), true) = (with, supported) else { return None };
    Some((attrs, with))
}

/// `Model._normalize_<attr>(value)` for each normalized attribute that
/// is one of the model's columns: `value.nil? ? nil : <body>`.
pub(crate) fn push_normalize_methods(methods: &mut Vec<MethodDef>, model: &Model, columns: &HashMap<Symbol, Ty>) {
    for (attr, (param, body)) in normalizations(model) {
        let Some(col_ty) = columns.get(&attr) else { continue };
        let name = normalizer_name(&attr);
        if methods.iter().any(|m| m.receiver == MethodReceiver::Class && m.name == name) {
            continue;
        }
        let value = Symbol::from("value");
        let read = || synthetic(ExprNode::Var { id: VarId(0), name: value.clone() });
        let mut normalized = body.clone();
        crate::lower::case_lambda::subst(&mut normalized, &param, &read());
        // The lambda's body is the method's tail, so its own `next v`
        // (and a lambda's `break v`) is the method's `return v`.
        crate::lower::lifted_block::to_method_body(&mut normalized, crate::lower::lifted_block::Form::Lambda);
        let nil_check = synthetic(ExprNode::Send {
            recv: Some(read()),
            method: Symbol::from("nil?"),
            args: vec![],
            block: None,
            parenthesized: false,
        });
        let body = synthetic(ExprNode::If {
            cond: nil_check,
            then_branch: synthetic(ExprNode::Lit { value: Literal::Nil }),
            else_branch: normalized,
        });
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: Span::synthetic(),
            name,
            receiver: MethodReceiver::Class,
            params: vec![Param::positional(value.clone())],
            body,
            signature: Some(super::model_to_library::fn_sig(vec![(value.clone(), col_ty.clone())], col_ty.clone())),
            effects: crate::effect::EffectSet::default(),
            enclosing_class: Some(model.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });

        // `Model._query_<attr>_normalized(value)`: an Array maps through
        // the normalizer element by element, anything else is one value.
        let query_name = query_normalizer_name(&attr);
        let item = Symbol::from("item");
        let item_ref = synthetic(ExprNode::Var { id: VarId(0), name: item.clone() });
        let is_array = synthetic(ExprNode::Send {
            recv: Some(read()),
            method: Symbol::from("is_a?"),
            args: vec![synthetic(ExprNode::Const { path: vec![Symbol::from("Array")] })],
            block: None,
            parenthesized: true,
        });
        let each_item = synthetic(ExprNode::Send {
            recv: Some(read()),
            method: Symbol::from("map"),
            args: vec![],
            block: Some(synthetic(ExprNode::Lambda {
                signature: Default::default(),
                extra_params: Vec::new(),
                params: vec![item],
                rest_param: None,
                block_param: None,
                body: call_normalizer(&model.name, &attr, item_ref),
                block_style: crate::expr::BlockStyle::Brace,
            })),
            parenthesized: false,
        });
        let query_body = synthetic(ExprNode::If {
            cond: is_array,
            then_branch: each_item,
            else_branch: call_normalizer(&model.name, &attr, read()),
        });
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: Span::synthetic(),
            name: query_name,
            receiver: MethodReceiver::Class,
            params: vec![Param::positional(value.clone())],
            body: query_body,
            signature: Some(super::model_to_library::fn_sig(vec![(value.clone(), Ty::Untyped)], Ty::Untyped)),
            effects: crate::effect::EffectSet::default(),
            enclosing_class: Some(model.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }
}

/// The column writer's right-hand side for a normalized attribute:
/// `Model._normalize_<attr>(value)`.
pub(crate) fn normalized_write(model: &Model, attr: &Symbol, value: Expr) -> Option<Expr> {
    if !normalizations(model).contains_key(attr) {
        return None;
    }
    Some(call_normalizer(&model.name, attr, value))
}

fn call_normalizer(model: &ClassId, attr: &Symbol, value: Expr) -> Expr {
    let mut e = Expr::new(
        value.span,
        ExprNode::Send {
            recv: Some(Expr::new(
                value.span,
                ExprNode::Const { path: model.0.as_str().split("::").map(Symbol::from).collect() },
            )),
            method: normalizer_name(attr),
            args: vec![value.clone()],
            block: None,
            parenthesized: true,
        },
    );
    e.ty = value.ty.clone();
    e
}

/// Finder keywords: `Model.find_by(email_address: v)` (and `find_by!`,
/// `where`, `exists?`, `find_or_create_by`, `find_or_initialize_by`, on
/// the class or a relation over it) normalize `v` the way assignment
/// would. Runs after every pass that synthesizes a `find_by`
/// (`authenticate_by` expands into one).
pub fn apply_normalizes_finder_lowering(app: &mut App) {
    let normalized: HashMap<ClassId, Vec<Symbol>> = app
        .models
        .iter()
        .filter_map(|m| {
            let attrs: Vec<Symbol> = normalizations(m).into_keys().collect();
            (!attrs.is_empty()).then(|| (m.name.clone(), attrs))
        })
        .collect();
    if normalized.is_empty() {
        return;
    }
    super::for_each_hook_body(app, &mut |body| rewrite(body, &normalized));
}

const FINDERS: &[&str] =
    &["find_by", "find_by!", "where", "exists?", "find_or_create_by", "find_or_initialize_by"];

fn rewrite(e: &mut Expr, normalized: &HashMap<ClassId, Vec<Symbol>>) {
    e.node.for_each_child_mut(&mut |c| rewrite(c, normalized));
    let ExprNode::Send { recv: Some(r), method, args, .. } = &mut *e.node else { return };
    if !FINDERS.contains(&method.as_str()) {
        return;
    }
    let class = match r.ty.as_ref() {
        Some(Ty::Class { id, .. }) => id.clone(),
        Some(Ty::Relation { of }) => of.clone(),
        _ => match &*r.node {
            ExprNode::Const { path } => {
                ClassId(Symbol::from(path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::")))
            }
            _ => return,
        },
    };
    let Some(attrs) = normalized.get(&class) else { return };
    for arg in args.iter_mut() {
        let ExprNode::Hash { entries, .. } = &mut *arg.node else { continue };
        for (k, v) in entries.iter_mut() {
            let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else { continue };
            if !attrs.contains(key) || is_normalizer_call(v) {
                continue;
            }
            *v = call_query_normalizer(&class, key, v.clone());
        }
    }
}

fn call_query_normalizer(model: &ClassId, attr: &Symbol, value: Expr) -> Expr {
    let mut e = call_normalizer(model, attr, value);
    let ExprNode::Send { method, .. } = &mut *e.node else { unreachable!() };
    *method = query_normalizer_name(attr);
    e
}

/// Already wrapped — the rewrite must be idempotent, since a body can be
/// visited twice (a concern's copy and its includer's).
fn is_normalizer_call(e: &Expr) -> bool {
    matches!(&*e.node, ExprNode::Send { method, .. }
        if method.as_str().starts_with("_normalize_")
            || (method.as_str().starts_with("_query_") && method.as_str().ends_with("_normalized")))
}

fn synthetic(node: ExprNode) -> Expr {
    Expr::new(Span::synthetic(), node)
}
