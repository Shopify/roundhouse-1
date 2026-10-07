//! Hash collections whose iteration order is the same on every run.
//!
//! `std`'s `RandomState` seeds each process differently, so anything that
//! walks a `HashMap` — a fixpoint worklist, slot updates, diagnostic
//! output — could produce a different (equally valid) answer run to run.
//! On Shopify Core the cap-12 fixpoint still moves when it stops, and
//! which facts it reached first decided hundreds of notes per run.
//! These aliases use rustc's FxHasher, which has no seed: same API
//! (construct with `default()`/`with_capacity_and_hasher`), and a
//! deterministic order for a given insertion sequence. It replaced
//! `std`'s fixed-key SipHash, which was a sixth of `check`'s analyze
//! time on Shopify Core (perf, 2026-10-07). Nothing here hashes
//! attacker-chosen keys at a scale where Fx's weaker mixing matters.

use rustc_hash::FxHasher as DefaultHasher;
use std::hash::BuildHasherDefault;

/// The fixed-key hasher behind [`HashMap`] and [`HashSet`].
pub type DetState = BuildHasherDefault<DefaultHasher>;

/// `std::collections::HashMap` with a deterministic hasher.
pub type HashMap<K, V> = std::collections::HashMap<K, V, DetState>;

/// `std::collections::HashSet` with a deterministic hasher.
pub type HashSet<T> = std::collections::HashSet<T, DetState>;
