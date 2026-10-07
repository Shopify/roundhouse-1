//! Widening for signature entries that cycle between fixpoint rounds.
//!
//! Each whole-program round recomputes method returns and call-site
//! parameter types from scratch, from bodies typed against the previous
//! round's registry. That step is not monotone. A slot whose evidence
//! feeds back into itself through a "no information" rule can flip
//! between states forever. `unify_param_ty` ignores a top-level `Var`
//! or `Untyped` argument but keeps `Var | Nil`, and a reader typed
//! `Nil` makes `x || Unknown.call` collapse to evidence-free `Untyped`.
//! An example is `attr_accessor :ctx` written from
//! `ctx || Context.unknown` in one method and `= nil` in another: the
//! writer sees `Nil` one round and `Var | Untyped | Nil` the next, and
//! the reader, its callers and their params follow, with period 2 or 3.
//! On Shopify Core, about a hundred such slots kept every loop running
//! to the cap.
//!
//! An iteration over a lattice that is not monotone has no least
//! fixpoint to reach. The standard repair is to widen the iterate by
//! joining. Once a slot returns to a value it held in an earlier round
//! of the same loop, it has shown a cycle. From then on the slot holds
//! the join of every value in that cycle (the values since it last
//! held this one, not the approach before) and of every later
//! recomputation. Each value is what some round's evidence said the
//! slot can be, so the join is the sound description of the slot. The
//! slot is now monotone, and its dependents see one fixed input and
//! settle. Slots that only move forward never revisit a value, so a
//! long propagation chain (lobsters' nine-round pagination chain) is
//! untouched.
//!
//! Joining cycles does not stop a slot that grows without repeating.
//! A method that returns its own result one container deeper each
//! round (`{ key => sub_set.serialize! }` on another instance of the
//! same class, which the self-call recursion widening does not see),
//! or a parameter fed back its own value wrapped in a Hash, gains one
//! nesting level per round forever; on Shopify Core thousands of such
//! slots kept the production and absorb loops moving at the cap, and
//! each round typed larger trees than the last. Some gain the level in
//! one round; others take a few rounds per level, the innermost
//! element passing through `Var`, `Array[Var]` and a union on the way.
//! The lattice has no finite height along that chain, so it needs the
//! other standard widening, k-limiting: once a slot has grown (more
//! type nodes than the round before) [`GROWTH_ROUNDS`] rounds running
//! and is nested deeper than the larger of [`DEPTH_FLOOR`] and the
//! depth it started the loop at, every container below that depth
//! becomes the gradual `Untyped`, now and in each later round. The
//! levels above the limit keep their precise types. A slot that stops
//! growing, or grows only within the limit, is untouched.

use std::collections::HashMap;

use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;

use super::body::union_of;
use super::fixpoint_debug::ty_size;
use super::{Analyzer, InferenceSig};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Slot {
    Instance(ClassId, Symbol),
    Class(ClassId, Symbol),
    /// The whole parameter vector, held as a `Ty::Tuple` and joined
    /// position by position.
    Params(ClassId, Symbol),
}

/// Values a slot took in earlier rounds of the same loop. Bounded: a
/// cycle with a longer period than this is still caught, because the
/// cap is only reached after the period has repeated.
const VISITED_PER_SLOT: usize = 6;

/// Consecutive rounds a slot must grow before it is depth-limited.
const GROWTH_ROUNDS: usize = 3;

/// The least nesting depth a limited slot keeps precise.
const DEPTH_FLOOR: usize = 4;

/// One slot's growth over a loop.
#[derive(Clone, Copy)]
struct Growth {
    /// Container depth when the slot first changed in the loop.
    start_depth: usize,
    /// The last round it changed in.
    last_round: usize,
    /// Consecutive rounds, up to `last_round`, in which it grew.
    streak: usize,
}

/// One whole-program loop's cycle state. Start a fresh one per loop:
/// the production, test and absorb loops recompute different evidence,
/// so returning to a production-round value in the absorb loop is not a
/// cycle.
#[derive(Default)]
pub(super) struct CycleWidening {
    visited: HashMap<Slot, Vec<Ty>>,
    widened: HashMap<Slot, Ty>,
    growth: HashMap<Slot, Growth>,
    /// `widen_cycles` calls so far in this loop.
    round: usize,
    /// Depth-limited slots and the depth each keeps. Carried into the
    /// next loop (see [`Self::next_loop`]): a slot that grew without
    /// bound under one loop's evidence does under the next one's too.
    limited: HashMap<Slot, usize>,
    /// What the last `widen_cycles` call did, for the debug report.
    last: WidenRound,
}

/// Per-round counts of what widening did to the held slots.
#[derive(Default, Clone, Copy)]
struct WidenRound {
    /// Held slot recomputed to a different value inside its join: written
    /// back to the join, so it is no change.
    held: usize,
    /// Held slot recomputed outside its join: the join grew.
    grew: usize,
    /// Held slot whose new value cannot be joined (`Fn`, arity change):
    /// it keeps the recomputed value.
    unjoinable: usize,
    /// Slots widened for the first time this round.
    newly: usize,
    /// Slots depth-limited for the first time this round.
    newly_limited: usize,
    /// Limited slots whose recomputed value was cut back this round.
    cut: usize,
}

impl CycleWidening {
    /// The state the next loop starts from: no cycle history, the same
    /// depth limits.
    pub(super) fn next_loop(&self) -> Self {
        Self { limited: self.limited.clone(), ..Self::default() }
    }

    /// Debug summary of the last round: what widening did to the held
    /// slots, and how many of the round's changed slots `changed` are held
    /// slots (their join grew or could not be joined) versus slots that
    /// were never widened.
    pub(super) fn round_summary(&self, analyzer: &Analyzer, prev: &InferenceSig) -> String {
        let changed = analyzer.changed_slots(prev);
        let in_held = changed.iter().filter(|(s, ..)| self.widened.contains_key(s)).count();
        let WidenRound { held, grew, unjoinable, newly, newly_limited, cut } = self.last;
        format!(
            "widened {}: held-equal {held}, held-grew {grew}, unjoinable {unjoinable}, newly {newly}; depth-limited {}: newly {newly_limited}, cut {cut}; changed {} = {in_held} held + {} unwidened",
            self.widened.len(),
            self.limited.len(),
            changed.len(),
            changed.len() - in_held,
        )
    }
}

/// Container nesting depth: each Array, Hash, Tuple, Record or class
/// with type arguments is a level; a union is not. A parameter vector
/// is not a level either: its depth is its deepest parameter's.
fn slot_depth(slot: &Slot, t: &Ty) -> usize {
    match (slot, t) {
        (Slot::Params(..), Ty::Tuple { elems }) => elems.iter().map(depth).max().unwrap_or(0),
        _ => depth(t),
    }
}

fn depth(t: &Ty) -> usize {
    match t {
        Ty::Array { elem } => 1 + depth(elem),
        Ty::Hash { key, value } => 1 + depth(key).max(depth(value)),
        Ty::Tuple { elems } => 1 + elems.iter().map(depth).max().unwrap_or(0),
        Ty::Record { row } => 1 + row.fields.values().map(depth).max().unwrap_or(0),
        Ty::Class { args, .. } if !args.is_empty() => 1 + args.iter().map(depth).max().unwrap_or(0),
        Ty::Union { variants } => variants.iter().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// `t` with every container below the first `keep` levels replaced by
/// `Untyped`.
fn limit_slot(slot: &Slot, t: Ty, keep: usize) -> Ty {
    match (slot, t) {
        (Slot::Params(..), Ty::Tuple { elems }) => {
            Ty::Tuple { elems: elems.into_iter().map(|e| limit(e, keep)).collect() }
        }
        (_, t) => limit(t, keep),
    }
}

fn limit(t: Ty, keep: usize) -> Ty {
    let is_container = match &t {
        Ty::Array { .. } | Ty::Hash { .. } | Ty::Tuple { .. } | Ty::Record { .. } => true,
        Ty::Class { args, .. } => !args.is_empty(),
        _ => false,
    };
    if is_container && keep == 0 {
        return Ty::Untyped;
    }
    let below = keep.saturating_sub(1);
    match t {
        Ty::Array { elem } => Ty::Array { elem: Box::new(limit(*elem, below)) },
        Ty::Hash { key, value } => Ty::Hash {
            key: Box::new(limit(*key, below)),
            value: Box::new(limit(*value, below)),
        },
        Ty::Tuple { elems } => Ty::Tuple { elems: elems.into_iter().map(|e| limit(e, below)).collect() },
        Ty::Record { row } => Ty::Record {
            row: crate::ty::Row {
                fields: row.fields.into_iter().map(|(k, v)| (k, limit(v, below))).collect(),
                rest: row.rest,
            },
        },
        Ty::Class { id, args } => Ty::Class { id, args: args.into_iter().map(|a| limit(a, below)).collect() },
        // Re-joined so two variants cut to the same shape merge.
        Ty::Union { variants } => variants
            .into_iter()
            .map(|v| limit(v, keep))
            .reduce(union_of)
            .unwrap_or(Ty::Bottom),
        other => other,
    }
}

fn join(slot: &Slot, a: Ty, b: Ty) -> Option<Ty> {
    if matches!(a, Ty::Fn { .. }) || matches!(b, Ty::Fn { .. }) {
        // Declared signatures never move; nothing to widen.
        return None;
    }
    match slot {
        Slot::Params(..) => {
            let (Ty::Tuple { elems: xs }, Ty::Tuple { elems: ys }) = (a, b) else { return None };
            (xs.len() == ys.len()).then(|| Ty::Tuple {
                elems: xs.into_iter().zip(ys).map(|(x, y)| union_of(x, y)).collect(),
            })
        }
        Slot::Instance(..) | Slot::Class(..) => Some(union_of(a, b)),
    }
}

impl Analyzer {
    fn slot_ty(&self, slot: &Slot) -> Option<Ty> {
        match slot {
            Slot::Instance(c, m) => self.classes.get(c)?.instance_methods.get(m).cloned(),
            Slot::Class(c, m) => self.classes.get(c)?.class_methods.get(m).cloned(),
            Slot::Params(c, m) => self
                .inferred_params
                .get(&(c.clone(), m.clone()))
                .map(|v| Ty::Tuple { elems: v.clone() }),
        }
    }

    fn set_slot(&mut self, slot: &Slot, ty: Ty) {
        match slot {
            Slot::Instance(c, m) => {
                if let Some(cls) = self.classes.get_mut(c) {
                    cls.instance_methods.insert(m.clone(), ty);
                }
            }
            Slot::Class(c, m) => {
                if let Some(cls) = self.classes.get_mut(c) {
                    cls.class_methods.insert(m.clone(), ty);
                }
            }
            Slot::Params(c, m) => {
                if let Ty::Tuple { elems } = ty {
                    self.inferred_params.insert((c.clone(), m.clone()), elems);
                }
            }
        }
    }

    /// Slots whose value differs from `prev`: `(slot, before, now)`.
    fn changed_slots(&self, prev: &InferenceSig) -> Vec<(Slot, Option<Ty>, Ty)> {
        let mut out = Vec::new();
        for (id, cls) in &self.classes {
            for (is_class, now, before) in [
                (false, &cls.instance_methods, prev.instance.get(id)),
                (true, &cls.class_methods, prev.class_methods.get(id)),
            ] {
                if before == Some(now) {
                    continue;
                }
                for (m, t) in now {
                    let b = before.and_then(|p| p.get(m));
                    if b != Some(t) {
                        let slot = if is_class {
                            Slot::Class(id.clone(), m.clone())
                        } else {
                            Slot::Instance(id.clone(), m.clone())
                        };
                        out.push((slot, b.cloned(), t.clone()));
                    }
                }
            }
        }
        for ((c, m), tys) in &self.inferred_params {
            let b = prev.params.get(&(c.clone(), m.clone()));
            if b != Some(tys) {
                out.push((
                    Slot::Params(c.clone(), m.clone()),
                    b.map(|v| Ty::Tuple { elems: v.clone() }),
                    Ty::Tuple { elems: tys.clone() },
                ));
            }
        }
        out
    }

    /// Run after a round's harvest and unify, before its convergence
    /// check against `prev` (the signature the round's typing passes
    /// read). Re-joins every slot already widened in this loop, then
    /// widens each slot that has just returned to an earlier value,
    /// then depth-limits slots that keep nesting deeper.
    pub(super) fn widen_cycles(&mut self, prev: &InferenceSig, state: &mut CycleWidening) {
        let mut round = WidenRound::default();
        let widened: Vec<(Slot, Ty)> =
            state.widened.iter().map(|(s, t)| (s.clone(), t.clone())).collect();
        for (slot, acc) in widened {
            let Some(now) = self.slot_ty(&slot) else { continue };
            if now == acc {
                continue;
            }
            if let Some(joined) = join(&slot, acc.clone(), now) {
                if joined == acc {
                    round.held += 1;
                } else {
                    round.grew += 1;
                }
                self.set_slot(&slot, joined.clone());
                state.widened.insert(slot, joined);
            } else {
                round.unjoinable += 1;
            }
        }
        for (slot, before, now) in self.changed_slots(prev) {
            if !state.limited.contains_key(&slot) {
                let this_round = state.round;
                let growth = state.growth.entry(slot.clone()).or_insert_with(|| Growth {
                    start_depth: before.as_ref().map_or(0, |b| slot_depth(&slot, b)),
                    last_round: usize::MAX,
                    streak: 0,
                });
                let grew = before.as_ref().is_some_and(|b| ty_size(&now) > ty_size(b));
                growth.streak = match (grew, growth.last_round.wrapping_add(1) == this_round) {
                    (false, _) => 0,
                    (true, true) => growth.streak + 1,
                    (true, false) => 1,
                };
                growth.last_round = this_round;
                let keep = growth.start_depth.max(DEPTH_FLOOR);
                if growth.streak >= GROWTH_ROUNDS && slot_depth(&slot, &now) > keep {
                    state.limited.insert(slot.clone(), keep);
                    round.newly_limited += 1;
                }
            }
            if state.widened.contains_key(&slot) {
                continue;
            }
            let visited = state.visited.entry(slot.clone()).or_default();
            if visited.is_empty() {
                visited.extend(before);
            }
            let Some(start) = visited.iter().position(|v| v == &now) else {
                if visited.len() == VISITED_PER_SLOT {
                    visited.remove(0);
                }
                visited.push(now);
                continue;
            };
            // The cycle is what the slot took since it last held `now`;
            // values before that were the approach, not the cycle.
            let joined = visited[start + 1..]
                .iter()
                .cloned()
                .try_fold(now, |acc, v| join(&slot, acc, v));
            state.visited.remove(&slot);
            if let Some(joined) = joined {
                self.set_slot(&slot, joined.clone());
                state.widened.insert(slot, joined);
                round.newly += 1;
            }
        }
        round.cut = self.limit_depths(state);
        state.last = round;
        state.round += 1;
    }

    /// Cut every depth-limited slot back to its limit; the number cut.
    /// `widen_cycles` does this each round; call it after any later
    /// harvest that recomputes returns from the bodies, which nest one
    /// level past the limit again.
    pub(super) fn limit_depths(&mut self, state: &mut CycleWidening) -> usize {
        let mut cut_count = 0;
        for (slot, &keep) in &state.limited {
            let Some(now) = self.slot_ty(slot) else { continue };
            if slot_depth(slot, &now) <= keep {
                continue;
            }
            let cut = limit_slot(slot, now, keep);
            self.set_slot(slot, cut.clone());
            if let Some(acc) = state.widened.get_mut(slot) {
                *acc = cut;
            }
            cut_count += 1;
        }
        cut_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ident::TyVar;

    #[test]
    fn params_join_position_by_position() {
        let slot = Slot::Params(ClassId(Symbol::from("A")), Symbol::from("m"));
        let var = Ty::Var { var: TyVar(0) };
        let a = Ty::Tuple { elems: vec![Ty::Nil, Ty::Int] };
        let b = Ty::Tuple { elems: vec![union_of(var.clone(), Ty::Nil), Ty::Int] };
        let j = join(&slot, a, b.clone()).unwrap();
        assert_eq!(j, b);
        let short = Ty::Tuple { elems: vec![Ty::Nil] };
        assert_eq!(join(&slot, short, j), None);
    }
}
