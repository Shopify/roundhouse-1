//! How each loop of the whole-program fixpoint in
//! [`super::Analyzer::analyze`] ended.

/// One [`LoopEnd`] per loop, in the order they run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FixpointRounds {
    /// Harvest, unify and retype over production code.
    pub production: LoopEnd,
    /// Views once, then test rounds.
    pub views_and_tests: LoopEnd,
    /// Production again, when view or test call sites moved a production
    /// signature; otherwise one unchecked pass runs in its place.
    pub absorb: LoopEnd,
}

/// How one loop ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopEnd {
    /// Its signature check passed on this round, counting from 0.
    Settled(usize),
    /// It ran every round its cap allows without its check passing.
    RanToCap,
    /// It never started.
    #[default]
    NotRun,
}

impl LoopEnd {
    /// Rounds the loop ran up to and including the one whose check passed
    /// (1-based, at most [`super::fixpoint_cap`]); `Some(0)` when it never
    /// started; `None` when it ran to its cap and the last round still
    /// moved.
    pub fn rounds(self) -> Option<usize> {
        match self {
            LoopEnd::Settled(round) => Some(round + 1),
            LoopEnd::NotRun => Some(0),
            LoopEnd::RanToCap => None,
        }
    }
}

/// The `fixpoint rounds:` line `ROUNDHOUSE_TIMINGS` prints. The timing
/// span labels count differently: each loop's first round has its own
/// label (`typing passes (views after fixpoint)`, `typing passes (after
/// view unify)`), and the rest are `round 1` … `round N-1`, so with a cap
/// of 40 `round 39: absorb production` is the fortieth round.
impl std::fmt::Display for FixpointRounds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cap = super::fixpoint_cap();
        let one = |name: &str, end: LoopEnd| match end {
            LoopEnd::NotRun => format!("{name} not run"),
            LoopEnd::Settled(round) => format!("{name} settled in {} of {cap}", round + 1),
            LoopEnd::RanToCap => format!("{name} at cap {cap}, still moving"),
        };
        write!(
            f,
            "{}; {}; {}",
            one("production", self.production),
            one("tests", self.views_and_tests),
            one("absorb", self.absorb)
        )
    }
}
