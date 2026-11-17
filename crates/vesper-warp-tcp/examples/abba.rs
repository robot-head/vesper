//! ABBA benchmark of the Rust and Vesper builds (port wave P1 lane 11,
//! #908): segment encode/decode and the harness transfer loop.
//!
//! The two builds are one crate compiled with and without `vesper-port`, so
//! they cannot share a process. Each arm is a worker process (this example
//! built one way or the other) that runs one batch of the workload per
//! request on stdin; the driver times each request with `vesper-bench`'s
//! `run_abba`, so both arms carry the same pipe round trip. A batch is long
//! (milliseconds) next to that round trip. Every batch returns a digest of
//! its results, and the driver checks that both builds agree.
//!
//! ```text
//! crates/vesper-warp-tcp/scripts/bench.sh        # builds both, runs both workloads
//! abba worker <codec|transfer>                   # one arm
//! abba drive <codec|transfer> <rust-worker> <vesper-worker>
//! ```
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_warp_tcp::harness::{Harness, LinkConfig};
use vesper_warp_tcp::rtt::RtoConfig;
use vesper_warp_tcp::wire::{
    HeaderFields, SackBlock, Segment, TcpOptions, Timestamps, fill_checksum, verify_checksum,
    write_header,
};
use vesper_warp_tcp::{Config, Connection, Flags, IpPair, Seq, State};

/// Segments encoded and decoded per codec batch.
const CODEC_SEGMENTS: u32 = 64;
/// Payload of each codec segment (one Ethernet MSS).
const CODEC_PAYLOAD: usize = 1460;
/// Bytes moved A → B per transfer batch.
const TRANSFER_BYTES: usize = 64 * 1024;
const MS: u64 = 1_000_000;

fn ips() -> IpPair {
    IpPair::V4 {
        src: [10, 0, 0, 1],
        dst: [10, 0, 0, 2],
    }
}

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Encode a data segment with timestamps and two SACK blocks, fill its
/// checksum, then parse and verify it: the per-segment work of both the
/// send and the receive path.
fn codec_batch(buf: &mut [u8], payload: &[u8]) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    for i in 0..CODEC_SEGMENTS {
        let fields = HeaderFields {
            src_port: 40000,
            dst_port: 80,
            seq: Seq(0x1000_0000_u32.wrapping_add(i.wrapping_mul(1460))),
            ack: Seq(0x2000_0000 + i),
            flags: Flags::ACK | Flags::PSH,
            window: 0xffff,
            urgent: 0,
        };
        let mut options = TcpOptions {
            timestamps: Some(Timestamps {
                val: 1000 + i,
                ecr: 900 + i,
            }),
            ..TcpOptions::default()
        };
        options.sack_blocks[0] = SackBlock {
            left: Seq(0x2000_1000),
            right: Seq(0x2000_2000),
        };
        options.sack_blocks[1] = SackBlock {
            left: Seq(0x2000_3000),
            right: Seq(0x2000_4000 + i),
        };
        options.sack_len = 2;
        let header = write_header(buf, &fields, &options).expect("header fits");
        let len = header + payload.len();
        buf[header..len].copy_from_slice(payload);
        fill_checksum(ips(), &mut buf[..len]).expect("checksum");
        verify_checksum(ips(), &buf[..len]).expect("checksum verifies");
        let segment = Segment::parse(&buf[..len]).expect("segment parses");
        digest = fnv(digest, &segment.seq.0.to_le_bytes());
        digest = fnv(digest, &segment.checksum.to_le_bytes());
        digest = fnv(digest, &[segment.options.sack_len]);
        digest = fnv(digest, &(segment.payload.len() as u32).to_le_bytes());
    }
    digest
}

fn config(port: u16) -> Config {
    Config {
        local_port: port,
        mss: 1460,
        recv_buffer: 256 * 1024,
        rto: RtoConfig {
            initial_ns: 200 * MS,
            min_ns: 50 * MS,
            max_ns: 4_000 * MS,
            granularity_ns: MS,
        },
        time_wait_ns: 100 * MS,
        delayed_ack_ns: 10 * MS,
        max_in_flight_segments: 256,
        ..Config::default()
    }
}

/// Handshake, transfer `TRANSFER_BYTES` A → B over a lossy, reordering
/// link (seeded, so both builds see the same packets) and close both ways.
fn transfer_batch(data: &[u8], seed: u64) -> u64 {
    let link = LinkConfig {
        delay_ns: 5 * MS,
        jitter_ns: 2 * MS,
        loss_ppm: 10_000,
        seed,
        ..LinkConfig::default()
    };
    let mut h = Harness::new(
        Connection::new(config(40000)).expect("config"),
        Connection::new(config(80)).expect("config"),
        link,
    )
    .expect("harness");
    let established = h
        .run_until(30_000 * MS, |h| h.both(State::Established))
        .expect("handshake");
    assert!(established, "handshake did not complete");
    h.a.write(data).expect("write");
    h.a.conn.close().expect("close");
    let done = h
        .run_until(600_000 * MS, |h| h.b.peer_finished)
        .expect("transfer");
    assert!(done && h.b.received == data, "stream not delivered");
    h.b.conn.close().expect("close");
    let closed = h
        .run_until(h.now + 600_000 * MS, |h| h.both(State::Closed))
        .expect("close");
    assert!(closed, "connections did not close");
    let stats = h.a.conn.stats();
    let mut digest = fnv(0xcbf2_9ce4_8422_2325, &h.now.to_le_bytes());
    digest = fnv(digest, &stats.retransmits.to_le_bytes());
    digest = fnv(digest, &stats.segments_sent.to_le_bytes());
    fnv(digest, &h.dropped.to_le_bytes())
}

fn pattern(n: usize) -> Vec<u8> {
    let mut x = 0x9e37_79b9_7f4a_7c15_u64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Serve batches: each stdin line `go <n>` runs batch `n` and answers with
/// its digest.
fn worker(workload: &str) {
    let payload = pattern(CODEC_PAYLOAD);
    let data = pattern(TRANSFER_BYTES);
    let mut buf = vec![0u8; 60 + CODEC_PAYLOAD];
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.expect("read request");
        let n: u64 = line
            .strip_prefix("go ")
            .and_then(|n| n.parse().ok())
            .expect("request `go <n>`");
        let digest = match workload {
            "codec" => codec_batch(&mut buf, &payload),
            "transfer" => transfer_batch(&data, 1 + n % 8),
            other => panic!("unknown workload {other}"),
        };
        writeln!(out, "{digest}").expect("answer");
        out.flush().expect("flush");
    }
}

/// One arm: a worker process timed per request.
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
    let (operations, per_batch) = match workload {
        "codec" => (32, u64::from(CODEC_SEGMENTS)),
        "transfer" => (4, 1),
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
    assert_eq!(
        a, b,
        "the Rust and Vesper builds computed different results"
    );
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
    let unit = if workload == "codec" {
        "segment"
    } else {
        "transfer"
    };
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
        "{workload}: 2x gate (candidate at most 10000 bp slower): {:?}; digests agree over {} batches",
        analysis.decision,
        a.len()
    );
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
        _ => {
            eprintln!(
                "usage: abba worker <codec|transfer> | abba drive <codec|transfer> <rust-worker> <vesper-worker>"
            );
            std::process::exit(2);
        }
    }
}
