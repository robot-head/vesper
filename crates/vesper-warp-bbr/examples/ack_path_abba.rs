//! ABBA benchmark of the ack/loss event path, Rust build against Vesper
//! build (port wave P1 lane 10, #907).
//!
//! The two builds are the same crate with different features, so they
//! cannot be linked into one process. Each measured window therefore runs
//! in a fresh child process of the build under test:
//!
//! ```text
//! ack_path_abba measure <steps>
//! ack_path_abba abba <rust-build> <vesper-build> [epochs] [steps]
//! ```
//!
//! `measure` warms one controller up into ProbeBW, then times `steps`
//! iterations of the steady-state workload of `tests/zero_alloc.rs` (send
//! while the window allows, then ACK the oldest packet, or every 97th
//! packet lose it with a CE mark) and prints the build and the elapsed
//! nanoseconds. `abba` runs the two binaries in the fixed order A B B A per
//! epoch, pairs the two windows of each arm, and applies
//! `vesper_bench::analyze_pairs` to the epochs.
use std::process::Command;
use std::time::Instant;
use vesper_bench::{PairedSample, analyze_pairs};
use vesper_warp_bbr::{
    AckedPacket, BbrConfig, Bbrv3, Delivered, IMPLEMENTATION, LostPacket, PacketDeliveryState,
    RttSample, SentPacket,
};

const RING: usize = 256;

struct Flow {
    bbr: Bbrv3,
    slots: [(SentPacket, PacketDeliveryState); RING],
    head: usize,
    len: usize,
    now: u64,
    pn: u64,
}

impl Flow {
    fn new() -> Self {
        let mut bbr = Bbrv3::new(BbrConfig::quic(1200), 0).unwrap();
        let empty = SentPacket {
            pn: 0,
            sent_ns: 0,
            bytes: 0,
            ack_eliciting: false,
            in_flight: false,
        };
        let blank = bbr.on_packet_sent(0, empty).unwrap();
        Self {
            bbr,
            slots: [(empty, blank); RING],
            head: 0,
            len: 0,
            now: 1,
            pn: 1,
        }
    }

    fn step(&mut self, loss_every: u64) {
        while self.bbr.can_send() && self.len < RING {
            let launch = self.bbr.schedule_edt_ns(1200, self.now);
            self.now = launch;
            let packet = SentPacket {
                pn: self.pn,
                sent_ns: launch,
                bytes: 1200,
                ack_eliciting: true,
                in_flight: true,
            };
            self.pn += 1;
            let state = self.bbr.on_packet_sent(launch, packet).unwrap();
            self.slots[(self.head + self.len) % RING] = (packet, state);
            self.len += 1;
        }
        let (packet, state) = self.slots[self.head];
        self.head = (self.head + 1) % RING;
        self.len -= 1;
        self.now = self.now.max(packet.sent_ns + 20_000_000);
        if loss_every != 0 && packet.pn % loss_every == 0 {
            self.bbr.on_ecn_ce(self.now, 1).unwrap();
            let lost = LostPacket {
                pn: packet.pn,
                sent_ns: packet.sent_ns,
                bytes: packet.bytes,
            };
            self.bbr.on_packet_lost(self.now, lost, state).unwrap();
        } else {
            let acked = [Delivered {
                packet: AckedPacket {
                    pn: packet.pn,
                    sent_ns: packet.sent_ns,
                    bytes: packet.bytes,
                },
                state,
                previously_lost: false,
            }];
            let rtt = self.now - packet.sent_ns;
            let sample = RttSample {
                latest_ns: rtt,
                min_ns: 20_000_000,
                smoothed_ns: rtt,
                ack_delay_ns: 0,
            };
            self.bbr.on_ack(self.now, &acked, Some(sample)).unwrap();
        }
    }
}

fn measure(steps: u64) {
    let mut flow = Flow::new();
    for _ in 0..5_000 {
        flow.step(0);
    }
    assert!(flow.bbr.full_bw_reached());
    let start = Instant::now();
    for _ in 0..steps {
        flow.step(97);
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(flow.bbr.delivered_bytes());
    println!("{IMPLEMENTATION} {elapsed}");
}

fn window(binary: &str, expect: &str, steps: u64) -> u64 {
    let out = Command::new(binary)
        .args(["measure", &steps.to_string()])
        .output()
        .expect("run a measured window");
    assert!(out.status.success(), "{binary} failed");
    let text = String::from_utf8(out.stdout).unwrap();
    let (build, nanos) = text.trim().split_once(' ').unwrap();
    assert_eq!(build, expect, "{binary} is the {build} build");
    nanos.parse().unwrap()
}

fn abba(rust: &str, vesper: &str, epochs: usize, steps: u64) {
    let mut pairs = Vec::new();
    println!("epoch  rust ns/step  vesper ns/step  ratio");
    for epoch in 0..epochs {
        let a1 = window(rust, "rust", steps);
        let b1 = window(vesper, "vesper", steps);
        let b2 = window(vesper, "vesper", steps);
        let a2 = window(rust, "rust", steps);
        let (a, b) = (a1 + a2, b1 + b2);
        let ops = 2 * steps;
        println!(
            "{epoch:>5}  {:>12.1}  {:>14.1}  {:>5.2}",
            a as f64 / ops as f64,
            b as f64 / ops as f64,
            b as f64 / a as f64
        );
        pairs.push(PairedSample::new(a, b, ops).unwrap());
    }
    let analysis = analyze_pairs(&pairs, 10_000).unwrap();
    let lo = analysis.median_lower.regression_bp_bounds();
    let hi = analysis.median_upper.regression_bp_bounds();
    println!(
        "median regression {}..{} bp (sample median pair); gate at +100% (2x): {:?}",
        lo.floor, hi.ceil, analysis.decision
    );
    if let Some(interval) = analysis.interval {
        let at = |i: usize| {
            let mut sorted: Vec<f64> = pairs
                .iter()
                .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
                .collect();
            sorted.sort_by(f64::total_cmp);
            sorted[i - 1]
        };
        println!(
            "95% median interval of the ratio: {:.3}..{:.3}",
            at(interval.lower_rank),
            at(interval.upper_rank)
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("measure") => measure(args.get(2).map_or(200_000, |s| s.parse().unwrap())),
        Some("abba") => abba(
            &args[2],
            &args[3],
            args.get(4).map_or(12, |s| s.parse().unwrap()),
            args.get(5).map_or(200_000, |s| s.parse().unwrap()),
        ),
        _ => eprintln!(
            "usage: ack_path_abba measure <steps> | abba <rust> <vesper> [epochs] [steps]"
        ),
    }
}
