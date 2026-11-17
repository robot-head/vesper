//! ABBA benchmark of the Vesper build against the Rust build of [`Wheel`]
//! (insert, advance/poll and drain), with `vesper-bench`. Both builds are in
//! one process: under `vesper-port` the crate also compiles the frozen Rust
//! wheel as `wheel::rust_reference`. Ignored by default; run it with
//!
//! ```text
//! cargo test --release -p vesper-warp-wheel --features vesper-port \
//!     --test abba -- --ignored --nocapture
//! ```
#![cfg(feature = "vesper-port")]

use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_warp_wheel::WheelConfig;
use vesper_warp_wheel::wheel::rust_reference;

const CAPACITY: u32 = 4096;

fn config() -> WheelConfig {
    WheelConfig {
        tick_ns: 1_000,
        slot_bits: 6,
        levels: 4,
        capacity: CAPACITY,
        start_ns: 0,
    }
}

/// One operation: fill the wheel with 4096 entries at pseudo-random near,
/// mid and far deadlines, advance in 64 steps polling everything due, then
/// drain what is left. Returns a checksum of what fired.
macro_rules! workload {
    ($wheel:expr) => {{
        let wheel = &mut $wheel;
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut sum: u64 = 0;
        let base = wheel.now_ns();
        for i in 0..u64::from(CAPACITY) {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let delta = match (x >> 33) % 4 {
                0 => (x >> 20) % 5_000,
                1 => (x >> 20) % 500_000,
                2 => (x >> 20) % 50_000_000,
                _ => (x >> 20) % 50_000_000_000,
            };
            let _ = wheel.insert(base + delta, i).unwrap();
        }
        for step in 1..=64u64 {
            while let Some(e) = wheel.poll(base + step * 1_000_000).unwrap() {
                sum = sum.wrapping_add(e.deadline_ns ^ e.payload);
            }
        }
        wheel.drain(|d, p| sum = sum.wrapping_add(d ^ p));
        black_box(sum)
    }};
}

struct RustWheel;
struct VesperWheel;

impl BenchCase for RustWheel {
    type Input = rust_reference::Wheel<u64>;
    type Output = (rust_reference::Wheel<u64>, u64);
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<Self::Input, ()> {
        rust_reference::Wheel::new(config()).map_err(|_| ())
    }
    fn run(&mut self, mut wheel: Self::Input) -> Result<Self::Output, ()> {
        let sum = workload!(wheel);
        Ok((wheel, sum))
    }
}

impl BenchCase for VesperWheel {
    type Input = vesper_warp_wheel::Wheel<u64>;
    type Output = (vesper_warp_wheel::Wheel<u64>, u64);
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<Self::Input, ()> {
        vesper_warp_wheel::Wheel::new(config()).map_err(|_| ())
    }
    fn run(&mut self, mut wheel: Self::Input) -> Result<Self::Output, ()> {
        let sum = workload!(wheel);
        Ok((wheel, sum))
    }
}

#[test]
fn both_builds_compute_the_same_workload() {
    let mut rust = rust_reference::Wheel::<u64>::new(config()).unwrap();
    let mut vesper = vesper_warp_wheel::Wheel::<u64>::new(config()).unwrap();
    assert_eq!(workload!(rust), workload!(vesper));
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_insert_advance_drain() {
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report = run_abba(run, &mut RustWheel, &mut VesperWheel).expect("ABBA run");
    let pairs = report.pairs();
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let per_op = |total: u64, p: &vesper_bench::PairedSample| {
        total as f64 / p.operations_per_arm() as f64 / 1e3
    };
    let mut rust_us: Vec<f64> = pairs
        .iter()
        .map(|p| per_op(p.baseline_total(), p))
        .collect();
    let mut vesper_us: Vec<f64> = pairs
        .iter()
        .map(|p| per_op(p.candidate_total(), p))
        .collect();
    rust_us.sort_by(f64::total_cmp);
    vesper_us.sort_by(f64::total_cmp);
    let mid = pairs.len() / 2;
    println!(
        "ABBA insert/advance/drain, {} epochs x {} ops per arm ({} entries per op)",
        pairs.len(),
        pairs[0].operations_per_arm(),
        CAPACITY
    );
    println!(
        "median per op: rust {:.1} us, vesper {:.1} us; Vesper/Rust ratio median {:.3} (min {:.3}, max {:.3})",
        rust_us[mid],
        vesper_us[mid],
        ratios[mid],
        ratios[0],
        ratios[ratios.len() - 1]
    );
    // The lane's deletion gate: the port may not be slower than 2x.
    let analysis = analyze_pairs(pairs, 10_000).expect("analysis");
    println!("2x gate (max regression 10000 bp): {:?}", analysis.decision);
    if let Some(i) = analysis.interval {
        println!(
            "95% median interval of Vesper/Rust: [{:.3}, {:.3}]",
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64
        );
    }
}
