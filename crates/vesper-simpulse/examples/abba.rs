//! ABBA comparison of the Rust build (the seed package, baseline) and the
//! Vesper port (the port-backed package, candidate). The two builds are two
//! packages with one library name, so this example is built once per package
//! and, in `compare` mode, runs the two executables as separate processes in
//! Rust, port, port, Rust order per epoch, then analyzes the paired totals
//! with vesper-bench. Run `aspect crate simpulse bench-abba`.
//!
//! `abba measure <ops>` warms each workload up and prints its total
//! nanoseconds over `<ops>` runs. `abba compare <rust> <port> <epochs> <ops>`
//! drives the two builds.
//!
//! Workloads (workload/mod.rs), one run each, recording a journal:
//! - `mixed-8`: 8 tasks, 2,000 dispatches over every transition.
//! - `mixed-256`: 256 tasks, 4,000 dispatches (the O(tasks) run-queue scan).
//! - `yield-64`: 64 tasks that only yield, 4,000 dispatches.
//! - `timers-32`: 32 tasks that only sleep, 4,000 dispatches.
use std::{process::Command, time::Instant};
use vesper_bench::{PairedSample, analyze_pairs};
use vesper_simpulse::Simulator;

#[path = "../workload/mod.rs"]
mod workload;
use workload::{Digest, MIXED, Mix, drive};

const YIELD: Mix = Mix {
    yield_task: 100,
    sleep: 0,
    schedule_io: 0,
    park_io: 0,
    side: 0,
    delay: 1,
};
const TIMERS: Mix = Mix {
    yield_task: 0,
    sleep: 100,
    schedule_io: 0,
    park_io: 0,
    side: 0,
    delay: 10_000,
};

const WORKLOADS: [(&str, u64, u64, Mix); 4] = [
    ("mixed-8", 8, 2_000, MIXED),
    ("mixed-256", 256, 4_000, MIXED),
    ("yield-64", 64, 4_000, YIELD),
    ("timers-32", 32, 4_000, TIMERS),
];

/// Total nanoseconds for `ops` timed runs after `ops / 4` warm-up ones.
fn measure(tasks: u64, dispatches: u64, mix: Mix, ops: u64) -> u128 {
    let mut sink = 0;
    let mut once = |seed: u64| {
        let mut sim = Simulator::new(seed);
        let mut d = Digest::off();
        sink ^= drive(&mut sim, seed, tasks, dispatches, mix, &mut d) ^ d.0;
        sink ^= sim.journal().events.len() as u64;
    };
    for seed in 0..ops / 4 {
        once(seed);
    }
    let start = Instant::now();
    for seed in 0..ops {
        once(seed);
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed
}

fn run(exe: &str, ops: u64) -> Vec<u64> {
    let out = Command::new(exe)
        .args(["measure", &ops.to_string()])
        .output()
        .expect("run a build");
    assert!(out.status.success(), "{exe} failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.split_whitespace().nth(1).unwrap().parse().unwrap())
        .collect()
}

fn compare(rust: &str, port: &str, epochs: usize, ops: u64) {
    let mut pairs = vec![Vec::new(); WORKLOADS.len()];
    for _ in 0..epochs {
        let a1 = run(rust, ops);
        let b1 = run(port, ops);
        let b2 = run(port, ops);
        let a2 = run(rust, ops);
        for (w, pair) in pairs.iter_mut().enumerate() {
            pair.push(PairedSample::new(a1[w] + a2[w], b1[w] + b2[w], 2 * ops).unwrap());
        }
    }
    for ((name, ..), pairs) in WORKLOADS.iter().zip(&pairs) {
        let per_op = |t: u64| t as f64 / (2 * ops) as f64;
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let rust_ns = median(pairs.iter().map(|p| per_op(p.baseline_total())).collect());
        let port_ns = median(pairs.iter().map(|p| per_op(p.candidate_total())).collect());
        let ratios: Vec<f64> = pairs
            .iter()
            .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
            .collect();
        let lo = ratios.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = ratios.iter().copied().fold(0.0, f64::max);
        let ratio = median(ratios);
        let analysis = analyze_pairs(pairs, 0).unwrap();
        let bp = |s: PairedSample| s.regression_bp_bounds().floor as f64 / 100.0;
        println!(
            "{name:<10} rust {rust_ns:>10.0} ns/run  port {port_ns:>10.0} ns/run  port/rust {ratio:.2}x \
             (range {lo:.2}x..{hi:.2}x, sample median regression {:+.0}%..{:+.0}%, {epochs} epochs)",
            bp(analysis.median_lower),
            bp(analysis.median_upper),
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["measure", ops] => {
            let ops = ops.parse().unwrap();
            for (name, tasks, dispatches, mix) in WORKLOADS {
                println!("{name} {}", measure(tasks, dispatches, mix, ops));
            }
        }
        ["compare", rust, port, epochs, ops] => {
            compare(rust, port, epochs.parse().unwrap(), ops.parse().unwrap())
        }
        _ => {
            eprintln!("usage: abba measure <ops> | abba compare <rust> <port> <epochs> <ops>");
            std::process::exit(2);
        }
    }
}
