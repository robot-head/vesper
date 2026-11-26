//! ABBA benchmark of the Vesper virtqueue walks against the Rust ones:
//! on a 256-entry split ring and a 256-entry packed ring, post block-style
//! chains (a 16-byte header, a 512-byte data buffer, a status byte), then
//! capture, complete and flush every chain. Ignored by default; run it
//! with
//!
//! ```text
//! cargo test --release -p vesper-aether --lib abba_virtqueue -- --ignored --nocapture
//! ```
//!
//! It prints the flip gate's `flip-gate pairs` line
//! (crates/vesper-aether/flip-gate.json).

use std::hint::black_box;

use crate::devices::queue::*;
use crate::guest_mem::GuestMemory;
use crate::virtio::{FLAG_AVAIL, FLAG_NEXT, FLAG_USED, FLAG_WRITE, QueueError, QueueLimits};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

const SIZE: u16 = 256;
const DESC: u64 = 0x1_0000;
const DRIVER: u64 = 0x2_0000;
const DEVICE: u64 = 0x3_0000;
/// Chains per lap: three descriptors each.
const CHAINS: u16 = SIZE / 4;

fn put(
    m: &mut GuestMemory,
    format: RingFormat,
    slot: u16,
    gpa: u64,
    len: u32,
    flags: u16,
    id: u16,
) {
    let mut d = [0u8; 16];
    d[0..8].copy_from_slice(&gpa.to_le_bytes());
    d[8..12].copy_from_slice(&len.to_le_bytes());
    match format {
        RingFormat::Split => {
            d[12..14].copy_from_slice(&flags.to_le_bytes());
            d[14..16].copy_from_slice(&id.to_le_bytes());
        }
        RingFormat::Packed => {
            d[12..14].copy_from_slice(&id.to_le_bytes());
            d[14..16].copy_from_slice(&flags.to_le_bytes());
        }
    }
    m.write(DESC + 16 * u64::from(slot), &d).unwrap();
}

struct Ring {
    m: GuestMemory,
    q: VirtQueue,
    format: RingFormat,
    /// Split: the driver's avail index. Packed: the driver's (index, wrap).
    avail: u16,
    next: (u16, bool),
}

fn ring(format: RingFormat) -> Ring {
    let m = GuestMemory::new(&[(0, 1 << 20)]).unwrap();
    let layout = RingLayout {
        format,
        size: SIZE,
        desc: DESC,
        driver: DRIVER,
        device: DEVICE,
    };
    let q = VirtQueue::new(layout, RingFeatures::default(), QueueLimits::default(), &m).unwrap();
    Ring {
        m,
        q,
        format,
        avail: 0,
        next: (0, true),
    }
}

/// Posts `CHAINS` three-descriptor chains.
fn post(r: &mut Ring) {
    for c in 0..CHAINS {
        let data = 0x4_0000 + 0x400 * u64::from(c);
        let parts = [
            (data, 16, FLAG_NEXT),
            (data + 16, 512, FLAG_NEXT | FLAG_WRITE),
            (data + 528, 1, FLAG_WRITE),
        ];
        match r.format {
            RingFormat::Split => {
                let head = 3 * c;
                for (k, &(gpa, len, flags)) in parts.iter().enumerate() {
                    let i = head + k as u16;
                    put(&mut r.m, r.format, i, gpa, len, flags, i + 1);
                }
                let slot = u64::from(r.avail % SIZE);
                r.m.write(DRIVER + 4 + 2 * slot, &head.to_le_bytes())
                    .unwrap();
                r.avail = r.avail.wrapping_add(1);
            }
            RingFormat::Packed => {
                for &(gpa, len, flags) in &parts {
                    let (i, wrap) = r.next;
                    let phase = if wrap { FLAG_AVAIL } else { FLAG_USED };
                    put(&mut r.m, r.format, i, gpa, len, flags | phase, c);
                    r.next = if i + 1 == SIZE {
                        (0, !wrap)
                    } else {
                        (i + 1, wrap)
                    };
                }
            }
        }
    }
    if r.format == RingFormat::Split {
        r.m.write(DRIVER + 2, &r.avail.to_le_bytes()).unwrap();
    }
}

type Capture = fn(&mut VirtQueue, &GuestMemory) -> Result<Option<Chain>, QueueError>;

fn lap(capture: Capture, r: &mut Ring) -> u64 {
    post(r);
    let mut sum = 0u64;
    while let Some(chain) = capture(&mut r.q, &r.m).unwrap() {
        sum = sum.wrapping_add(chain.writable_len() + chain.readable().len() as u64);
        r.q.complete(&mut r.m, chain, 513).unwrap();
    }
    r.q.flush(&mut r.m).unwrap();
    sum
}

fn rust_capture(q: &mut VirtQueue, m: &GuestMemory) -> Result<Option<Chain>, QueueError> {
    q.capture_reference(m, None)
}

fn vesper_capture(q: &mut VirtQueue, m: &GuestMemory) -> Result<Option<Chain>, QueueError> {
    q.capture(m, None)
}

struct Arm(Capture, Ring, Ring);

fn arm(capture: Capture) -> Arm {
    Arm(capture, ring(RingFormat::Split), ring(RingFormat::Packed))
}

impl BenchCase for Arm {
    type Input = ();
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<(), ()> {
        Ok(())
    }
    fn run(&mut self, (): ()) -> Result<u64, ()> {
        let a = lap(self.0, black_box(&mut self.1));
        let b = lap(self.0, black_box(&mut self.2));
        Ok(a.wrapping_add(b))
    }
}

#[test]
fn both_builds_compute_the_same_workload() {
    let (mut a, mut b) = (arm(rust_capture), arm(vesper_capture));
    for _ in 0..6 {
        assert_eq!(a.run(()), b.run(()));
    }
    assert_eq!(a.run(()), Ok(2 * u64::from(CHAINS) * (513 + 16)));
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_virtqueue() {
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report = run_abba(run, &mut arm(rust_capture), &mut arm(vesper_capture)).expect("ABBA run");
    let pairs = report.pairs();
    let line: Vec<String> = pairs
        .iter()
        .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
        .collect();
    println!("flip-gate pairs virtqueue {}", line.join(" "));
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
