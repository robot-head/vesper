//! Causal profiling on a synthetic program with a known bottleneck.
//!
//! Two threads do one round of work each, then meet at a barrier; the round
//! ends with a progress point. Thread "slow" works 40 ms per round and thread
//! "fast" 15 ms (wall-clock waits that yield, so the program behaves the same
//! on a single-CPU host), so a round lasts max(40, 15) ms. Truly speeding
//! "slow" up by s gives a round of max(40(1-s), 15) ms: 50% predicts 50%,
//! 100% predicts 1 - 15/40 = 62.5%. Speeding "fast" up changes nothing.
//!
//! Those figures hold only when the round has no other cost. Any per-round
//! time outside both regions is not sped up and dilutes the prediction, as it
//! should in a causal profile. On macOS CI each round carried about 6 ms of
//! such cost (late returns from `yield_now` and barrier wakeups: rounds meant
//! to last 8 ms took about 15 ms, and the curve read 2271 bp at 50% and
//! 2211 bp at 100%). Rounds are therefore long enough that a fixed cost of
//! up to 10 ms still leaves the predictions inside the bounds below
//! (1 - 30/50 = 40% and 1 - 25/50 = 50%), and the barrier spins briefly
//! before it yields so that a release on another core is seen at once.
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use vesper_bench::causal::{CausalConfig, CausalProfiler, profile};

fn work(d: Duration) {
    // Wall-clock work that yields the CPU on every check. Pure spinning
    // cannot overlap two threads on a single-CPU host, and sleeping is too
    // coarse on macOS CI (rounds meant to last 8 ms took about 45 ms), which
    // buries the bottleneck structure under timer noise. Yielding threads
    // share one CPU and still finish at their wall-clock deadlines.
    let start = Instant::now();
    while start.elapsed() < d {
        thread::yield_now();
    }
}

/// A reusable barrier that waits by yielding instead of parking, so the last
/// arrival releases the others without a wakeup delay.
struct SpinBarrier {
    parties: usize,
    arrived: AtomicUsize,
    generation: AtomicUsize,
}
impl SpinBarrier {
    fn new(parties: usize) -> Self {
        Self {
            parties,
            arrived: AtomicUsize::new(0),
            generation: AtomicUsize::new(0),
        }
    }
    fn wait(&self) {
        let generation = self.generation.load(Ordering::Acquire);
        if self.arrived.fetch_add(1, Ordering::AcqRel) + 1 == self.parties {
            self.arrived.store(0, Ordering::Release);
            self.generation.fetch_add(1, Ordering::AcqRel);
            return;
        }
        let mut spins = 0u32;
        while self.generation.load(Ordering::Acquire) == generation {
            // Spin first: on a multi-core host the release lands within
            // microseconds. Then yield, so a single-CPU host still lets the
            // last arrival run.
            if spins < 10_000 {
                spins += 1;
                std::hint::spin_loop();
            } else {
                thread::yield_now();
            }
        }
    }
}

fn program(p: &CausalProfiler) -> Result<(), Infallible> {
    const ROUNDS: usize = 6;
    let barrier = Arc::new(SpinBarrier::new(2));
    let slow = p.region("slow").unwrap();
    let fast = p.region("fast").unwrap();
    let done = p.progress_point("round").unwrap();
    let worker = |region, amount, report: bool| {
        let barrier = Arc::clone(&barrier);
        let mut t = p.thread();
        thread::spawn(move || {
            for _ in 0..ROUNDS {
                t.region(region, || work(amount));
                barrier.wait();
                if report {
                    t.progress(done);
                }
            }
        })
    };
    let a = worker(slow, Duration::from_millis(40), true);
    let b = worker(fast, Duration::from_millis(15), false);
    a.join().unwrap();
    b.join().unwrap();
    Ok(())
}

#[test]
fn causal_profile_finds_the_known_bottleneck() {
    let profiler = CausalProfiler::new(&["slow", "fast"], &["round"]).unwrap();
    let config = CausalConfig {
        speedups_pct: vec![50, 100],
        repetitions: 5,
        progress_point: "round",
    };
    let report = profile(&profiler, &config, program).unwrap();
    assert_eq!(report.baseline.progress, 6);
    eprintln!(
        "baseline round {} us",
        report.baseline.effective_ns / report.baseline.progress / 1000
    );
    let bp = |region: &str, pct: u32| {
        report
            .curve(region)
            .into_iter()
            .find(|p| p.speedup_pct == pct)
            .unwrap()
            .predicted_speedup_bp
    };
    for p in &report.points {
        eprintln!(
            "{} +{}% -> {} bp",
            p.region, p.speedup_pct, p.predicted_speedup_bp
        );
    }
    // Expected 5000 and 6250 bp; generous bounds absorb scheduler noise.
    assert!(
        (3000..=7000).contains(&bp("slow", 50)),
        "{}",
        bp("slow", 50)
    );
    assert!(
        (4000..=8000).contains(&bp("slow", 100)),
        "{}",
        bp("slow", 100)
    );
    // Expected 0 bp for the off-critical-path thread.
    assert!(
        (-1500..=1500).contains(&bp("fast", 50)),
        "{}",
        bp("fast", 50)
    );
    assert!(
        (-1500..=1500).contains(&bp("fast", 100)),
        "{}",
        bp("fast", 100)
    );
    assert!(bp("slow", 50) > bp("fast", 100) + 2000);
}

#[test]
fn invalid_experiments_are_typed_errors() {
    let profiler = CausalProfiler::new(&["slow", "fast"], &["round"]).unwrap();
    for (speedups, reps, point) in [
        (vec![], 1, "round"),
        (vec![0], 1, "round"),
        (vec![101], 1, "round"),
        (vec![50], 0, "round"),
        (vec![50], 16, "round"),
        (vec![50], 1, "missing"),
    ] {
        let config = CausalConfig {
            speedups_pct: speedups,
            repetitions: reps,
            progress_point: point,
        };
        assert!(profile(&profiler, &config, |_| Ok::<(), Infallible>(())).is_err());
    }
    // A program that never reaches its progress point yields no prediction.
    let config = CausalConfig {
        speedups_pct: vec![50],
        repetitions: 1,
        progress_point: "round",
    };
    assert!(profile(&profiler, &config, |_| Ok::<(), Infallible>(())).is_err());
}
