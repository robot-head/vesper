//! Coz-style causal profiling for host threads.
//!
//! A program marks named regions (the code whose speedup is being asked about)
//! and progress points (the end-to-end events whose rate matters). An
//! experiment selects one region and a virtual speedup `s` percent. Every time
//! a thread finishes the selected region after `t` nanoseconds, it adds
//! `t * s / 100` to a global delay count and to its own local count. Every other
//! thread, at its next delay point (a region entry, a progress point or an
//! explicit [`CausalThread::delay_point`]), pauses until its local count
//! catches up with the global one. Pausing everyone else by `d` is the same,
//! relative to the program's progress, as making the region `d` faster; the
//! experiment's effective time is its elapsed time minus the global delay.
//! The predicted program speedup is the drop in effective time per progress
//! visit against experiments with no region selected.
//!
//! Limits, relative to Coz: regions are explicit instrumented calls, not
//! sampled source lines; delays accrue when the selected region finishes,
//! not per sample; and there is no credit for delays accrued while a thread
//! was blocked (Coz forwards them from the waking thread). A thread blocked on
//! something other than the selected region can therefore be over-delayed,
//! which biases predictions toward zero. Threads are host OS threads, not
//! Pulse fibers. All timing is `std::time::Instant` wall time.
use crate::Error;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub const MAX_CAUSAL_REGIONS: usize = 64;
pub const MAX_PROGRESS_POINTS: usize = 64;
pub const MAX_SPEEDUP_STEPS: usize = 21;
pub const MAX_REPETITIONS: usize = 15;
const NONE: usize = usize::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionId(usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgressPoint(usize);

struct Shared {
    regions: Vec<&'static str>,
    progress_names: Vec<&'static str>,
    selected: AtomicUsize,
    speedup_pct: AtomicU32,
    /// Monotonic across experiments; an experiment's delay is the growth
    /// since `start_delay_ns`.
    global_delay_ns: AtomicU64,
    start_delay_ns: AtomicU64,
    generation: AtomicU64,
    paused_ns: AtomicU64,
    progress: Vec<AtomicU64>,
    region_visits: Vec<AtomicU64>,
    region_ns: Vec<AtomicU64>,
}

/// Shared profiler state. Clone the handle into the threads of a program and
/// give each thread its own [`CausalThread`].
#[derive(Clone)]
pub struct CausalProfiler {
    shared: Arc<Shared>,
}

fn unique(names: &[&'static str], limit: usize) -> Result<(), Error> {
    if names.is_empty() || names.len() > limit {
        return Err(Error::InvalidConfig("causal names must be 1..=64"));
    }
    for (i, a) in names.iter().enumerate() {
        if a.is_empty() || names[..i].contains(a) {
            return Err(Error::InvalidConfig(
                "causal names must be unique and nonempty",
            ));
        }
    }
    Ok(())
}
fn counters(n: usize) -> Vec<AtomicU64> {
    (0..n).map(|_| AtomicU64::new(0)).collect()
}

impl CausalProfiler {
    pub fn new(regions: &[&'static str], progress: &[&'static str]) -> Result<Self, Error> {
        unique(regions, MAX_CAUSAL_REGIONS)?;
        unique(progress, MAX_PROGRESS_POINTS)?;
        Ok(Self {
            shared: Arc::new(Shared {
                regions: regions.to_vec(),
                progress_names: progress.to_vec(),
                selected: AtomicUsize::new(NONE),
                speedup_pct: AtomicU32::new(0),
                global_delay_ns: AtomicU64::new(0),
                start_delay_ns: AtomicU64::new(0),
                generation: AtomicU64::new(0),
                paused_ns: AtomicU64::new(0),
                progress: counters(progress.len()),
                region_visits: counters(regions.len()),
                region_ns: counters(regions.len()),
            }),
        })
    }
    pub fn region(&self, name: &str) -> Result<RegionId, Error> {
        self.shared
            .regions
            .iter()
            .position(|r| *r == name)
            .map(RegionId)
            .ok_or(Error::InvalidConfig("unknown causal region"))
    }
    pub fn progress_point(&self, name: &str) -> Result<ProgressPoint, Error> {
        self.shared
            .progress_names
            .iter()
            .position(|r| *r == name)
            .map(ProgressPoint)
            .ok_or(Error::InvalidConfig("unknown progress point"))
    }
    /// A per-thread handle. Its local delay count starts caught up, so a new
    /// thread never pays for delays inserted before it existed.
    pub fn thread(&self) -> CausalThread {
        CausalThread {
            shared: Arc::clone(&self.shared),
            generation: self.shared.generation.load(Ordering::Acquire),
            local_delay_ns: self.shared.global_delay_ns.load(Ordering::Acquire),
        }
    }
    /// Delay inserted since the current experiment began.
    pub fn global_delay_nanos(&self) -> u64 {
        let s = &self.shared;
        s.global_delay_ns.load(Ordering::Acquire) - s.start_delay_ns.load(Ordering::Acquire)
    }
    /// Nanoseconds threads actually spent paused for virtual speedup.
    pub fn paused_nanos(&self) -> u64 {
        self.shared.paused_ns.load(Ordering::Acquire)
    }
    pub fn progress_count(&self, point: ProgressPoint) -> u64 {
        self.shared.progress[point.0].load(Ordering::Acquire)
    }
    pub fn region_visits(&self, region: RegionId) -> u64 {
        self.shared.region_visits[region.0].load(Ordering::Acquire)
    }
    pub fn region_nanos(&self, region: RegionId) -> u64 {
        self.shared.region_ns[region.0].load(Ordering::Acquire)
    }
    fn reset(&self, selected: Option<RegionId>, speedup_pct: u32) {
        let s = &self.shared;
        s.selected
            .store(selected.map_or(NONE, |r| r.0), Ordering::Release);
        s.speedup_pct.store(speedup_pct, Ordering::Release);
        for c in s
            .progress
            .iter()
            .chain(&s.region_visits)
            .chain(&s.region_ns)
        {
            c.store(0, Ordering::Release);
        }
        // Keep the global count monotonic and open a new generation, so a
        // handle retained from an earlier experiment starts caught up with
        // this one instead of keeping stale credit or stale debt.
        s.start_delay_ns
            .store(s.global_delay_ns.load(Ordering::Acquire), Ordering::Release);
        s.generation.fetch_add(1, Ordering::AcqRel);
        s.paused_ns.store(0, Ordering::Release);
    }
}

/// One thread's view of the profiler. Not Clone: each thread has exactly one
/// local delay count.
pub struct CausalThread {
    shared: Arc<Shared>,
    generation: u64,
    local_delay_ns: u64,
}

fn pause(nanos: u64) {
    let start = Instant::now();
    let target = Duration::from_nanos(nanos);
    // Sleep for the bulk, spin for the last millisecond.
    if target > Duration::from_millis(2) {
        std::thread::sleep(target - Duration::from_millis(1));
    }
    while start.elapsed() < target {
        std::hint::spin_loop();
    }
}

impl CausalThread {
    /// Pays any virtual-speedup delay this thread owes. Returns nanoseconds paused.
    fn sync_generation(&mut self) {
        let generation = self.shared.generation.load(Ordering::Acquire);
        if generation != self.generation {
            self.generation = generation;
            self.local_delay_ns = self.shared.start_delay_ns.load(Ordering::Acquire);
        }
    }
    pub fn delay_point(&mut self) -> u64 {
        self.sync_generation();
        let global = self.shared.global_delay_ns.load(Ordering::Acquire);
        if global <= self.local_delay_ns {
            return 0;
        }
        let owed = global - self.local_delay_ns;
        pause(owed);
        self.local_delay_ns = global;
        self.shared.paused_ns.fetch_add(owed, Ordering::AcqRel);
        owed
    }
    /// Runs an instrumented region. If it is the selected region, other threads
    /// owe `elapsed * speedup / 100` nanoseconds of delay; this thread does not.
    pub fn region<R>(&mut self, region: RegionId, f: impl FnOnce() -> R) -> R {
        self.delay_point();
        let start = Instant::now();
        let result = f();
        self.sync_generation();
        let elapsed = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let s = &self.shared;
        s.region_visits[region.0].fetch_add(1, Ordering::AcqRel);
        s.region_ns[region.0].fetch_add(elapsed, Ordering::AcqRel);
        if s.selected.load(Ordering::Acquire) == region.0 {
            let pct = u128::from(s.speedup_pct.load(Ordering::Acquire));
            // pct <= 100, so the product of a u64 and 100 fits u128 and the
            // quotient fits u64.
            let delay = (u128::from(elapsed) * pct / 100) as u64;
            s.global_delay_ns.fetch_add(delay, Ordering::AcqRel);
            self.local_delay_ns = self.local_delay_ns.saturating_add(delay);
        }
        result
    }
    /// Counts one visit to an end-to-end progress point, then pays owed delay.
    pub fn progress(&mut self, point: ProgressPoint) {
        self.shared.progress[point.0].fetch_add(1, Ordering::AcqRel);
        self.delay_point();
    }
}

#[derive(Clone, Debug)]
pub struct CausalConfig {
    /// Virtual speedups to try, each 1..=100 percent.
    pub speedups_pct: Vec<u32>,
    /// Runs per experiment; the median effective period is kept.
    pub repetitions: usize,
    /// Which progress point's rate defines program speed.
    pub progress_point: &'static str,
}

/// Effective time and progress of one experiment run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExperimentRun {
    pub elapsed_ns: u64,
    pub global_delay_ns: u64,
    pub effective_ns: u64,
    pub progress: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CausalPoint {
    pub region: &'static str,
    pub speedup_pct: u32,
    /// Median run by effective period.
    pub run: ExperimentRun,
    /// Predicted end-to-end speedup in basis points of the baseline period
    /// (positive is faster), floored.
    pub predicted_speedup_bp: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CausalReport {
    pub progress_point: &'static str,
    pub baseline: ExperimentRun,
    pub points: Vec<CausalPoint>,
}
impl CausalReport {
    /// The curve for one region, in the configured speedup order.
    pub fn curve(&self, region: &str) -> Vec<&CausalPoint> {
        self.points.iter().filter(|p| p.region == region).collect()
    }
}

/// Compares periods effective/progress exactly: a < b.
fn period_cmp(a: ExperimentRun, b: ExperimentRun) -> std::cmp::Ordering {
    (u128::from(a.effective_ns) * u128::from(b.progress))
        .cmp(&(u128::from(b.effective_ns) * u128::from(a.progress)))
}
fn median(mut runs: Vec<ExperimentRun>) -> ExperimentRun {
    runs.sort_by(|a, b| period_cmp(*a, *b));
    runs[runs.len() / 2]
}
/// 10000 * (1 - period_s / period_0), floored.
fn predicted_bp(baseline: ExperimentRun, run: ExperimentRun) -> Result<i64, Error> {
    let num = i128::from(run.effective_ns) * i128::from(baseline.progress) * 10_000;
    let den = i128::from(run.progress) * i128::from(baseline.effective_ns);
    if den == 0 {
        return Err(Error::EmptyEvidence);
    }
    i64::try_from(10_000 - num.div_euclid(den) - i128::from(num.rem_euclid(den) != 0))
        .map_err(|_| Error::ArithmeticOverflow)
}

/// Runs every experiment: `repetitions` baseline runs with no region selected,
/// then `repetitions` runs per (region, speedup). `program` must run a complete
/// workload with the profiler's threads and join them before returning.
pub fn profile<E>(
    profiler: &CausalProfiler,
    config: &CausalConfig,
    mut program: impl FnMut(&CausalProfiler) -> Result<(), E>,
) -> Result<CausalReport, CausalError<E>> {
    if config.speedups_pct.is_empty()
        || config.speedups_pct.len() > MAX_SPEEDUP_STEPS
        || config.speedups_pct.iter().any(|s| !(1..=100).contains(s))
    {
        return Err(CausalError::Harness(Error::InvalidConfig(
            "speedups must be 1..=21 values in 1..=100",
        )));
    }
    if !(1..=MAX_REPETITIONS).contains(&config.repetitions) {
        return Err(CausalError::Harness(Error::InvalidConfig(
            "repetitions must be 1..=15",
        )));
    }
    let point = profiler
        .progress_point(config.progress_point)
        .map_err(CausalError::Harness)?;
    // Every exit, including program and harness errors, turns virtual
    // speedup off again.
    struct Disarm<'a>(&'a CausalProfiler);
    impl Drop for Disarm<'_> {
        fn drop(&mut self) {
            self.0.reset(None, 0);
        }
    }
    let _disarm = Disarm(profiler);
    let mut experiment = |selected: Option<RegionId>, pct: u32| {
        let mut runs = Vec::with_capacity(config.repetitions);
        for _ in 0..config.repetitions {
            profiler.reset(selected, pct);
            let start = Instant::now();
            program(profiler).map_err(CausalError::Program)?;
            let elapsed = u64::try_from(start.elapsed().as_nanos())
                .map_err(|_| CausalError::Harness(Error::ClockOverflow))?;
            let delay = profiler.global_delay_nanos();
            let progress = profiler.progress_count(point);
            if progress == 0 {
                return Err(CausalError::Harness(Error::EmptyEvidence));
            }
            let effective = elapsed
                .checked_sub(delay)
                .filter(|e| *e > 0)
                .ok_or(CausalError::DelayExceedsElapsed { elapsed, delay })?;
            runs.push(ExperimentRun {
                elapsed_ns: elapsed,
                global_delay_ns: delay,
                effective_ns: effective,
                progress,
            });
        }
        Ok(median(runs))
    };
    let baseline = experiment(None, 0)?;
    let mut points = Vec::new();
    for (index, region) in profiler.shared.regions.iter().enumerate() {
        for &pct in &config.speedups_pct {
            let run = experiment(Some(RegionId(index)), pct)?;
            points.push(CausalPoint {
                region,
                speedup_pct: pct,
                run,
                predicted_speedup_bp: predicted_bp(baseline, run).map_err(CausalError::Harness)?,
            });
        }
    }
    Ok(CausalReport {
        progress_point: config.progress_point,
        baseline,
        points,
    })
}

#[derive(Debug)]
pub enum CausalError<E> {
    Harness(Error),
    Program(E),
    /// More delay was inserted than time elapsed, e.g. the selected region ran
    /// concurrently on several threads. No prediction is made.
    DelayExceedsElapsed {
        elapsed: u64,
        delay: u64,
    },
}
impl<E: std::fmt::Display> std::fmt::Display for CausalError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Harness(e) => e.fmt(f),
            Self::Program(e) => write!(f, "causal program failed: {e}"),
            Self::DelayExceedsElapsed { elapsed, delay } => write!(
                f,
                "inserted delay {delay}ns exceeds elapsed {elapsed}ns; no prediction"
            ),
        }
    }
}
impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for CausalError<E> {}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(effective: u64, progress: u64) -> ExperimentRun {
        ExperimentRun {
            elapsed_ns: effective,
            global_delay_ns: 0,
            effective_ns: effective,
            progress,
        }
    }
    #[test]
    fn predicted_speedup_is_exact_period_reduction() {
        assert_eq!(predicted_bp(run(100, 10), run(50, 10)), Ok(5000));
        assert_eq!(predicted_bp(run(100, 10), run(100, 20)), Ok(5000));
        assert_eq!(predicted_bp(run(100, 10), run(100, 10)), Ok(0));
        assert_eq!(predicted_bp(run(100, 10), run(150, 10)), Ok(-5000));
        // 1 - 2/3 = 3333.33.. floors to 3333.
        assert_eq!(predicted_bp(run(3, 1), run(2, 1)), Ok(3333));
        assert_eq!(
            predicted_bp(run(100, 10), run(50, 0)),
            Err(Error::EmptyEvidence)
        );
        assert_eq!(
            median(vec![run(30, 1), run(10, 1), run(20, 1)]).effective_ns,
            20
        );
    }
    #[test]
    fn selected_region_charges_other_threads_not_itself() {
        let p = CausalProfiler::new(&["hot", "cold"], &["done"]).unwrap();
        let hot = p.region("hot").unwrap();
        let cold = p.region("cold").unwrap();
        p.reset(Some(hot), 100);
        let mut a = p.thread();
        let mut b = p.thread();
        a.region(hot, || std::thread::sleep(Duration::from_millis(3)));
        let charged = p.global_delay_nanos();
        assert!(charged >= 3_000_000);
        // The executing thread is already caught up.
        assert_eq!(a.delay_point(), 0);
        // The other thread owes exactly the charged delay, once.
        assert_eq!(b.delay_point(), charged);
        assert_eq!(b.delay_point(), 0);
        assert_eq!(p.paused_nanos(), charged);
        // A non-selected region charges nobody.
        b.region(cold, || std::thread::sleep(Duration::from_millis(1)));
        assert_eq!(p.global_delay_nanos(), charged);
        // A thread created later starts caught up.
        assert_eq!(p.thread().delay_point(), 0);
        assert_eq!(p.region_visits(hot), 1);
        assert_eq!(p.region_visits(cold), 1);
        assert!(p.region("missing").is_err());
        assert!(CausalProfiler::new(&["x", "x"], &["p"]).is_err());
        assert!(CausalProfiler::new(&[], &["p"]).is_err());
    }
    #[test]
    fn retained_handles_neither_keep_credit_nor_debt_across_experiments() {
        let p = CausalProfiler::new(&["hot"], &["done"]).unwrap();
        let hot = p.region("hot").unwrap();
        let mut a = p.thread();
        let mut b = p.thread();
        p.reset(Some(hot), 100);
        // b keeps credit from executing the region; a is left owing.
        b.region(hot, || std::thread::sleep(Duration::from_millis(2)));
        p.reset(Some(hot), 100);
        assert_eq!(p.global_delay_nanos(), 0);
        assert_eq!(a.delay_point(), 0, "stale debt was not dropped");
        a.region(hot, || std::thread::sleep(Duration::from_millis(2)));
        let charged = p.global_delay_nanos();
        assert_eq!(b.delay_point(), charged, "stale credit skipped new delay");
    }
    #[test]
    fn a_failing_program_leaves_virtual_speedup_disabled() {
        let p = CausalProfiler::new(&["hot"], &["done"]).unwrap();
        let hot = p.region("hot").unwrap();
        let done = p.progress_point("done").unwrap();
        let mut calls = 0;
        let config = CausalConfig {
            speedups_pct: vec![50],
            repetitions: 1,
            progress_point: "done",
        };
        let result = profile(&p, &config, |p| {
            calls += 1;
            p.thread().progress(done);
            if calls == 2 {
                Err("selected experiment failed")
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(CausalError::Program(_))));
        p.thread()
            .region(hot, || std::thread::sleep(Duration::from_millis(1)));
        assert_eq!(p.global_delay_nanos(), 0);
    }
}
