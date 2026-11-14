//! Stabilizer-style layout randomization for paired A/B runs.
//!
//! Each randomized run draws a [`LayoutPlan`] from a seed: a heap offset, a
//! stack padding and an environment size. In-process runs apply the heap offset
//! (a live allocation held across the run, plus [`OffsetBuffer`] for input data
//! placed at that page offset) and the stack padding (real stack frames between
//! the caller and the benchmark). The environment size only changes a child
//! process, so it is applied by a driver that spawns one process per layout
//! (`vesper bench --randomize-layout`). Each run is reduced to one aggregate
//! paired sample, and [`layout_survival`] asks whether the exact median interval
//! over the randomized layouts still excludes "no difference".
//!
//! Limits: this does not relocate machine code, function entries or globals
//! (Stabilizer also re-randomizes code and globals at runtime), and the
//! allocator's own placement policy decides how much a held block shifts later
//! allocations. Inference treats layouts as independent random draws.
use crate::{
    Analysis, BenchCase, Decision, Error, PairedSample, RunConfig, RunError, RunReport,
    analyze_pairs, run_abba,
};
use std::cmp::Ordering;

/// Largest number of randomized layouts one survival test accepts.
pub const MAX_LAYOUT_RUNS: usize = 64;
/// Smallest number of layouts that gives a finite >=95% median interval.
pub const MIN_LAYOUT_RUNS: usize = 6;
/// Heap offsets are 16-byte multiples in [0, 4096).
pub const HEAP_OFFSET_STEP: u32 = 16;
/// Stack padding is a multiple of 64-byte frames in [0, 4096).
pub const STACK_FRAME_BYTES: u32 = 64;
/// Environment padding is in [0, 4096) bytes.
pub const MAX_ENV_PADDING: u32 = 4096;
/// Environment variable a layout driver sets to change a child's environment size.
pub const ENV_PADDING_VAR: &str = "VESPER_BENCH_LAYOUT_PAD";

/// One randomized layout. Deterministic in (seed, run).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutPlan {
    pub seed: u64,
    pub run: u32,
    pub heap_offset_bytes: u32,
    pub stack_padding_bytes: u32,
    pub env_padding_bytes: u32,
    /// Seed handed to the run's `RunConfig::layout_seed`.
    pub run_seed: u64,
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Draws the layout for `run` under `seed` (SplitMix64, an explicit modular
/// generator; wrapping is its definition).
pub fn layout_plan(seed: u64, run: u32) -> LayoutPlan {
    let mut state = seed ^ (u64::from(run) << 32 | u64::from(run));
    // Mix the run index in again so neighbouring runs decorrelate.
    let _ = splitmix64(&mut state);
    let heap = (splitmix64(&mut state) % u64::from(4096 / HEAP_OFFSET_STEP)) as u32;
    let stack = (splitmix64(&mut state) % u64::from(4096 / STACK_FRAME_BYTES)) as u32;
    let env = (splitmix64(&mut state) % u64::from(MAX_ENV_PADDING)) as u32;
    LayoutPlan {
        seed,
        run,
        heap_offset_bytes: heap * HEAP_OFFSET_STEP,
        stack_padding_bytes: stack * STACK_FRAME_BYTES,
        env_padding_bytes: env,
        run_seed: splitmix64(&mut state),
    }
}

#[inline(never)]
fn pad_frames<R, F: FnOnce() -> R>(frames: u32, f: F) -> R {
    if frames == 0 {
        return f();
    }
    // A live 64-byte local in every frame; black_box keeps it on the stack and
    // the use after the call keeps this from becoming a tail call.
    let frame = std::hint::black_box([frames as u8; STACK_FRAME_BYTES as usize]);
    let result = pad_frames(frames - 1, f);
    std::hint::black_box(&frame);
    result
}

/// Runs `f` below at least `bytes` (rounded down to 64-byte frames) of extra
/// stack. Each frame is a real non-inlined call holding a live 64-byte local;
/// the exact distance also includes each frame's return address and spills.
pub fn with_stack_padding<R>(bytes: u32, f: impl FnOnce() -> R) -> R {
    pad_frames(bytes / STACK_FRAME_BYTES, f)
}

/// A live heap block that shifts the allocator state for the run it spans.
/// Held until dropped; zero bytes allocate nothing.
#[must_use = "the heap offset only applies while the shim is alive"]
pub struct HeapShim {
    block: Vec<u8>,
}
impl HeapShim {
    pub fn new(bytes: u32) -> Result<Self, Error> {
        let mut block = Vec::new();
        block
            .try_reserve_exact(bytes as usize)
            .map_err(|_| Error::AllocationFailed)?;
        block.resize(bytes as usize, 0xa5);
        Ok(Self {
            block: std::hint::black_box(block),
        })
    }
    pub fn len(&self) -> usize {
        self.block.len()
    }
    pub fn is_empty(&self) -> bool {
        self.block.is_empty()
    }
}

/// A byte buffer whose first byte sits at a chosen offset within a 4 KiB page,
/// so input data placement follows the layout plan rather than the allocator.
pub struct OffsetBuffer {
    storage: Vec<u8>,
    start: usize,
    len: usize,
}
impl OffsetBuffer {
    /// `page_offset` must be below 4096.
    pub fn new(len: usize, page_offset: u32) -> Result<Self, Error> {
        if page_offset >= 4096 {
            return Err(Error::InvalidConfig("page offset must be below 4096"));
        }
        let capacity = len.checked_add(8192).ok_or(Error::ArithmeticOverflow)?;
        let mut storage = Vec::new();
        storage
            .try_reserve_exact(capacity)
            .map_err(|_| Error::AllocationFailed)?;
        storage.resize(capacity, 0);
        let base = storage.as_ptr() as usize;
        let target = page_offset as usize;
        let start = (target + 4096 - base % 4096) % 4096;
        Ok(Self {
            storage,
            start,
            len,
        })
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.storage[self.start..self.start + self.len]
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.storage[self.start..self.start + self.len]
    }
}

/// The value a driver assigns to [`ENV_PADDING_VAR`] for a plan.
pub fn env_padding_value(bytes: u32) -> String {
    "x".repeat(bytes.min(MAX_ENV_PADDING - 1) as usize)
}

/// Sums every measured epoch of one run into one paired sample.
pub fn layout_sample(report: &RunReport) -> Result<PairedSample, Error> {
    let mut baseline = 0u64;
    let mut candidate = 0u64;
    let mut operations = 0u64;
    for pair in report.pairs() {
        baseline = baseline
            .checked_add(pair.baseline_total())
            .ok_or(Error::ArithmeticOverflow)?;
        candidate = candidate
            .checked_add(pair.candidate_total())
            .ok_or(Error::ArithmeticOverflow)?;
        operations = operations
            .checked_add(pair.operations_per_arm())
            .ok_or(Error::ArithmeticOverflow)?;
    }
    PairedSample::new(baseline, candidate, operations)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutVerdict {
    /// The interval lies entirely above ratio 1: the candidate is slower in
    /// the median randomized layout.
    CandidateSlower,
    /// The interval lies entirely below ratio 1: the candidate is faster.
    CandidateFaster,
    /// The interval contains ratio 1: the difference does not survive
    /// randomization (it may be a layout artifact, or there may be none).
    DoesNotSurvive,
    /// Fewer than six layouts: no finite interval.
    InsufficientLayouts,
}
impl LayoutVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CandidateSlower => "candidate_slower",
            Self::CandidateFaster => "candidate_faster",
            Self::DoesNotSurvive => "does_not_survive",
            Self::InsufficientLayouts => "insufficient_layouts",
        }
    }
    pub fn survives(self) -> bool {
        matches!(self, Self::CandidateSlower | Self::CandidateFaster)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutSurvival {
    pub verdict: LayoutVerdict,
    pub analysis: Analysis,
    pub layouts: usize,
}

/// Exact two-sided >=95% median-ratio interval over per-layout samples, tested
/// against ratio 1. Assumes the layouts are independent draws.
pub fn layout_survival(per_layout: &[PairedSample]) -> Result<LayoutSurvival, Error> {
    if per_layout.len() > MAX_LAYOUT_RUNS {
        return Err(Error::Limit("layouts"));
    }
    let analysis = analyze_pairs(per_layout, 0)?;
    let verdict = match (&analysis.interval, analysis.decision) {
        (None, _) => LayoutVerdict::InsufficientLayouts,
        (Some(_), Decision::Regression) => LayoutVerdict::CandidateSlower,
        // Both arms of a sample run the same operation count, so the ratio is
        // below one exactly when the candidate total is below the baseline's.
        (Some(interval), _)
            if interval
                .upper
                .candidate_total()
                .cmp(&interval.upper.baseline_total())
                == Ordering::Less =>
        {
            LayoutVerdict::CandidateFaster
        }
        (Some(_), _) => LayoutVerdict::DoesNotSurvive,
    };
    Ok(LayoutSurvival {
        verdict,
        analysis,
        layouts: per_layout.len(),
    })
}

/// One measured layout of an in-process randomized comparison.
#[derive(Debug)]
pub struct LayoutRun {
    pub plan: LayoutPlan,
    pub sample: PairedSample,
    pub report: RunReport,
}
#[derive(Debug)]
pub struct RandomizedReport {
    pub runs: Vec<LayoutRun>,
    pub survival: LayoutSurvival,
}

/// Runs `layouts` paired ABBA comparisons, each under a fresh heap shim and
/// stack padding drawn from `seed`, then tests whether the difference survives.
/// The environment size is not changed in-process; see the module docs.
pub fn run_randomized_layouts<A: BenchCase, B: BenchCase>(
    config: RunConfig,
    layouts: usize,
    seed: u64,
    baseline: &mut A,
    candidate: &mut B,
) -> Result<RandomizedReport, RunError<A::Error, B::Error>> {
    if !(1..=MAX_LAYOUT_RUNS).contains(&layouts) {
        return Err(RunError::Harness(Error::InvalidConfig(
            "layouts must be 1..=64",
        )));
    }
    let mut runs = Vec::new();
    runs.try_reserve_exact(layouts)
        .map_err(|_| RunError::Harness(Error::AllocationFailed))?;
    for run in 0..layouts {
        let plan = layout_plan(seed, run as u32);
        let shim = HeapShim::new(plan.heap_offset_bytes).map_err(RunError::Harness)?;
        let config = RunConfig {
            layout_seed: plan.run_seed,
            ..config
        };
        let report = with_stack_padding(plan.stack_padding_bytes, || {
            run_abba(config, &mut *baseline, &mut *candidate)
        })?;
        drop(shim);
        let sample = layout_sample(&report).map_err(RunError::Harness)?;
        runs.push(LayoutRun {
            plan,
            sample,
            report,
        });
    }
    let samples: Vec<PairedSample> = runs.iter().map(|r| r.sample).collect();
    let survival = layout_survival(&samples).map_err(RunError::Harness)?;
    Ok(RandomizedReport { runs, survival })
}
