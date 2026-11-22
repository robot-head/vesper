//! ABBA benchmark of the Vesper PVH ELF parser against the Rust one: parse
//! the test kernel image and every truncation of it. Ignored by default;
//! run it with
//!
//! ```text
//! cargo test --release -p vesper-aether --features vesper-port \
//!     --test abba_pvh -- --ignored --nocapture
//! ```
#![cfg(feature = "vesper-port")]

use std::hint::black_box;

use vesper_aether::guest_mem::GuestMemory;
use vesper_aether::pvh::{build_test_pvh_elf, parse_pvh_elf, parse_pvh_elf_reference};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

const CODE: [u8; 10] = [0x8B, 0x03, 0xE6, 0x10, 0x8B, 0x43, 0x04, 0xE6, 0x10, 0xF4];

fn mem() -> GuestMemory {
    GuestMemory::new(&[(0, 2 << 20), (0x1000_0000, 1 << 20)]).unwrap()
}

type Parse = fn(&[u8], &GuestMemory) -> u64;

fn rust_workload(elf: &[u8], m: &GuestMemory) -> u64 {
    parse_pvh_elf_reference(elf, m).map_or(0, |i| u64::from(i.entry))
}

fn vesper_workload(elf: &[u8], m: &GuestMemory) -> u64 {
    parse_pvh_elf(elf, m).map_or(0, |i| u64::from(i.entry))
}

struct Arm(Parse, GuestMemory, Vec<u8>);

fn arm(parse: Parse) -> Arm {
    Arm(parse, mem(), build_test_pvh_elf(0x10_0000, &CODE))
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
        for n in 0..=self.2.len() {
            sum = sum.wrapping_add((self.0)(black_box(&self.2[..n]), &self.1));
        }
        Ok(sum)
    }
}

#[test]
fn both_builds_compute_the_same_workload() {
    let m = mem();
    let elf = build_test_pvh_elf(0x10_0000, &CODE);
    for n in 0..=elf.len() {
        assert_eq!(rust_workload(&elf[..n], &m), vesper_workload(&elf[..n], &m));
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_pvh_parse() {
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
