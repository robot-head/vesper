//! ABBA benchmark of the Vesper hot-set decoder and run builder against
//! the Rust ones: decode a 1,000-page hot set, then coalesce it into
//! prefetch runs. Ignored by default; run it with
//!
//! ```text
//! cargo test --release -p vesper-aether --features vesper-port \
//!     --test abba_hotset -- --ignored --nocapture
//! ```
#![cfg(feature = "vesper-port")]

use std::hint::black_box;

use vesper_aether::hotset::{HotSet, PREFETCH_BATCH_PAGES};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

const PAGES: usize = 1 << 16;

fn encoded() -> Vec<u8> {
    // Clusters of 40 pages every 300 pages.
    let indices: Vec<usize> = (0..1000).map(|i| (i / 40) * 300 + i % 40).collect();
    HotSet::new(PAGES, &indices).unwrap().encode()
}

type Work = fn(&[u8]) -> u64;

fn rust_workload(e: &[u8]) -> u64 {
    let h = HotSet::decode_reference(e, PAGES).unwrap();
    h.runs_reference(PREFETCH_BATCH_PAGES).unwrap().len() as u64 + h.len() as u64
}

fn vesper_workload(e: &[u8]) -> u64 {
    let h = HotSet::decode(e, PAGES).unwrap();
    h.runs(PREFETCH_BATCH_PAGES).unwrap().len() as u64 + h.len() as u64
}

struct Arm(Work, Vec<u8>);

fn arm(work: Work) -> Arm {
    Arm(work, encoded())
}

impl BenchCase for Arm {
    type Input = ();
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<(), ()> {
        Ok(())
    }
    fn run(&mut self, (): ()) -> Result<u64, ()> {
        let mut sum = 0u64;
        for _ in 0..16 {
            sum = sum.wrapping_add((self.0)(black_box(&self.1)));
        }
        Ok(sum)
    }
}

#[test]
fn both_builds_compute_the_same_workload() {
    let e = encoded();
    assert_eq!(rust_workload(&e), vesper_workload(&e));
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_hotset() {
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report =
        run_abba(run, &mut arm(rust_workload), &mut arm(vesper_workload)).expect("ABBA run");
    let pairs = report.pairs();
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    println!(
        "Vesper/Rust ratio median {:.3} (min {:.3}, max {:.3})",
        ratios[pairs.len() / 2],
        ratios[0],
        ratios[ratios.len() - 1]
    );
    // The flip bar: the upper end of the 95% interval at or below 1.5x.
    let analysis = analyze_pairs(pairs, 5_000).expect("analysis");
    println!(
        "1.5x gate (max regression 5000 bp): {:?}",
        analysis.decision
    );
    if let Some(i) = analysis.interval {
        println!(
            "95% median interval of Vesper/Rust: [{:.3}, {:.3}]",
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64
        );
    }
}
