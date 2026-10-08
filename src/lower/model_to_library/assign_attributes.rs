//! The compiled `initialize`, `update` and `update!` routed through
//! `assign_attributes`, on the PostgreSQL CRuby tree, whose runtime ships
//! ActiveModel::AttributeAssignment and ActiveRecord::AttributeAssignment
//! (runtime/spinel/cruby/active_record_core.rb).
//!
//! Rails' `new(attributes)` starts from the class's default attributes,
//! without calling a writer, and then `assign_attributes(attributes)`
//! sends `<key>=` for each key in the hash's order, raising
//! ActiveModel::UnknownAttributeError for a key with no writer
//! (activerecord 8.2.0.alpha core.rb `initialize`, attribute_assignment.rb);
//! `update(attributes)` is `assign_attributes(attributes)` and `save`
//! (persistence.rb). The synthesized `initialize` instead writes every
//! column in column order, each from its key or its default, and then the
//! virtual keys it knows of; `update` writes each known key it is given.
//! So a writer an override or a gem's `_assign_attributes` hook would
//! see, the order it is called in, and an unknown key all differed.
//!
//! This pass keeps the synthesized `initialize`'s default writes, with
//! every given value folded away (`attrs[:k]` is nil, `attrs.key?(:k)`
//! false outside a statement guard), and appends `assign_attributes(attrs)
//! unless attrs.equal?(HYDRATE_ATTRS)` before the `after_initialize`
//! tail; a hydration factory's record gets its columns from the row
//! (`schema::hydrate_attrs_const`). `update`/`update!` assign through
//! `assign_attributes` and answer their `save`/`save!`.
//!
//! The Ruby family's GeneratedAttributeMethods emit writes the defaults
//! through the module's own writers (`emit::ruby::library`), so a class's
//! override of a column writer does not run for them.
use crate::dialect::{MethodDef, MethodReceiver};
use crate::expr::{BoolOpKind, Expr, ExprNode, Literal};
use crate::ident::Symbol;
use crate::span::Span;

/// Rewrite the synthesized `initialize`, `update` and `update!` in place.
pub(super) fn route_through_assign_attributes(methods: &mut [MethodDef]) {
    for method in methods.iter_mut() {
        if method.receiver != MethodReceiver::Instance || !method.name_span.is_synthetic() {
            continue;
        }
        let Some(attrs) = single_param(method) else { continue };
        match method.name.as_str() {
            "initialize" => route_initialize(method, &attrs),
            "update" | "update!" => route_update(method, &attrs),
            _ => {}
        }
    }
}

fn single_param(method: &MethodDef) -> Option<Symbol> {
    match method.params.as_slice() {
        [param] => Some(param.name.clone()),
        _ => None,
    }
}

fn route_initialize(method: &mut MethodDef, attrs: &Symbol) {
    let ExprNode::Seq { exprs } = &mut *method.body.node else { return };
    let stmts = std::mem::take(exprs);
    let mut routed = Vec::with_capacity(stmts.len() + 1);
    let mut assigned = false;
    for stmt in stmts {
        if is_after_initialize_tail(&stmt) && !assigned {
            routed.push(assign_attributes_unless_hydrating(attrs));
            assigned = true;
        }
        if let Some(stmt) = defaults_only(stmt, attrs) {
            routed.push(stmt);
        }
    }
    if !assigned {
        routed.push(assign_attributes_unless_hydrating(attrs));
    }
    *exprs = routed;
}

fn route_update(method: &mut MethodDef, attrs: &Symbol) {
    let ExprNode::Seq { exprs } = &mut *method.body.node else { return };
    let Some(save) = exprs.iter().position(|e| {
        matches!(&*e.node, ExprNode::Send { recv: None, method, args, .. }
            if args.is_empty() && matches!(method.as_str(), "save" | "save!"))
    }) else {
        return;
    };
    // What `save`/`save!` answers is the call's value, as in Rails; the
    // `self` read after `save!` is for strict targets.
    let save = exprs.swap_remove(save);
    *exprs = vec![send_self("assign_attributes", vec![var(attrs)]), save];
}

/// `after_initialize unless attrs.equal?(HYDRATE_ATTRS)`, the
/// synthesized tail (`schema::synth_initialize`).
fn is_after_initialize_tail(stmt: &Expr) -> bool {
    let ExprNode::If { then_branch, .. } = &*stmt.node else { return false };
    matches!(&*then_branch.node, ExprNode::Send { recv: None, method, args, .. }
        if method.as_str() == "after_initialize" && args.is_empty())
}

/// `assign_attributes(attrs) unless attrs.equal?(ActiveRecord::Base::HYDRATE_ATTRS)`.
fn assign_attributes_unless_hydrating(attrs: &Symbol) -> Expr {
    let hydrate = Expr::new(
        Span::synthetic(),
        ExprNode::Const {
            path: vec![Symbol::from("ActiveRecord"), Symbol::from("Base"), Symbol::from("HYDRATE_ATTRS")],
        },
    );
    let hydrating = send(var(attrs), "equal?", vec![hydrate]);
    Expr::new(
        Span::synthetic(),
        ExprNode::If {
            cond: send(hydrating, "!", Vec::new()),
            then_branch: send_self("assign_attributes", vec![var(attrs)]),
            else_branch: lit(Literal::Nil),
        },
    )
}

/// The statement with the attrs it is given folded away, or None when
/// nothing of it is left.
fn defaults_only(stmt: Expr, attrs: &Symbol) -> Option<Expr> {
    let stmt = match &*stmt.node {
        // `self.<col> = attrs[:<col>] if attrs.key?(:<col>)`, with an else
        // branch for a declared default: only the else branch is left, under
        // `unless attrs.key?(:<col>)`.
        ExprNode::If { cond, else_branch, .. } if is_key_test(cond, attrs) => {
            let otherwise = fold(else_branch.clone(), attrs);
            if is_nil(&otherwise) {
                return None;
            }
            Expr::new(
                stmt.span,
                ExprNode::If { cond: send(cond.clone(), "!", Vec::new()), then_branch: otherwise, else_branch: lit(Literal::Nil) },
            )
        }
        _ => fold(stmt, attrs),
    };
    (!is_nil(&stmt)).then_some(stmt)
}

/// Fold `attrs[:k]` to nil and `attrs.key?(:k)` to false, and what they
/// decide: `nil || x`, a Cast of nil, `nil.nil?`, `!`, and an `if` on a
/// literal.
fn fold(mut expr: Expr, attrs: &Symbol) -> Expr {
    expr.node.for_each_child_mut(&mut |child| {
        let taken = std::mem::replace(child, lit(Literal::Nil));
        *child = fold(taken, attrs);
    });
    let folded = match &*expr.node {
        ExprNode::Send { recv: Some(recv), method, args, .. } if is_var(recv, attrs) => match method.as_str() {
            "[]" if args.len() == 1 => Some(lit(Literal::Nil)),
            "key?" if args.len() == 1 => Some(lit(Literal::Bool { value: false })),
            _ => None,
        },
        ExprNode::Send { recv: Some(recv), method, args, .. } if args.is_empty() && method.as_str() == "nil?" => {
            (is_nil(recv) || literal_bool(recv).is_some()).then(|| lit(Literal::Bool { value: is_nil(recv) }))
        }
        ExprNode::Send { recv: Some(recv), method, args, .. } if args.is_empty() && method.as_str() == "!" => {
            literal_bool(recv).map(|value| lit(Literal::Bool { value: !value }))
        }
        ExprNode::BoolOp { op: BoolOpKind::Or, left, right, .. } if is_nil(left) => Some(right.clone()),
        ExprNode::Cast { value, .. } if is_nil(value) => Some(lit(Literal::Nil)),
        ExprNode::If { cond, then_branch, else_branch } => {
            literal_bool(cond).map(|value| if value { then_branch.clone() } else { else_branch.clone() })
        }
        _ => None,
    };
    folded.unwrap_or(expr)
}

fn is_key_test(expr: &Expr, attrs: &Symbol) -> bool {
    matches!(&*expr.node, ExprNode::Send { recv: Some(recv), method, args, .. }
        if is_var(recv, attrs) && method.as_str() == "key?" && args.len() == 1)
}

fn is_var(expr: &Expr, name: &Symbol) -> bool {
    matches!(&*expr.node, ExprNode::Var { name: n, .. } if n == name)
}

fn is_nil(expr: &Expr) -> bool {
    matches!(&*expr.node, ExprNode::Lit { value: Literal::Nil })
}

fn literal_bool(expr: &Expr) -> Option<bool> {
    match &*expr.node {
        ExprNode::Lit { value: Literal::Bool { value } } => Some(*value),
        _ => None,
    }
}

fn lit(value: Literal) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Lit { value })
}

fn var(name: &Symbol) -> Expr {
    super::var_ref(name.clone())
}

fn send(recv: Expr, method: &str, args: Vec<Expr>) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send { recv: Some(recv), method: Symbol::from(method), parenthesized: !args.is_empty(), args, block: None },
    )
}

fn send_self(method: &str, args: Vec<Expr>) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send { recv: None, method: Symbol::from(method), args, block: None, parenthesized: true },
    )
}
