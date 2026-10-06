//! Recursive methods and the widening their returns need.
//!
//! A structurally recursive method — an AST printer, a deep
//! `transform_values`, a JSON sanitizer — has a recursive return type:
//!
//! ```ruby
//! def fix(value)
//!   case value
//!   when Hash then value.to_h { |k, v| [k, fix(v)] }
//!   when Array then value.map { |v| fix(v) }
//!   else value
//!   end
//! end
//! ```
//!
//! returns `μX. Hash[K, X] | Array[X] | T`. The type lattice has no
//! μ-types, so Kleene iteration over it never reaches that answer: each
//! fixpoint round types the body against the return harvested the round
//! before and comes back one container deeper, `Hash[K, Hash[K, …]]`.
//! The chain is infinite; the analyzer's convergence check never holds,
//! every round re-types the whole app with larger types, and where a
//! method is recursive in both key and value (or through two paths) the
//! size doubles per round. Every call site downstream inherits the type
//! too, so caller params grow in step.
//!
//! The fix is the textbook one for an infinite-height lattice: widen at
//! the join. When a method that can reach itself through implicit-self
//! calls harvests a return that nests its previous return inside a
//! container, the nested occurrence is the recursion point, and the
//! only finite sound description of "the same shape again, to any
//! depth" is the gradual `Untyped`. One unfolding of the shape stays
//! precise; the next round reproduces the widened type exactly, so the
//! fixpoint holds. Finite nesting (a method building a `Hash[Sym,
//! Hash[Sym, Str]]` without calling itself) is untouched: the widening
//! needs both the call cycle and the self-embedding.

use std::collections::{HashMap, HashSet};

use crate::dialect::{MethodDef, MethodReceiver};
use crate::expr::{Expr, ExprNode};
use crate::ident::Symbol;
use crate::ty::{Row, Ty};

/// `(is_class_method, name)` of every method in `methods` that reaches
/// itself through implicit-self (or `self.`) calls within the same
/// class: direct recursion and mutual recursion between siblings.
pub(super) fn recursive_methods<'a>(
    methods: impl IntoIterator<Item = &'a MethodDef>,
) -> HashSet<(bool, Symbol)> {
    let methods: Vec<&MethodDef> = methods.into_iter().collect();
    let key = |m: &MethodDef| (m.receiver == MethodReceiver::Class, m.name.clone());
    let index: HashMap<(bool, Symbol), usize> =
        methods.iter().enumerate().map(|(i, m)| (key(m), i)).collect();
    let edges: Vec<Vec<usize>> = methods
        .iter()
        .map(|m| {
            let is_class = m.receiver == MethodReceiver::Class;
            let mut out = Vec::new();
            collect_self_calls(&m.body, &mut |name| {
                if let Some(&j) = index.get(&(is_class, name.clone())) {
                    out.push(j);
                }
            });
            out.sort_unstable();
            out.dedup();
            out
        })
        .collect();
    let mut recursive = HashSet::new();
    for (start, m) in methods.iter().enumerate() {
        // Reachability back to `start`; classes have few methods, and
        // this runs once per analysis, not per round.
        let mut seen = vec![false; methods.len()];
        let mut stack: Vec<usize> = edges[start].clone();
        while let Some(i) = stack.pop() {
            if i == start {
                recursive.insert(key(m));
                break;
            }
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            stack.extend(edges[i].iter().copied());
        }
    }
    recursive
}

fn collect_self_calls(e: &Expr, f: &mut dyn FnMut(&Symbol)) {
    match &*e.node {
        ExprNode::Send { recv, method, .. } | ExprNode::MethodRef { recv, name: method }
            if recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)) =>
        {
            f(method)
        }
        _ => {}
    }
    e.node.for_each_child(&mut |c| collect_self_calls(c, f));
}

/// Widen a recursive method's freshly harvested return `new` against
/// the return it had last round, `old`: every subterm of `new` strictly
/// inside a container that already covers `old` is the recursion
/// point, and becomes `Untyped`. Returns `new` unchanged when `old`
/// carries no container (scalar recursion — `fact(n - 1) * n` — joins
/// into a finite type on its own) or nothing nests.
pub(super) fn widen_recursive_return(old: &Ty, new: Ty) -> Ty {
    if matches!(old, Ty::Var { .. } | Ty::Untyped | Ty::Bottom) || !has_container(old) {
        return new;
    }
    cut(new, old, false)
}

fn has_container(t: &Ty) -> bool {
    match t {
        Ty::Array { .. } | Ty::Hash { .. } | Ty::Tuple { .. } | Ty::Record { .. } => true,
        Ty::Union { variants } => variants.iter().any(has_container),
        Ty::Class { args, .. } => !args.is_empty(),
        _ => false,
    }
}

fn cut(t: Ty, old: &Ty, nested: bool) -> Ty {
    if nested && covers(&t, old) {
        return Ty::Untyped;
    }
    match t {
        Ty::Array { elem } => Ty::Array { elem: Box::new(cut(*elem, old, true)) },
        Ty::Hash { key, value } => Ty::Hash {
            key: Box::new(cut(*key, old, true)),
            value: Box::new(cut(*value, old, true)),
        },
        Ty::Tuple { elems } => {
            Ty::Tuple { elems: elems.into_iter().map(|e| cut(e, old, true)).collect() }
        }
        Ty::Record { row } => Ty::Record {
            row: Row {
                fields: row.fields.into_iter().map(|(k, v)| (k, cut(v, old, true))).collect(),
                rest: row.rest,
            },
        },
        Ty::Class { id, args } => {
            Ty::Class { id, args: args.into_iter().map(|a| cut(a, old, true)).collect() }
        }
        // A union is not a level of nesting: its variants sit where it
        // does. Re-joined so the result stays canonical.
        Ty::Union { variants } => variants
            .into_iter()
            .map(|v| cut(v, old, nested))
            .reduce(super::body::union_of)
            .unwrap_or(Ty::Bottom),
        other => other,
    }
}

/// Structural "`sup` describes every value `sub` does". `Untyped`
/// covers anything; an unresolved `Var` in `sub` constrains nothing.
fn covers(sup: &Ty, sub: &Ty) -> bool {
    if sup == sub {
        return true;
    }
    match (sup, sub) {
        (Ty::Untyped, _) => true,
        (_, Ty::Var { .. } | Ty::Bottom) => true,
        (_, Ty::Union { variants }) => variants.iter().all(|v| covers(sup, v)),
        (Ty::Union { variants }, _) => variants.iter().any(|w| covers(w, sub)),
        (Ty::Array { elem: a }, Ty::Array { elem: b }) => covers(a, b),
        (Ty::Hash { key: k1, value: v1 }, Ty::Hash { key: k2, value: v2 }) => {
            covers(k1, k2) && covers(v1, v2)
        }
        (Ty::Tuple { elems: a }, Ty::Tuple { elems: b }) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| covers(x, y))
        }
        (Ty::Record { row: a }, Ty::Record { row: b }) => b
            .fields
            .iter()
            .all(|(k, tb)| a.fields.get(k).is_some_and(|ta| covers(ta, tb))),
        (Ty::Class { id: a, args: xs }, Ty::Class { id: b, args: ys }) => {
            a == b && xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| covers(x, y))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ident::TyVar;

    fn hash(k: Ty, v: Ty) -> Ty {
        Ty::Hash { key: Box::new(k), value: Box::new(v) }
    }
    fn union(vs: Vec<Ty>) -> Ty {
        vs.into_iter().reduce(super::super::body::union_of).unwrap()
    }

    #[test]
    fn nested_previous_return_becomes_the_recursion_point() {
        // fix_bad_encodings: Hash[_, X] | X-of-last-round | untyped
        let var = Ty::Var { var: TyVar(0) };
        let old = union(vec![hash(var.clone(), Ty::Untyped), var.clone(), Ty::Untyped]);
        let new = union(vec![hash(var.clone(), old.clone()), var.clone(), Ty::Untyped]);
        let widened = widen_recursive_return(&old, new);
        assert_eq!(widened, old);
    }

    #[test]
    fn widening_is_stable_under_one_more_unfolding() {
        // normalize: Hash[Sym, Str | X]
        let f = |x: Ty| hash(Ty::Sym, union(vec![Ty::Str, x]));
        let mut ret = f(hash(Ty::Sym, Ty::Str));
        for _ in 0..4 {
            ret = widen_recursive_return(&ret.clone(), f(ret));
        }
        assert_eq!(widen_recursive_return(&ret, f(ret.clone())), ret);
        assert!(super::super::fixpoint_debug::ty_size(&ret) <= 4, "{ret:?}");
    }

    #[test]
    fn finite_nesting_and_scalars_are_untouched() {
        let old = hash(Ty::Sym, Ty::Str);
        let new = hash(Ty::Sym, union(vec![Ty::Int, Ty::Str]));
        assert_eq!(widen_recursive_return(&old, new.clone()), new);
        let new = Ty::Array { elem: Box::new(Ty::Int) };
        assert_eq!(widen_recursive_return(&Ty::Int, new.clone()), new);
    }
}
