//! Frozen Rust choice-tape shrinker fallback for `vesper-crucible-seed`.

use crate::exec::{Run, Verdict, execute};
use crate::shrink::{ShrinkConfig, ShrinkStats, Shrunk};
use crate::source::{Failure, PropError, Source};
use crate::tape::{ChoiceTape, ShrinkKey};

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

    fn attempt(&mut self, candidate: Vec<u64>) -> bool {
        let best_key = ShrinkKey::of(&self.best);
        if !ShrinkKey::of(&candidate).smaller_than(best_key) || self.out_of_budget() {
            return false;
        }
        self.stats.runs += 1;
        let run = self.run(candidate);
        match run.verdict {
            Verdict::Fail(failure) if failure.kind == self.kind => {
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

pub(crate) fn reduce<P>(
    prop: &mut P,
    config: &ShrinkConfig,
    failure: Failure,
    first: Vec<u64>,
    initial: ShrinkKey,
) -> Shrunk
where
    P: FnMut(&mut Source) -> Result<(), PropError>,
{
    let mut reducer = Reducer {
        prop,
        config: config.clone(),
        kind: failure.kind.clone(),
        best: first,
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
    Shrunk {
        tape: ChoiceTape::new(reducer.best),
        failure: reducer.failure,
        stats: reducer.stats,
    }
}
