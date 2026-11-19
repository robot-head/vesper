//! ABBA benchmark of the Rust and Vesper builds of the ported modules (port
//! wave P3b lane 4, #1066): packet and frame encode/decode, ACK tracking,
//! and a loopback stream echo over two endpoints (whose packet path calls
//! the ported header, frame, packet-number and ACK code).
//!
//! The two builds are one crate compiled with and without `vesper-port`, so
//! they cannot share a process. Each arm is a worker process (this example
//! built one way or the other) that runs one batch of the workload per
//! request on stdin; the driver times each request with `vesper-bench`'s
//! `run_abba`, so both arms carry the same pipe round trip. Every batch
//! returns a digest of its results, and the driver checks that both builds
//! agree.
//!
//! ```text
//! crates/vesper-warp/port/bench.sh                 # builds both, runs every workload
//! abba worker <frames|ack|transfer>                # one arm
//! abba drive <frames|ack|transfer> <rust-worker> <vesper-worker>
//! abba once <frames|ack|transfer> <batches>        # for callgrind
//! ```
#[path = "../tests/support/mod.rs"]
mod support;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use support::{MS, Pair};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_warp::ack::{Ecn, ReceivedPackets};
use vesper_warp::endpoint::{Discard, EndpointConfig};
use vesper_warp::frame::{AckFrame, AckRange, EcnCounts, Frame, StreamFrame, scan_payload};
use vesper_warp::header::{HeaderParser, ParsedPacket};
use vesper_warp::packet_number::encoded_len;
use vesper_warp::{PacketType, PnSpace};

/// Packets built and parsed per frames batch.
const FRAME_PACKETS: u64 = 1024;
/// Packets received per ACK batch.
const ACK_PACKETS: u64 = 8192;
/// Echo rounds (2 KiB each way) per transfer batch.
const TRANSFER_ROUNDS: u64 = 16;
const DCID: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x100_0000_01b3)
}

/// Build a 1-RTT packet (short header, ACK with three ranges and ECN,
/// STREAM, MAX_STREAM_DATA, PATH_CHALLENGE), then parse it back the way the
/// receive path does: header, packet-number field, full number, frames.
fn frames_batch(parser: &HeaderParser, packet: &mut [u8], data: &[u8], base: u64) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    for k in 0..FRAME_PACKETS {
        let pn = base + k;
        let pn_len = encoded_len(pn, pn.checked_sub(3)).expect("pn length");
        let mut end = parser
            .write_short(packet, &DCID, k % 2 == 0, false, pn, pn_len)
            .expect("short header");
        let ranges = [
            AckRange {
                smallest: pn.saturating_sub(4),
                largest: pn + 10,
            },
            AckRange {
                smallest: pn.saturating_sub(20),
                largest: pn.saturating_sub(8),
            },
            AckRange {
                smallest: 0,
                largest: pn.saturating_sub(40).min(1),
            },
        ];
        let ack = AckFrame::from_ranges(
            &ranges[..if pn >= 64 { 3 } else { 1 }],
            25,
            Some(EcnCounts {
                ect0: pn,
                ect1: 0,
                ce: k,
            }),
        )
        .expect("ack ranges");
        let frames = [
            Frame::Ack(ack),
            Frame::Stream(StreamFrame {
                stream_id: 4,
                offset: pn * 1000,
                data,
                fin: false,
                has_length: true,
            }),
            Frame::MaxStreamData {
                stream_id: 4,
                max: pn * 1000 + 65_536,
            },
            Frame::PathChallenge(pn.to_be_bytes()),
        ];
        for f in &frames {
            end += f.encode(&mut packet[end..]).expect("encode");
        }
        let datagram = &packet[..end];
        let ParsedPacket::Protected(h) = parser.parse(datagram).expect("parse") else {
            panic!("not a protected packet");
        };
        let u = h.packet_number_field(datagram).expect("pn field");
        let full = u.pn.decode(Some(pn.saturating_sub(1))).expect("pn decode");
        digest = mix(digest, full);
        let payload = u.payload.of(datagram);
        let info = scan_payload(payload, PacketType::OneRtt, |f| {
            digest = mix(digest, f.frame_type());
            match f {
                Frame::Ack(a) => {
                    for r in a.ranges() {
                        digest = mix(digest, r.smallest ^ r.largest << 1);
                    }
                }
                Frame::Stream(s) => digest = mix(digest, s.offset + s.data.len() as u64),
                Frame::MaxStreamData { max, .. } => digest = mix(digest, *max),
                _ => {}
            }
        })
        .expect("scan");
        digest = mix(digest, info.frames as u64);
    }
    digest
}

/// Receive a reordered, gappy run of packet numbers, building and encoding
/// an ACK frame every other packet and pruning on ACK-of-ACK.
fn ack_batch(base: u64) -> u64 {
    let mut rx: ReceivedPackets<32> = ReceivedPackets::new(PnSpace::ApplicationData, 25 * MS);
    let mut out = [0u8; 512];
    let mut digest = 0xcbf2_9ce4_8422_2325;
    for i in 0..ACK_PACKETS {
        // Swap neighbours every seventh pair, skip every fiftieth number.
        let j = if i % 7 == 3 {
            i + 1
        } else if i % 7 == 4 {
            i - 1
        } else {
            i
        };
        if j % 50 == 49 {
            continue;
        }
        let pn = base + j;
        let now = (base + i) * 10_000;
        let ecn = if i % 97 == 0 { Ecn::Ce } else { Ecn::Ect0 };
        if let Ok(r) = rx.on_packet_received(pn, now, i % 3 != 0, ecn) {
            digest = mix(
                digest,
                u64::from(r.largest) | u64::from(r.out_of_order) << 1,
            );
        }
        if i % 2 == 1 {
            if let Some(ack) = rx.ack_frame(now + 5_000, 3).expect("ack frame") {
                let n = Frame::Ack(ack).encode(&mut out).expect("encode ack");
                digest = mix(digest, n as u64);
            }
            rx.on_ack_sent();
        }
        if i % 64 == 63
            && let Some(l) = rx.largest()
        {
            rx.on_ack_of_ack(l.saturating_sub(16));
        }
        digest = mix(digest, rx.ack_deadline().unwrap_or(0));
    }
    digest
}

/// The stream echo of tests/stream_zero_alloc.rs: the client writes 2 KiB,
/// the server echoes what it reads, and the client reads the echo back.
struct Transfer {
    p: Pair<Discard, Discard>,
    id: u64,
    data: [u8; 2048],
    buf: Vec<u8>,
    returned: u64,
}

impl Transfer {
    fn new() -> Self {
        let config = EndpointConfig {
            initial_rtt_ns: 20 * MS,
            ..EndpointConfig::default()
        };
        let mut p = Pair::new(config, Discard::default(), Discard::default());
        p.record = false;
        p.client.tap_mut().log_enabled = false;
        p.server.tap_mut().log_enabled = false;
        let id = p.client.open_stream(p.c_conn, false).expect("open stream");
        Self {
            p,
            id,
            data: [0x5a; 2048],
            buf: vec![0; 8192],
            returned: 0,
        }
    }

    fn round(&mut self) {
        let (p, id) = (&mut self.p, self.id);
        let n = p
            .client
            .stream_write(p.c_conn, id, &self.data)
            .expect("write");
        assert_eq!(n, self.data.len(), "the send ring keeps up");
        for _ in 0..4 {
            p.step();
            while p.server.poll_stream_event(p.s_conn).is_some() {}
            while p.client.poll_stream_event(p.c_conn).is_some() {}
            let conn = p.server.connection(p.s_conn).expect("server connection");
            if conn.streams.recv_state(id).is_some() {
                let space = conn.streams.write_space(id).expect("write space");
                let r = p
                    .server
                    .stream_read(p.s_conn, id, &mut self.buf[..space.min(8192)])
                    .expect("server read");
                let w = p
                    .server
                    .stream_write(p.s_conn, id, &self.buf[..r.len])
                    .expect("echo");
                assert_eq!(w, r.len);
            }
            let r = p
                .client
                .stream_read(p.c_conn, id, &mut self.buf)
                .expect("client read");
            self.returned += r.len as u64;
        }
    }

    fn batch(&mut self) -> u64 {
        for _ in 0..TRANSFER_ROUNDS {
            self.round();
        }
        let s = self.p.client.stats();
        assert_eq!(
            s.transport_errors + self.p.server.stats().transport_errors,
            0
        );
        // Both arms see the same request sequence, so the totals agree.
        mix(
            mix(0xcbf2_9ce4_8422_2325, self.returned),
            s.stream_packets_sent,
        )
    }
}

struct Worker {
    workload: String,
    parser: HeaderParser,
    packet: Vec<u8>,
    data: Vec<u8>,
    transfer: Option<Transfer>,
}

impl Worker {
    fn new(workload: &str) -> Self {
        let mut transfer = None;
        if workload == "transfer" {
            let mut t = Transfer::new();
            // Warm up past the handshake-free start and slow start.
            for _ in 0..50 {
                t.round();
            }
            transfer = Some(t);
        }
        Self {
            workload: workload.to_string(),
            parser: HeaderParser::new(DCID.len()).expect("parser"),
            packet: vec![0; 1500],
            data: (0..1000u32).map(|i| (i * 31) as u8).collect(),
            transfer,
        }
    }

    fn batch(&mut self, n: u64) -> u64 {
        match self.workload.as_str() {
            "frames" => frames_batch(
                &self.parser,
                &mut self.packet,
                &self.data,
                n * FRAME_PACKETS,
            ),
            "ack" => ack_batch(n * ACK_PACKETS),
            "transfer" => self.transfer.as_mut().expect("transfer").batch(),
            other => panic!("unknown workload {other}"),
        }
    }
}

fn worker(workload: &str) {
    let mut w = Worker::new(workload);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.expect("read request");
        let n: u64 = line
            .strip_prefix("go ")
            .and_then(|n| n.parse().ok())
            .expect("request `go <n>`");
        let digest = w.batch(n);
        writeln!(out, "{digest}").expect("answer");
        out.flush().expect("flush");
    }
}

fn once(workload: &str, batches: u64) {
    let mut w = Worker::new(workload);
    let mut digest = 0;
    for n in 0..batches {
        digest = mix(digest, w.batch(n));
    }
    println!("{workload}: {batches} batches, digest {digest:016x}");
}

struct Arm {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    digests: Vec<(usize, u64)>,
}

impl Arm {
    fn spawn(binary: &str, workload: &str) -> Arm {
        let mut child = Command::new(binary)
            .args(["worker", workload])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {binary}: {e}"));
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Arm {
            child,
            stdin,
            stdout,
            digests: Vec::new(),
        }
    }
}

impl BenchCase for Arm {
    type Input = usize;
    type Output = (usize, u64);
    type Error = std::io::Error;
    fn setup(&mut self, i: Invocation) -> Result<usize, std::io::Error> {
        Ok(i.operation)
    }
    fn run(&mut self, n: usize) -> Result<(usize, u64), std::io::Error> {
        writeln!(self.stdin, "go {n}")?;
        self.stdin.flush()?;
        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        let digest = line
            .trim()
            .parse()
            .map_err(|e| std::io::Error::other(format!("worker answered {line:?}: {e}")))?;
        Ok((n, digest))
    }
    fn teardown(&mut self, output: (usize, u64)) -> Result<(), std::io::Error> {
        self.digests.push(output);
        Ok(())
    }
}

fn drive(workload: &str, rust: &str, vesper: &str) {
    let (operations, per_batch, unit) = match workload {
        "frames" => (32, FRAME_PACKETS, "packet"),
        "ack" => (16, ACK_PACKETS, "packet"),
        "transfer" => (8, TRANSFER_ROUNDS, "round"),
        other => panic!("unknown workload {other}"),
    };
    let config = RunConfig {
        epochs: 12,
        warmup_epochs: 1,
        operations_per_window: operations,
        max_elapsed_nanos: 600_000_000_000,
        ..RunConfig::default()
    };
    let mut baseline = Arm::spawn(rust, workload);
    let mut candidate = Arm::spawn(vesper, workload);
    let report = run_abba(config, &mut baseline, &mut candidate).expect("ABBA run");
    let mut a = baseline.digests.clone();
    let mut b = candidate.digests.clone();
    a.sort_unstable();
    b.sort_unstable();
    if workload != "transfer" {
        // Transfer batches continue one connection, so a digest depends on
        // the arm's request order; frames and ack batches are pure.
        assert_eq!(
            a, b,
            "the Rust and Vesper builds computed different results"
        );
    }
    let mut ratios: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let per_unit = |total: u64| total as f64 / (2 * operations) as f64 / per_batch as f64;
    let mut rust_ns: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| per_unit(p.baseline_total()))
        .collect();
    let mut vesper_ns: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| per_unit(p.candidate_total()))
        .collect();
    rust_ns.sort_by(f64::total_cmp);
    vesper_ns.sort_by(f64::total_cmp);
    let median = |v: &[f64]| (v[(v.len() - 1) / 2] + v[v.len() / 2]) / 2.0;
    println!(
        "{workload}: {} measured epochs x A/B/B/A x {operations} requests; \
         Rust median {:.0} ns/{unit}, Vesper median {:.0} ns/{unit}, \
         Vesper/Rust median {:.2}x (min {:.2}x, max {:.2}x)",
        report.pairs().len(),
        median(&rust_ns),
        median(&vesper_ns),
        median(&ratios),
        ratios[0],
        ratios[ratios.len() - 1],
    );
    let analysis = analyze_pairs(report.pairs(), 10_000).expect("analysis");
    println!(
        "{workload}: 2x gate (candidate at most 10000 bp slower): {:?}; {} batches each",
        analysis.decision,
        a.len()
    );
    if let Some(interval) = analysis.interval {
        println!(
            "{workload}: 95% median interval of Vesper/Rust: {:.3}x..{:.3}x",
            ratios[interval.lower_rank - 1],
            ratios[interval.upper_rank - 1]
        );
    }
    for arm in [&mut baseline, &mut candidate] {
        let _ = arm.child.kill();
        let _ = arm.child.wait();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[1..] {
        ["worker", workload] => worker(workload),
        ["drive", workload, rust, vesper] => drive(workload, rust, vesper),
        ["once", workload, batches] => once(workload, batches.parse().expect("batches")),
        _ => {
            eprintln!(
                "usage: abba worker <frames|ack|transfer> | abba drive <workload> <rust-worker> <vesper-worker> | abba once <workload> <batches>"
            );
            std::process::exit(2);
        }
    }
}
