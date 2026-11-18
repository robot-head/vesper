//! ABBA comparison of the Rust build (the seed package, baseline) and the
//! Vesper port (the port-backed package, candidate). The two builds are two
//! packages with one library name, so this example is built once per package
//! and, in `compare` mode, runs the two executables as separate processes in
//! Rust, port, port, Rust order per epoch, then analyzes the paired totals
//! with vesper-bench. Run `crates/vesper-crucible/bench-abba.sh`.
//!
//! `abba measure <ops>` warms each workload up and prints its total
//! nanoseconds over `<ops>` runs. `abba compare <rust> <port> <epochs> <ops>`
//! drives the two builds.
//!
//! Workloads, one run each:
//! - `draws`: 4,096 bounded draws from a random source.
//! - `tape-text`: a 4,096-choice tape to text and back.
//! - `shrink`: shrinking a 64-element "sum exceeds 1,000" counterexample.
//! - `fuzz`: guided fuzzing until the README's 32-bit magic guard fails,
//!   then shrinking the counterexample.
//! - `dpor`: DPOR over a random three-fiber program of the exploration
//!   tests (16 programs, by seed).
//! - `cached`: DPOR with state caching and a preemption bound of one over a
//!   random two-fiber program with spawns, cancellation and chosen I/O
//!   results.
#[path = "../tests/common/mod.rs"]
mod common;

use std::{process::Command, time::Instant};
use vesper_bench::{PairedSample, analyze_pairs};
use vesper_crucible::{
    ChoiceTape, ExploreConfig, FuzzConfig, PropError, Search, ShrinkConfig, Source, SplitMix64,
    explore_cached, explore_dpor, fuzz, shrink,
};

fn draws(seed: u64) -> u64 {
    let mut src = Source::random(seed);
    let mut acc = 0u64;
    for i in 0..4096u64 {
        acc ^= src.draw_u64_bounded(i | 255).unwrap();
    }
    acc
}

fn tape_text(seed: u64) -> u64 {
    let mut rng = SplitMix64(seed);
    let tape = ChoiceTape::new((0..4096).map(|_| rng.next_u64() >> (rng.0 & 63)).collect());
    let text = tape.to_text();
    let back = ChoiceTape::from_text(&text).unwrap();
    back.values.len() as u64 ^ text.len() as u64
}

fn sum_prop(src: &mut Source) -> Result<(), PropError> {
    let v = src.draw_vec(64, |s| s.draw_u64_bounded(100))?;
    if v.iter().sum::<u64>() > 1000 {
        Err(PropError::fail("sum", "exceeds 1000"))
    } else {
        Ok(())
    }
}

fn shrink_run(seed: u64) -> u64 {
    let mut rng = SplitMix64(seed);
    let mut tape = Vec::new();
    for _ in 0..64 {
        tape.push(1);
        tape.push(50 + rng.below_inclusive(50));
    }
    tape.push(0);
    let config = ShrinkConfig::default();
    let shrunk = shrink(&mut sum_prop, &ChoiceTape::new(tape), &config).unwrap();
    shrunk.stats.runs ^ shrunk.tape.len() as u64
}

fn fuzz_run(seed: u64) -> u64 {
    let config = FuzzConfig {
        seed,
        ..FuzzConfig::default()
    };
    let report = fuzz(&config, |src: &mut Source| {
        let data = src.draw_bytes(64)?;
        if data.len() >= 4 {
            let word = u32::from_le_bytes(data[..4].try_into().unwrap());
            if src.probe().eq(1, u64::from(word), 0xfeed_f00d) {
                return Err(PropError::fail("magic", "reached"));
            }
        }
        Ok(())
    });
    report.execs ^ report.counterexample.map_or(0, |cx| cx.tape.len() as u64)
}

fn search_config(bound: Option<u32>) -> ExploreConfig {
    ExploreConfig {
        max_executions: 3_000,
        preemption_bound: bound,
        ..ExploreConfig::default()
    }
}

fn dpor_run(seed: u64) -> u64 {
    let program = common::random_program(seed % 16, 3, 3);
    let observe = |w: &common::Cells| Ok(w.clone());
    explore_dpor(&search_config(None), || common::build(&program), observe)
        .map_or(0, |e| e.executions ^ e.transitions)
}

fn cached_run(seed: u64) -> u64 {
    let program = common::random_program_ext(seed % 16, 2, 3);
    let observe = |w: &common::Cells| Ok(w.clone());
    explore_cached(
        Search::Dpor,
        &search_config(Some(1)),
        || common::build(&program),
        observe,
        common::cells_key,
    )
    .map_or(0, |e| e.executions ^ e.transitions)
}

type Workload = (&'static str, fn(u64) -> u64);

const WORKLOADS: [Workload; 6] = [
    ("draws", draws),
    ("tape-text", tape_text),
    ("shrink", shrink_run),
    ("fuzz", fuzz_run),
    ("dpor", dpor_run),
    ("cached", cached_run),
];

/// Total nanoseconds for `ops` timed runs after `ops / 4` warm-up ones.
fn measure(work: fn(u64) -> u64, ops: u64) -> u128 {
    let mut sink = 0;
    for seed in 0..ops / 4 {
        sink ^= work(seed);
    }
    let start = Instant::now();
    for seed in 0..ops {
        sink ^= work(seed);
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
    for ((name, _), pairs) in WORKLOADS.iter().zip(&pairs) {
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
            for (name, work) in WORKLOADS {
                println!("{name} {}", measure(work, ops));
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
