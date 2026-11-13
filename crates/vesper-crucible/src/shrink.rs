//! The shortlex choice-tape reducer (§29.5).
//!
//! The reducer never looks at the values a generator builds. It proposes
//! smaller tapes (span deletion, span zeroing, per-value binary-search
//! lowering, and "lower a count, delete what follows"), re-runs the
//! unmodified property on each, and accepts a candidate only if
//!
//! 1. the run fails with the same failure kind,
//! 2. the run was valid: every draw stayed inside the tape and no `assume`
//!    rejected it, so the regenerated values are ones the generator itself
//!    produces (a protocol generator that only emits legal begin/commit
//!    sequences keeps emitting legal ones), and
//! 3. the realized tape is strictly smaller in the `(len, sum)` order.
//!
//! Rule 3 is checked at run time before a candidate is accepted, so every
//! accepted step decreases a well-founded order and shrinking terminates
//! (`lean/CrucibleShrink.lean`: `shrink` is accepted by well-founded recursion
//! and `shrink_preserves` keeps the invariant; the proof is about the
//! Lean model of this loop, not about this Rust code). The result is locally
//! minimal with respect to these passes only; no global minimality is
//! claimed.

use crate::exec::{Run, Verdict, execute};
use crate::source::{DEFAULT_MAX_DRAWS, Failure, PropError, Source};
use crate::tape::{ChoiceTape, ShrinkKey};
use std::fmt;

/// Shrinker budget and options.
#[derive(Clone, Debug)]
pub struct ShrinkConfig {
    /// Property executions the shrinker may spend.
    pub max_runs: u64,
    /// Per-run draw budget.
    pub max_draws: usize,
    /// Treat a panicking property as a failure of kind `"panic"`.
    pub catch_panics: bool,
}

impl Default for ShrinkConfig {
    fn default() -> Self {
        Self {
            max_runs: 20_000,
            max_draws: DEFAULT_MAX_DRAWS,
            catch_panics: true,
        }
    }
}

/// What the shrinker did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShrinkStats {
    pub runs: u64,
    pub accepted: u64,
    /// Candidates that ran but were invalid (overrun or rejected).
    pub invalid: u64,
    /// Candidates that passed or failed differently.
    pub not_reproducing: u64,
    pub initial: ShrinkKey,
    pub shrunk: ShrinkKey,
    pub budget_exhausted: bool,
}

/// A shrunk counterexample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shrunk {
    pub tape: ChoiceTape,
    pub failure: Failure,
    pub stats: ShrinkStats,
}

/// The tape given to [`shrink`] does not reproduce a failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoesNotFail(pub Verdict);

impl fmt::Display for DoesNotFail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tape does not reproduce a failure: {:?}", self.0)
    }
}

impl std::error::Error for DoesNotFail {}

struct Reducer<'p, P> {
    prop: &'p mut P,
    config: ShrinkConfig,
    kind: String,
    best: Vec<u64>,
    failure: Failure,
    stats: ShrinkStats,
}

impl<P> Reducer<'_, P>
where
    P: FnMut(&mut Source) -> Result<(), PropError>,
{
    fn run(&mut self, values: Vec<u64>) -> Run {
        let src = Source::build_strict(values, self.config.max_draws);
        execute(self.prop, src, self.config.catch_panics)
    }

    fn out_of_budget(&mut self) -> bool {
        if self.stats.runs >= self.config.max_runs {
            self.stats.budget_exhausted = true;
        }
        self.stats.budget_exhausted
    }

    /// Run `candidate`; accept its realized tape if it still fails the same
    /// way and is strictly smaller than the current best.
    fn attempt(&mut self, candidate: Vec<u64>) -> bool {
        let best_key = ShrinkKey::of(&self.best);
        if !ShrinkKey::of(&candidate).smaller_than(best_key) || self.out_of_budget() {
            return false;
        }
        self.stats.runs += 1;
        let run = self.run(candidate);
        match run.verdict {
            Verdict::Fail(failure) if failure.kind == self.kind => {
                // Realized <= candidate pointwise and in length (clamping and
                // an unconsumed tail only lower it), but check, never assume.
                if ShrinkKey::of(&run.values).smaller_than(best_key) {
                    self.best = run.values;
                    self.failure = failure;
                    self.stats.accepted += 1;
                    true
                } else {
                    false
                }
            }
            Verdict::Invalid(_) => {
                self.stats.invalid += 1;
                false
            }
            _ => {
                self.stats.not_reproducing += 1;
                false
            }
        }
    }

    fn delete_spans(&mut self) -> bool {
        let mut improved = false;
        let mut chunk = self.best.len() / 2;
        while chunk > 0 {
            let mut idx = 0;
            while idx + chunk <= self.best.len() && !self.out_of_budget() {
                let mut candidate = self.best.clone();
                candidate.drain(idx..idx + chunk);
                if self.attempt(candidate) {
                    improved = true;
                } else {
                    idx += 1;
                }
            }
            chunk /= 2;
        }
        improved
    }

    fn zero_spans(&mut self) -> bool {
        let mut improved = false;
        for width in [8usize, 4, 2] {
            let mut idx = 0;
            while idx + width <= self.best.len() && !self.out_of_budget() {
                if self.best[idx..idx + width].iter().any(|&v| v != 0) {
                    let mut candidate = self.best.clone();
                    candidate[idx..idx + width].fill(0);
                    improved |= self.attempt(candidate);
                }
                idx += 1;
            }
        }
        improved
    }

    fn lower_values(&mut self) -> bool {
        let mut improved = false;
        let mut i = 0;
        while i < self.best.len() && !self.out_of_budget() {
            // Small values first: the binary search below assumes failure is
            // monotone in the value, which properties need not be.
            for small in 0..self.best[i].min(8) {
                if i >= self.best.len() || small >= self.best[i] {
                    break;
                }
                let mut candidate = self.best.clone();
                candidate[i] = small;
                if self.attempt(candidate) {
                    improved = true;
                    break;
                }
            }
            if i >= self.best.len() {
                break;
            }
            let (mut lo, mut hi) = (0u64, self.best[i]);
            while lo < hi && i < self.best.len() && !self.out_of_budget() {
                let mid = lo + (hi - lo) / 2;
                let mut candidate = self.best.clone();
                candidate[i] = mid;
                if self.attempt(candidate) {
                    improved = true;
                    hi = self.best.get(i).copied().unwrap_or(0).min(mid);
                } else {
                    lo = mid + 1;
                }
            }
            i += 1;
        }
        improved
    }

    /// Lower a value by one and delete up to eight following choices: shrinks
    /// count-prefixed structures (a drawn count followed by that many
    /// elements) where neither step alone still fails.
    fn lower_and_delete(&mut self) -> bool {
        let mut improved = false;
        let mut i = 0;
        while i < self.best.len() && !self.out_of_budget() {
            if self.best[i] > 0 {
                for k in 1..=8usize {
                    if i + 1 + k > self.best.len() || self.out_of_budget() {
                        break;
                    }
                    let mut candidate = self.best.clone();
                    candidate[i] -= 1;
                    candidate.drain(i + 1..i + 1 + k);
                    if self.attempt(candidate) {
                        improved = true;
                        break;
                    }
                }
            }
            i += 1;
        }
        improved
    }
}

/// Shrink a failing tape. `tape` is replayed strictly and must fail; the
/// shrinker then only accepts candidates failing with that failure's kind.
pub fn shrink<P>(
    prop: &mut P,
    tape: &ChoiceTape,
    config: &ShrinkConfig,
) -> Result<Shrunk, DoesNotFail>
where
    P: FnMut(&mut Source) -> Result<(), PropError>,
{
    let first = execute(
        prop,
        Source::build_strict(tape.values.clone(), config.max_draws),
        config.catch_panics,
    );
    let failure = match first.verdict {
        Verdict::Fail(f) => f,
        other => return Err(DoesNotFail(other)),
    };
    let initial = ShrinkKey::of(&first.values);
    let mut reducer = Reducer {
        prop,
        config: config.clone(),
        kind: failure.kind.clone(),
        best: first.values,
        failure,
        stats: ShrinkStats {
            runs: 1,
            accepted: 0,
            invalid: 0,
            not_reproducing: 0,
            initial,
            shrunk: initial,
            budget_exhausted: false,
        },
    };
    loop {
        let mut improved = reducer.delete_spans();
        improved |= reducer.zero_spans();
        improved |= reducer.lower_values();
        improved |= reducer.lower_and_delete();
        if !improved || reducer.out_of_budget() {
            break;
        }
    }
    reducer.stats.shrunk = ShrinkKey::of(&reducer.best);
    Ok(Shrunk {
        tape: ChoiceTape::new(reducer.best),
        failure: reducer.failure,
        stats: reducer.stats,
    })
}

/// Replay `tape` strictly and return the verdict: deterministic reproduction
/// of a saved counterexample.
pub fn replay<P>(prop: &mut P, tape: &ChoiceTape) -> Verdict
where
    P: FnMut(&mut Source) -> Result<(), PropError>,
{
    execute(prop, Source::replay(tape), true).verdict
}
