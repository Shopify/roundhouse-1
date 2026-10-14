//! Shared lowering: counted Relation terminals.
//!
//! Typed `first(n)` / `last(n)` rename to `first_n` / `last_n` — one
//! method cannot carry both a record and an Array on a strict target.
//! Syntactic chains go through `scope_chain::counted_terminal`.
//!
//! `rel.count > n` → `rel.more_than?(n)` (`SELECT 1 LIMIT 1 OFFSET n`).
//! Not `offset(n).exists?`: offset mutates, and loaded exists? ignores
//! it. Only a non-negative integer literal or Const (`PAGE_SIZE`).
//! Arrays and block forms stay Enumerable.

use crate::app::App;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;
use crate::ty::Ty;

pub fn apply_relation_counted_terminals(app: &mut App) {
    super::for_each_hook_body(app, &mut |body| rewrite(body));
}

fn rewrite(e: &mut Expr) {
    e.node.for_each_child_mut(&mut |c| rewrite(c));
    rewrite_count_gt_when(e, recv_is_relation);
    if let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &mut *e.node {
        if args.len() == 1 && recv_is_relation(r) {
            let renamed = match method.as_str() {
                "first" => "first_n",
                "last" => "last_n",
                _ => return,
            };
            *method = Symbol::from(renamed);
        }
    }
}

/// Typed `Relation`, or a zero-arg `skip_preloading!` hop over an
/// untyped seed (`Page.load`'s `relation.skip_preloading!.last(size)`).
/// That method exists only on Relation in the runtime, so the hop is
/// enough proof for the rename — without it, an untyped parameter keeps
/// `.last(n)` and MRI raises arity error. (`preloaded` is Relation-only
/// too, but always arity-2 in emit/runtime, so it is not a hop here.)
fn recv_is_relation(e: &Expr) -> bool {
    if matches!(e.ty, Some(Ty::Relation { .. })) {
        return !materializes_to_array(e);
    }
    match &*e.node {
        ExprNode::Send { recv: Some(inner), method, args, block: None, .. }
            if args.is_empty() && method.as_str() == "skip_preloading!" =>
        {
            matches!(&*inner.node, ExprNode::Var { .. } | ExprNode::Ivar { .. })
                || recv_is_relation(inner)
        }
        _ => false,
    }
}

/// A call Rails answers with an Array whatever the receiver: Enumerable's
/// block methods load a relation's records and return a plain Array
/// (`relation.reject(&:test?)`, `relation.sort_by { … }`). An analyzer
/// type of Relation on such a call is the receiver's type carried
/// through, not what the call answers, and renaming the `first(2)` after
/// it to `first_n` is a NoMethodError on the Array (Core's
/// `plans.reject(&:test?).first(2)`). `select` is a builder with
/// arguments (`select(:id)`) and Enumerable only with a block.
fn materializes_to_array(e: &Expr) -> bool {
    let ExprNode::Send { recv: Some(_), method, args, block, .. } = &*e.node else { return false };
    match method.as_str() {
        "select" | "filter" => block.is_some() && args.is_empty(),
        "reject" | "sort_by" | "map" | "collect" | "flat_map" | "filter_map" | "reverse" | "compact" | "uniq"
        | "partition" | "group_by" | "take_while" | "drop_while" | "to_a" | "sort" | "shuffle" | "take" | "drop"
        | "min_by" | "max_by" => true,
        _ => false,
    }
}

/// `rel.count > n` → `rel.more_than?(n)` when `rel_ok` proves the
/// count's receiver is a Relation. Used from this pass (typed) and
/// from `scope_chain` (syntactic `__rel` / chain).
pub(crate) fn rewrite_count_gt_when(e: &mut Expr, rel_ok: impl Fn(&Expr) -> bool) -> bool {
    if !is_count_gt_shape(e, &rel_ok) {
        return false;
    }
    let parenthesized = true;
    let ExprNode::Send { recv: Some(count_e), args, .. } = &mut *e.node else {
        return false;
    };
    let threshold = args.remove(0);
    let ExprNode::Send { recv, .. } = &mut *count_e.node else {
        return false;
    };
    let Some(rel) = recv.take() else {
        return false;
    };
    *e.node = ExprNode::Send {
        recv: Some(rel),
        method: Symbol::from("more_than?"),
        args: vec![threshold],
        block: None,
        parenthesized,
    };
    e.ty = Some(Ty::Bool);
    true
}

fn is_count_gt_shape(e: &Expr, rel_ok: &impl Fn(&Expr) -> bool) -> bool {
    let ExprNode::Send { recv: Some(count_e), method, args, block: None, .. } = &*e.node else {
        return false;
    };
    if method.as_str() != ">" || args.len() != 1 || !is_count_threshold(&args[0]) {
        return false;
    }
    let ExprNode::Send {
        recv: Some(rel),
        method: count_m,
        args: count_args,
        block: None,
        ..
    } = &*count_e.node
    else {
        return false;
    };
    count_m.as_str() == "count" && count_args.is_empty() && rel_ok(rel)
}

fn is_count_threshold(e: &Expr) -> bool {
    match &*e.node {
        ExprNode::Lit { value: Literal::Int { value } } => *value >= 0,
        ExprNode::Const { .. } => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;

    fn send(recv: Option<Expr>, method: &str, args: Vec<Expr>, block: Option<Expr>, ty: Option<Ty>) -> Expr {
        let mut e = Expr::new(
            Span::synthetic(),
            ExprNode::Send { recv, method: Symbol::from(method), args, block, parenthesized: true },
        );
        e.ty = ty;
        e
    }

    fn relation() -> Ty {
        Ty::Relation { of: crate::ident::ClassId(Symbol::from("Plan")) }
    }

    fn two() -> Expr {
        Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Int { value: 2 } })
    }

    fn block() -> Expr {
        Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil })
    }

    fn base() -> Expr {
        let mut var = Expr::new(
            Span::synthetic(),
            ExprNode::Var { id: crate::ident::VarId(0), name: Symbol::from("plans") },
        );
        var.ty = Some(relation());
        var
    }

    fn first_two_after(hop: Expr) -> Expr {
        send(Some(hop), "first", vec![two()], None, None)
    }

    fn method_of(e: &Expr) -> &str {
        let ExprNode::Send { method, .. } = &*e.node else { panic!() };
        method.as_str()
    }

    #[test]
    fn first_n_on_a_relation_is_renamed() {
        let mut e = first_two_after(base());
        rewrite(&mut e);
        assert_eq!(method_of(&e), "first_n");
    }

    /// Enumerable's `reject` on a relation answers an Array, whatever the
    /// analyzer typed the call: its `first(2)` is `Array#first`.
    #[test]
    fn first_after_an_enumerable_reject_stays_array_first() {
        let reject = send(Some(base()), "reject", Vec::new(), Some(block()), Some(relation()));
        let mut e = first_two_after(reject);
        rewrite(&mut e);
        assert_eq!(method_of(&e), "first");
    }

    #[test]
    fn last_after_a_block_select_stays_array_last() {
        let select = send(Some(base()), "select", Vec::new(), Some(block()), Some(relation()));
        let mut e = send(Some(select), "last", vec![two()], None, None);
        rewrite(&mut e);
        assert_eq!(method_of(&e), "last");
    }

    /// `select(:id)` is a builder: still a relation.
    #[test]
    fn first_after_a_column_select_is_renamed() {
        let id = Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Sym { value: Symbol::from("id") } });
        let select = send(Some(base()), "select", vec![id], None, Some(relation()));
        let mut e = first_two_after(select);
        rewrite(&mut e);
        assert_eq!(method_of(&e), "first_n");
    }
}
