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

#[cfg(vesper_port)]
use crate::exec::{Verdict, execute};
use crate::shrink::{ShrinkConfig, ShrinkStats, shrink};
#[cfg(vesper_port)]
use crate::source::{CmpKind, CmpOperands, CmpRecord, DrawKind, Feature};
use crate::source::{DEFAULT_MAX_DRAWS, Failure, PropError, Source};
use crate::tape::ChoiceTape;

/// Failure kind of a counterexample whose failure did not reproduce under
/// strict replay of its own tape.
pub const NONDETERMINISTIC_KIND: &str = "nondeterministic";

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

/// Fuzz `prop` until it fails or the execution budget is spent. A failure is
/// shrunk with `config.shrink` before it is returned.
pub fn fuzz<P>(config: &FuzzConfig, mut prop: P) -> FuzzReport
where
    P: FnMut(&mut Source) -> Result<(), PropError>,
{
    #[cfg(vesper_port)]
    let (mut report, found) = campaign_port(config, &mut prop);
    #[cfg(not(vesper_port))]
    let (mut report, found) = crate::engine::fuzz::campaign(config, &mut prop);
    if let Some((values, failure, at)) = found {
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
                    NONDETERMINISTIC_KIND,
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

/// The Vesper build of the campaign: the step machine in vesper/src/lib.ves
/// (`Fuzzer`) chooses every run (random, solver candidate or mutation) and
/// its source seed, tracks coverage and owns the corpus; this loop runs the
/// property closure and reports each verdict, its features and, for a kept
/// run, its draw kinds and comparisons.
#[cfg(vesper_port)]
fn campaign_port<P>(
    config: &FuzzConfig,
    prop: &mut P,
) -> (FuzzReport, Option<(Vec<u64>, Failure, u64)>)
where
    P: FnMut(&mut Source) -> Result<(), PropError>,
{
    let random = config.strategy == Strategy::Random;
    let mut machine = crate::binding::Fuzzer::new(
        config.seed,
        config.max_execs,
        random,
        config.initial_random,
        config.max_solver_candidates as u64,
    );
    let mut found = None;
    let mut features = Vec::new();
    while machine.next() {
        let src = Source::build_random(machine.candidate(), machine.seed(), config.max_draws);
        let run = execute(prop, src, config.catch_panics);
        features.clear();
        let verdict = match &run.verdict {
            Verdict::Pass => {
                if !random {
                    encode_features(&run.probe.features(config.value_profile), &mut features);
                }
                0
            }
            Verdict::Fail(_) => 1,
            Verdict::Invalid(_) => 2,
        };
        match machine.observe(verdict, &features) {
            1 => {
                let tags: Vec<u64> = run.kinds.iter().map(|k| kind_tag(*k)).collect();
                let maxes: Vec<u64> = run.kinds.iter().map(|k| k.max()).collect();
                let cmps = encode_cmps(run.probe.comparisons());
                machine.keep(&run.values, &tags, &maxes, &cmps);
            }
            2 => {
                let Verdict::Fail(failure) = run.verdict else {
                    unreachable!("only a failing run ends the campaign")
                };
                found = Some((run.values, failure, machine.stats().execs));
            }
            _ => {}
        }
    }
    let stats = machine.stats();
    let corpus = (0..crate::binding::fuzzer_corpus_len(&machine))
        .map(|e| ChoiceTape::new(crate::binding::fuzzer_corpus_entry(&machine, e)))
        .collect();
    let report = FuzzReport {
        execs: stats.execs,
        invalid_runs: stats.invalid,
        corpus,
        features: stats.features as usize,
        solver_runs: stats.solver_runs,
        solver_hits: stats.solver_hits,
        counterexample: None,
    };
    (report, found)
}

/// Feature words for `fuzzer_observe`: `(key, value)` pairs, tag in the
/// key's top two bits.
#[cfg(vesper_port)]
fn encode_features(features: &[Feature], out: &mut Vec<u64>) {
    for feature in features {
        let (key, value) = match *feature {
            Feature::Edge(id, bucket) => ((u64::from(id) << 9) | u64::from(bucket), 0),
            Feature::Cmp(site, taken, bucket) => (
                (1 << 62) | (u64::from(site) << 9) | (u64::from(taken) << 8) | u64::from(bucket),
                0,
            ),
            Feature::Closeness(site, close) => ((2 << 62) | u64::from(site), u64::from(close)),
        };
        out.push(key);
        out.push(value);
    }
}

#[cfg(vesper_port)]
fn kind_tag(kind: DrawKind) -> u64 {
    match kind {
        DrawKind::Value { .. } => 0,
        DrawKind::Byte => 1,
        DrawKind::Flag => 2,
        DrawKind::Schedule => 3,
    }
}

/// Comparison words for `fuzzer_keep` (the solver's input).
#[cfg(vesper_port)]
fn encode_cmps(records: &[CmpRecord]) -> Vec<u64> {
    let mut out = Vec::new();
    for record in records {
        match &record.operands {
            CmpOperands::Int { kind, lhs, rhs } => {
                let kind = match kind {
                    CmpKind::Eq => 0,
                    CmpKind::Ne => 1,
                    CmpKind::Lt => 2,
                    CmpKind::Le => 3,
                    CmpKind::Gt => 4,
                    CmpKind::Ge => 5,
                };
                out.extend([0, kind, *lhs, *rhs]);
            }
            CmpOperands::Bytes { lhs, rhs } => {
                out.extend([1, lhs.len() as u64, rhs.len() as u64]);
                out.extend(lhs.iter().map(|&b| u64::from(b)));
                out.extend(rhs.iter().map(|&b| u64::from(b)));
            }
        }
    }
    out
}
