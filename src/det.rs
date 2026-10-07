//! Hash collections whose iteration order is the same on every run.
//!
//! `std`'s `RandomState` seeds each process differently, so anything that
//! walks a `HashMap` — a fixpoint worklist, slot updates, diagnostic
//! output — could produce a different (equally valid) answer run to run.
//! On Shopify Core the cap-12 fixpoint still moves when it stops, and
//! which facts it reached first decided hundreds of notes per run.
//! These aliases keep `std`'s SipHash but with fixed keys: same speed,
//! same API (construct with `default()`/`with_capacity_and_hasher`), and
//! a deterministic order for a given insertion sequence.

use std::collections::hash_map::DefaultHasher;
use std::hash::BuildHasherDefault;

/// The fixed-key hasher behind [`HashMap`] and [`HashSet`].
pub type DetState = BuildHasherDefault<DefaultHasher>;

/// `std::collections::HashMap` with a deterministic hasher.
pub type HashMap<K, V> = std::collections::HashMap<K, V, DetState>;

/// `std::collections::HashSet` with a deterministic hasher.
pub type HashSet<T> = std::collections::HashSet<T, DetState>;
