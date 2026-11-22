//! ABBA benchmark of the Vesper packed-ring capture against the Rust one:
//! fill a 256-entry ring with two-descriptor chains, then capture and
//! complete every chain, twice around the ring. Ignored by default; run it
//! with
//!
//! ```text
//! cargo test --release -p vesper-aether --features vesper-port \
//!     --test abba_packed -- --ignored --nocapture
//! ```
#![cfg(feature = "vesper-port")]

use std::hint::black_box;

use vesper_aether::virtio::*;
use vesper_aether::{Limits, MemorySlot, PageMapping, Permission, SlotSnapshot, SnapshotMap};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

const SIZE: usize = 256;

fn authorization() -> GuestAuthorization {
    let map = SnapshotMap::validate(
        &[SlotSnapshot {
            slot: MemorySlot {
                vm_id: 7,
                slot_id: 1,
                guest_start: 0x1000,
                host_virtual_start: 0x10000,
                byte_len: 0x4000,
                permission: Permission::Private,
            },
            pages: vec![PageMapping::Zero; 4],
        }],
        Limits::default(),
    )
    .unwrap();
    let ranges: Vec<AuthorizedRange> = (0..4)
        .map(|i| AuthorizedRange {
            guest_start: 0x1000 + 0x1000 * i,
            byte_len: 0x1000,
            access: if i == 0 {
                DeviceAccess::Read
            } else {
                DeviceAccess::ReadWrite
            },
        })
        .collect();
    GuestAuthorization::new(&map, 7, &ranges, QueueLimits::default()).unwrap()
}

/// Posts the chains of one lap of the ring in phase `wrap`.
fn post(ring: &mut [u8], wrap: bool) {
    let phase = if wrap { FLAG_AVAIL } else { FLAG_USED };
    for i in 0..SIZE {
        let (gpa, flags) = if i % 2 == 0 {
            (0x1000 + 8 * i as u64, FLAG_NEXT)
        } else {
            (0x2000 + 8 * i as u64, FLAG_WRITE)
        };
        let d = PackedDescriptor {
            guest_address: gpa,
            byte_len: 64,
            buffer_id: (i / 2) as u16,
            flags: flags | phase,
        };
        ring[16 * i..16 * i + 16].copy_from_slice(&d.encode());
    }
}

type Capture = fn(&mut PackedQueue, &[u8]) -> Result<DescriptorLease, QueueError>;

fn lap(capture: Capture, q: &mut PackedQueue, ring: &mut [u8]) -> u64 {
    let mut sum = 0u64;
    for _ in 0..2 {
        let wrap = q.state().next_available.wrap;
        post(ring, wrap);
        for _ in 0..SIZE / 2 {
            let lease = capture(q, ring).unwrap();
            sum = sum.wrapping_add(lease.chain().writable_bytes());
            q.complete(ring, lease, 32).unwrap();
        }
    }
    sum
}

fn rust_workload(q: &mut PackedQueue, ring: &mut [u8]) -> u64 {
    lap(PackedQueue::capture_reference, q, ring)
}

fn vesper_workload(q: &mut PackedQueue, ring: &mut [u8]) -> u64 {
    lap(PackedQueue::capture, q, ring)
}

type Work = fn(&mut PackedQueue, &mut [u8]) -> u64;

struct Arm(Work, PackedQueue, Vec<u8>);

fn arm(work: Work) -> Arm {
    let q = PackedQueue::new(SIZE, &authorization(), QueueLimits::default()).unwrap();
    Arm(work, q, vec![0; 16 * SIZE])
}

impl BenchCase for Arm {
    type Input = ();
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<(), ()> {
        Ok(())
    }
    fn run(&mut self, (): ()) -> Result<u64, ()> {
        Ok((self.0)(&mut self.1, black_box(&mut self.2)))
    }
}

#[test]
fn both_builds_compute_the_same_workload() {
    let (mut a, mut b) = (arm(rust_workload), arm(vesper_workload));
    for _ in 0..3 {
        assert_eq!(
            rust_workload(&mut a.1, &mut a.2),
            vesper_workload(&mut b.1, &mut b.2)
        );
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_packed_capture() {
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
