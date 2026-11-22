//! ABBA benchmark of the Vesper slot-plan verifier against the Rust one:
//! verify a 32-slot plan, then the cross-VM check over four plans. Ignored
//! by default; run it with
//!
//! ```text
//! cargo test --release -p vesper-aether --features vesper-port \
//!     --test abba_slot_plan -- --ignored --nocapture
//! ```
#![cfg(feature = "vesper-port")]

use std::hint::black_box;

use vesper_aether::slot_plan::{
    KVM_MEM_READONLY, KvmHostLimits, KvmSlotPlan, PlannedRegion, VerifiedKvmSlotPlan,
    verify_cross_vm, verify_cross_vm_reference,
};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

const SLOTS: u32 = 32;
const VMS: u64 = 4;

fn plan(vm: u64) -> KvmSlotPlan {
    // Slots in a shuffled order so the sorts do real work; slot 0 of every
    // VM shares one read-only page, the rest are private.
    let regions = (0..SLOTS)
        .map(|i| {
            let s = (i * 13) % SLOTS;
            let shared = s == 0;
            PlannedRegion {
                slot: s,
                flags: if shared { KVM_MEM_READONLY } else { 0 },
                guest_phys_addr: u64::from(s) * 0x20_0000,
                memory_size: 0x1000 * (1 + u64::from(s % 3)),
                userspace_addr: if shared {
                    0x1000_0000
                } else {
                    0x2000_0000 + (vm * 64 + u64::from(s)) * 0x10_0000
                },
            }
        })
        .collect();
    KvmSlotPlan::from_regions(vm, regions).unwrap()
}

fn plans() -> Vec<KvmSlotPlan> {
    (0..VMS).map(plan).collect()
}

fn workload(
    plans: &[KvmSlotPlan],
    verify: fn(KvmSlotPlan) -> VerifiedKvmSlotPlan,
    cross: fn(&[&VerifiedKvmSlotPlan]) -> bool,
) -> u64 {
    let verified: Vec<VerifiedKvmSlotPlan> = plans.iter().cloned().map(verify).collect();
    let refs: Vec<&VerifiedKvmSlotPlan> = verified.iter().collect();
    black_box(verified.len() as u64 + u64::from(cross(&refs)))
}

fn rust_workload(plans: &[KvmSlotPlan]) -> u64 {
    workload(
        plans,
        |p| p.verify_reference(&KvmHostLimits::default()).unwrap(),
        |r| verify_cross_vm_reference(r).is_ok(),
    )
}

fn vesper_workload(plans: &[KvmSlotPlan]) -> u64 {
    workload(
        plans,
        |p| p.verify(&KvmHostLimits::default()).unwrap(),
        |r| verify_cross_vm(r).is_ok(),
    )
}

struct Arm(fn(&[KvmSlotPlan]) -> u64, Vec<KvmSlotPlan>);

impl BenchCase for Arm {
    type Input = ();
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<(), ()> {
        Ok(())
    }
    fn run(&mut self, (): ()) -> Result<u64, ()> {
        let mut sum = 0;
        for _ in 0..64 {
            sum += (self.0)(&self.1);
        }
        Ok(sum)
    }
}

#[test]
fn both_builds_accept_the_workload() {
    let p = plans();
    assert_eq!(rust_workload(&p), VMS + 1);
    assert_eq!(vesper_workload(&p), VMS + 1);
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_slot_plan() {
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report = run_abba(
        run,
        &mut Arm(rust_workload, plans()),
        &mut Arm(vesper_workload, plans()),
    )
    .expect("ABBA run");
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
