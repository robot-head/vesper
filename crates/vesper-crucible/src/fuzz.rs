//! Coverage-guided fuzzing over choice tapes with a comparison-operand
//! solver (§29.1.1).
//!
//! The target reports edges and comparison operands through [`Probe`]. Runs
//! that reach a new coverage feature join the corpus. Each new corpus entry
//! is handed once to the solver, which performs bounded input-to-state
//! replacement: for every recorded comparison it searches the entry's tape
//! for the operand (as a whole drawn value, or as 1/2/4/8 little- or
//! big-endian bytes in the byte lane) and substitutes the other operand,
//! with ±1 variants for ordering comparisons. That pierces magic-number,
//! checksum-equality and length-prefix guards whose operand is copied from
//! the input. It is not an SMT solver: operands computed non-trivially from
//! the input (for example a hash of the field being compared) are out of
//! reach, and no path condition is negated symbolically. Z3/Bitwuzla were
//! not used because neither builds from crates.io on all three CI platforms
//! without a system library.

use crate::exec::{Run, Verdict, execute};
use crate::rng::SplitMix64;
use crate::shrink::{ShrinkConfig, ShrinkStats, shrink};
use crate::source::{
    CmpKind, CmpOperands, CmpRecord, DEFAULT_MAX_DRAWS, DrawKind, Failure, Feature, PropError,
    Source,
};
use crate::tape::ChoiceTape;
use std::collections::{BTreeMap, BTreeSet};

/// Search strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Corpus, coverage feedback, mutation and the comparison solver.
    Guided,
    /// Fresh pseudo-random tapes only: the baseline the guided mode is
    /// measured against.
    Random,
}

/// Fuzzer configuration. Every run is a function of `seed`.
#[derive(Clone, Debug)]
pub struct FuzzConfig {
    pub seed: u64,
    /// Property executions before giving up (shrinking is budgeted
    /// separately by `shrink.max_runs`).
    pub max_execs: u64,
    pub strategy: Strategy,
    /// Fresh random runs before mutation starts (guided only).
    pub initial_random: u64,
    /// Count per-site operand closeness as coverage (guided only).
    pub value_profile: bool,
    /// Solver candidates tried per corpus entry.
    pub max_solver_candidates: usize,
    pub max_draws: usize,
    pub catch_panics: bool,
    pub shrink: ShrinkConfig,
}

impl Default for FuzzConfig {
    fn default() -> Self {
        Self {
            seed: 0x00c0_ffee,
            max_execs: 100_000,
            strategy: Strategy::Guided,
            initial_random: 64,
            value_profile: true,
            max_solver_candidates: 256,
            max_draws: DEFAULT_MAX_DRAWS,
            catch_panics: true,
            shrink: ShrinkConfig::default(),
        }
    }
}

/// A found and shrunk counterexample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counterexample {
    /// The minimal failing tape: replay it with [`crate::replay`].
    pub tape: ChoiceTape,
    pub failure: Failure,
    /// The tape as first found, before shrinking.
    pub original: ChoiceTape,
    /// Executions (1-based) up to and including the first failing one.
    pub found_after_execs: u64,
    pub shrink: ShrinkStats,
}

/// Result of a fuzzing campaign.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzReport {
    pub execs: u64,
    pub invalid_runs: u64,
    pub corpus: Vec<ChoiceTape>,
    pub features: usize,
    pub solver_runs: u64,
    /// Solver runs that reached new coverage or a failure.
    pub solver_hits: u64,
    pub counterexample: Option<Counterexample>,
}

struct Entry {
    values: Vec<u64>,
    kinds: Vec<DrawKind>,
    cmps: Vec<CmpRecord>,
    solved: bool,
}

struct Fuzzer<'p, P> {
    prop: &'p mut P,
    config: FuzzConfig,
    rng: SplitMix64,
    corpus: Vec<Entry>,
    seen: BTreeSet<Feature>,
    closeness: BTreeMap<u32, u32>,
    report: FuzzReport,
    found: Option<(Vec<u64>, Failure, u64)>,
}

impl<P> Fuzzer<'_, P>
where
    P: FnMut(&mut Source) -> Result<(), PropError>,
{
    fn budget_left(&self) -> bool {
        self.found.is_none() && self.report.execs < self.config.max_execs
    }

    fn run(&mut self, prefix: Vec<u64>) -> Run {
        let seed = self.rng.next_u64();
        let src = Source::build_random(prefix, seed, self.config.max_draws);
        self.report.execs += 1;
        execute(self.prop, src, self.config.catch_panics)
    }

    /// Record a run; returns true if it reached new coverage or failed.
    fn observe(&mut self, run: Run) -> bool {
        match run.verdict {
            Verdict::Fail(failure) => {
                self.found = Some((run.values, failure, self.report.execs));
                true
            }
            Verdict::Invalid(_) => {
                self.report.invalid_runs += 1;
                false
            }
            Verdict::Pass => {
                if self.config.strategy == Strategy::Random {
                    return false;
                }
                let mut novel = false;
                for feature in run.probe.features(self.config.value_profile) {
                    match feature {
                        Feature::Closeness(site, close) => {
                            let best = self.closeness.entry(site).or_insert(0);
                            if close > *best {
                                *best = close;
                                novel = true;
                            }
                        }
                        other => novel |= self.seen.insert(other),
                    }
                }
                if novel {
                    self.corpus.push(Entry {
                        values: run.values,
                        kinds: run.kinds,
                        cmps: run.probe.comparisons().to_vec(),
                        solved: false,
                    });
                }
                novel
            }
        }
    }

    fn campaign(&mut self) {
        while self.budget_left() {
            let random_phase = self.config.strategy == Strategy::Random
                || self.corpus.is_empty()
                || self.report.execs < self.config.initial_random;
            if random_phase {
                let run = self.run(Vec::new());
                self.observe(run);
                continue;
            }
            if let Some(index) = self.corpus.iter().position(|e| !e.solved) {
                self.corpus[index].solved = true;
                let candidates =
                    solver_candidates(&self.corpus[index], self.config.max_solver_candidates);
                for candidate in candidates {
                    if !self.budget_left() {
                        break;
                    }
                    self.report.solver_runs += 1;
                    let run = self.run(candidate);
                    if self.observe(run) {
                        self.report.solver_hits += 1;
                    }
                }
                continue;
            }
            let candidate = self.mutate();
            let run = self.run(candidate);
            self.observe(run);
        }
    }

    fn mutate(&mut self) -> Vec<u64> {
        let index = self.rng.index(self.corpus.len());
        let mut values = self.corpus[index].values.clone();
        let kinds = self.corpus[index].kinds.clone();
        let rounds = 1 + self.rng.index(4);
        for _ in 0..rounds {
            let len = values.len();
            if len == 0 {
                values.push(self.rng.next_u64());
                continue;
            }
            let pos = self.rng.index(len);
            let max = kinds.get(pos).map_or(u64::MAX, |k| k.max());
            match self.rng.index(10) {
                0 => values[pos] = self.rng.below_inclusive(max),
                1 => values[pos] = 0,
                2 => values[pos] = max,
                3 => {
                    let delta = 1 + self.rng.below_inclusive(15);
                    values[pos] = values[pos].saturating_add(delta).min(max);
                }
                4 => {
                    let delta = 1 + self.rng.below_inclusive(15);
                    values[pos] = values[pos].saturating_sub(delta);
                }
                5 => {
                    let span = (1 + self.rng.index(8)).min(len - pos);
                    values.drain(pos..pos + span);
                }
                6 => {
                    let span = (1 + self.rng.index(8)).min(len - pos);
                    let copy = values[pos..pos + span].to_vec();
                    let at = self.rng.index(len + 1);
                    values.splice(at..at, copy);
                }
                7 => values.truncate(pos),
                8 => {
                    let other = self.rng.index(self.corpus.len());
                    let donor = &self.corpus[other].values;
                    let cut = self.rng.index(donor.len() + 1);
                    values.truncate(pos);
                    values.extend_from_slice(&donor[cut..]);
                }
                _ => {
                    let bit = self.rng.below_inclusive(63);
                    values[pos] = (values[pos] ^ (1u64 << bit)).min(max);
                }
            }
        }
        values
    }
}

/// Bounded input-to-state candidates for one corpus entry.
fn solver_candidates(entry: &Entry, limit: usize) -> Vec<Vec<u64>> {
    let lane: Vec<usize> = entry
        .kinds
        .iter()
        .enumerate()
        .filter(|(_, k)| **k == DrawKind::Byte)
        .map(|(i, _)| i)
        .collect();
    let mut out = BTreeSet::new();
    let push = |candidate: Vec<u64>, out: &mut BTreeSet<Vec<u64>>| {
        if candidate != entry.values && out.len() < limit {
            out.insert(candidate);
        }
    };
    for record in &entry.cmps {
        if out.len() >= limit {
            break;
        }
        match &record.operands {
            CmpOperands::Int { kind, lhs, rhs } => {
                for (needle, target) in [(*lhs, *rhs), (*rhs, *lhs)] {
                    let mut replacements = vec![target];
                    if !matches!(kind, CmpKind::Eq | CmpKind::Ne) {
                        replacements.extend(target.checked_add(1));
                        replacements.extend(target.checked_sub(1));
                    }
                    for repl in replacements {
                        // Whole drawn values.
                        for (i, (&v, kind)) in entry.values.iter().zip(&entry.kinds).enumerate() {
                            if v == needle
                                && matches!(kind, DrawKind::Value { .. })
                                && repl <= kind.max()
                            {
                                let mut c = entry.values.clone();
                                c[i] = repl;
                                push(c, &mut out);
                            }
                        }
                        // Byte-lane encodings.
                        for width in [2usize, 4, 8, 1] {
                            let Some(n) = encode(needle, width) else {
                                continue;
                            };
                            let Some(r) = encode(repl, width) else {
                                continue;
                            };
                            for (n_bytes, r_bytes) in [(&n.0, &r.0), (&n.1, &r.1)] {
                                for start in find_in_lane(entry, &lane, n_bytes) {
                                    push(replace_in_lane(entry, &lane, start, r_bytes), &mut out);
                                }
                            }
                        }
                    }
                }
            }
            CmpOperands::Bytes { lhs, rhs } => {
                for (needle, target) in [(lhs, rhs), (rhs, lhs)] {
                    if needle.is_empty() {
                        continue;
                    }
                    let width = needle.len().min(target.len());
                    if width == 0 {
                        continue;
                    }
                    for start in find_in_lane(entry, &lane, needle) {
                        push(
                            replace_in_lane(entry, &lane, start, &target[..width]),
                            &mut out,
                        );
                    }
                }
            }
        }
    }
    out.into_iter().collect()
}

/// Little- and big-endian encodings of `value` in `width` bytes, if it fits.
fn encode(value: u64, width: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    if width < 8 && value >> (8 * width) != 0 {
        return None;
    }
    let le = value.to_le_bytes()[..width].to_vec();
    let mut be = le.clone();
    be.reverse();
    Some((le, be))
}

/// Start offsets (into the byte lane) where `needle` occurs. Capped so a
/// one-byte needle in a long buffer cannot flood the candidate set.
fn find_in_lane(entry: &Entry, lane: &[usize], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > lane.len() {
        return Vec::new();
    }
    (0..=lane.len() - needle.len())
        .filter(|&start| {
            needle
                .iter()
                .enumerate()
                .all(|(k, &b)| entry.values[lane[start + k]] == u64::from(b))
        })
        .take(8)
        .collect()
}

fn replace_in_lane(entry: &Entry, lane: &[usize], start: usize, bytes: &[u8]) -> Vec<u64> {
    let mut c = entry.values.clone();
    for (k, &b) in bytes.iter().enumerate() {
        if let Some(&pos) = lane.get(start + k) {
            c[pos] = u64::from(b);
        }
    }
    c
}

/// Fuzz `prop` until it fails or the execution budget is spent. A failure is
/// shrunk with `config.shrink` before it is returned.
pub fn fuzz<P>(config: &FuzzConfig, mut prop: P) -> FuzzReport
where
    P: FnMut(&mut Source) -> Result<(), PropError>,
{
    let mut fuzzer = Fuzzer {
        prop: &mut prop,
        config: config.clone(),
        rng: SplitMix64(config.seed),
        corpus: Vec::new(),
        seen: BTreeSet::new(),
        closeness: BTreeMap::new(),
        report: FuzzReport {
            execs: 0,
            invalid_runs: 0,
            corpus: Vec::new(),
            features: 0,
            solver_runs: 0,
            solver_hits: 0,
            counterexample: None,
        },
        found: None,
    };
    fuzzer.campaign();
    let mut report = fuzzer.report;
    report.features = fuzzer.seen.len() + fuzzer.closeness.len();
    report.corpus = fuzzer
        .corpus
        .into_iter()
        .map(|e| ChoiceTape::new(e.values))
        .collect();
    if let Some((values, failure, at)) = fuzzer.found {
        let original = ChoiceTape::new(values);
        let mut shrink_config = config.shrink.clone();
        shrink_config.catch_panics = config.catch_panics;
        report.counterexample = Some(match shrink(&mut prop, &original, &shrink_config) {
            Ok(shrunk) => Counterexample {
                tape: shrunk.tape,
                failure: shrunk.failure,
                original,
                found_after_execs: at,
                shrink: shrunk.stats,
            },
            // A property whose failure does not replay is nondeterministic;
            // report the unshrunk tape and say so in the failure.
            Err(not) => Counterexample {
                tape: original.clone(),
                failure: Failure::new(
                    "nondeterministic",
                    format!("{failure}; strict replay gave {:?}", not.0),
                ),
                shrink: ShrinkStats {
                    runs: 1,
                    accepted: 0,
                    invalid: 0,
                    not_reproducing: 1,
                    initial: original.key(),
                    shrunk: original.key(),
                    budget_exhausted: false,
                },
                original,
                found_after_execs: at,
            },
        });
    }
    report
}
