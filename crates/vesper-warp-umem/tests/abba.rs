//! ABBA benchmark of the Vesper build against the Rust build of the software
//! NIC and its driver: the echo loop of tests/zero_alloc.rs (step, harvest,
//! turn around, submit, retire, complete, replenish) with an exact audit
//! after every round. Both builds are in one process: under `vesper-port`
//! the crate also compiles the Rust implementation as `rust_reference`.
//! Ignored by default; run it with
//!
//! ```text
//! cargo test --release -p vesper-warp-umem --features vesper-port \
//!     --test abba -- --ignored --nocapture
//! ```
#![cfg(feature = "vesper-port")]

use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_warp_umem::UmemConfig;
use vesper_warp_umem::rust_reference;

const ROUNDS: u64 = 64;

fn config() -> UmemConfig {
    UmemConfig {
        frame_count: 64,
        frame_size: 2048,
        headroom: 128,
        fill_size: 64,
        rx_size: 32,
        tx_size: 32,
        completion_size: 32,
    }
}

/// One operation: a fresh device and driver, four seeded packets, then
/// `ROUNDS` rounds of the echo loop with an audit after each. Returns a
/// checksum of the packets echoed and the audit's counts.
macro_rules! workload {
    ($soft:path, $acc:path, $nic:path) => {{
        use $nic as nic;
        let mut xsk = <$soft>::xsk(config(), <nic::SoftNicMode>::Loopback).unwrap();
        xsk.replenish_fill(48);
        for _ in 0..4 {
            xsk.backend_mut().deliver(&[0x5a; 200]);
        }
        let mut auditor = <$acc>::new();
        let mut leases: Vec<Option<vesper_warp_umem::InFlightQuicPkt>> =
            (0..8).map(|_| None).collect();
        let mut pn = 0u64;
        let mut sum = 0u64;
        for _ in 0..ROUNDS {
            xsk.backend_mut().step();
            while let Some(rx) = xsk.harvest_rx().expect("well-formed") {
                let len = rx.pkt_len();
                sum = sum.wrapping_add(u64::from(xsk.view_frame_bytes(&rx).unwrap()[0]));
                let mut tx = xsk.turn_around_tx_in_place(rx, len).expect("in place");
                xsk.tx_bytes_mut(&mut tx).unwrap()[0] ^= 1;
                let lease = xsk.submit_paced_tx(tx, pn, pn * 1000).expect("tx space");
                if let Some(old) = leases[(pn % 8) as usize].replace(lease) {
                    xsk.retire(old).unwrap();
                }
                pn += 1;
            }
            xsk.harvest_completions(64);
            xsk.replenish_fill(64);
            let acc = auditor.audit(&mut xsk).expect("conserved");
            sum = sum.wrapping_add(acc.sum());
        }
        for lease in leases.into_iter().flatten() {
            xsk.retire(lease).unwrap();
        }
        black_box(sum.wrapping_add(pn))
    }};
}

fn rust_workload() -> u64 {
    use rust_reference::driver::XdpNic as _;
    workload!(
        rust_reference::soft::SoftNic,
        rust_reference::accounting::UmemAuditor,
        rust_reference::soft
    )
}

fn vesper_workload() -> u64 {
    use vesper_warp_umem::XdpNic as _;
    workload!(
        vesper_warp_umem::SoftNic,
        vesper_warp_umem::UmemAuditor,
        vesper_warp_umem
    )
}

struct RustEcho;
struct VesperEcho;

impl BenchCase for RustEcho {
    type Input = ();
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<(), ()> {
        Ok(())
    }
    fn run(&mut self, (): ()) -> Result<u64, ()> {
        Ok(rust_workload())
    }
}

impl BenchCase for VesperEcho {
    type Input = ();
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<(), ()> {
        Ok(())
    }
    fn run(&mut self, (): ()) -> Result<u64, ()> {
        Ok(vesper_workload())
    }
}

#[test]
fn both_builds_compute_the_same_workload() {
    assert_eq!(rust_workload(), vesper_workload());
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_echo_loop() {
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report = run_abba(run, &mut RustEcho, &mut VesperEcho).expect("ABBA run");
    let pairs = report.pairs();
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let mid = pairs.len() / 2;
    println!(
        "ABBA echo loop, {} epochs x {} ops per arm ({ROUNDS} rounds per op)",
        pairs.len(),
        pairs[0].operations_per_arm(),
    );
    println!(
        "Vesper/Rust ratio median {:.3} (min {:.3}, max {:.3})",
        ratios[mid],
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
