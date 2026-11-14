//! Causal profiling on a synthetic program with a known bottleneck.
//!
//! Two threads do one round of work each, then meet at a barrier; the round
//! ends with a progress point. Thread "slow" works 8 ms per round and thread
//! "fast" 3 ms (wall-clock waits that yield, so the program behaves the same on
//! a single-CPU host), so a round lasts max(8, 3) ms. Truly speeding "slow" up by s
//! gives a round of max(8(1-s), 3) ms: 50% predicts 50%, 100% predicts
//! 1 - 3/8 = 62.5%. Speeding "fast" up changes nothing.
use std::{
    convert::Infallible,
    sync::{Arc, Barrier},
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

fn program(p: &CausalProfiler) -> Result<(), Infallible> {
    const ROUNDS: usize = 12;
    let barrier = Arc::new(Barrier::new(2));
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
    let a = worker(slow, Duration::from_millis(8), true);
    let b = worker(fast, Duration::from_millis(3), false);
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
    assert_eq!(report.baseline.progress, 12);
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
