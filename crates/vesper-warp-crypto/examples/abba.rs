//! ABBA benchmark of vesper-warp-crypto (Rust-exit SH6 lane 17, #1412):
//! the Rust implementation (the last Rust revision) against the Vesper port.
//!
//! - `one_rtt`: [`ONE_RTT_PACKETS`] round trips of 1,200-byte 1-RTT packets
//!   per cipher suite, with a client key update every 64 (derivation,
//!   promotion and the peer's response).
//! - `initial`: [`INITIAL_RUNS`] times, the Initial secrets and keys of a
//!   fresh connection ID and one 1,200-byte Initial packet each way.
//! - `header`: [`HEADER_OPS`] header-protection removals and re-applications
//!   per cipher suite on a 64-byte packet: the per-call costs.
//!
//! The two arms are this example built at two revisions, in separate worker
//! processes; each worker runs one batch per request on stdin, and the
//! driver times each request with `vesper-bench`'s `run_abba`, so both arms
//! carry the same pipe round trip. Every batch returns a digest, and the
//! driver checks that both builds agree.
//!
//! ```text
//! abba worker <one_rtt|initial|header>                  # one arm
//! abba drive <one_rtt|initial|header> <rust-worker> <vesper-worker>
//! ```
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_warp_crypto::{Cipher, InitialSecrets, OneRttKeys, Opener, Sealer, Side, TrafficSecret};

/// Packets per suite per `one_rtt` batch.
const ONE_RTT_PACKETS: u64 = 512;
/// Connections per `initial` batch.
const INITIAL_RUNS: u64 = 64;
/// Removals (and re-applications) per suite per `header` batch.
const HEADER_OPS: u64 = 2048;
const PAYLOAD: usize = 1200 - 11 - 16;
const CIPHERS: [Cipher; 3] = [
    Cipher::Aes128Gcm,
    Cipher::Aes256Gcm,
    Cipher::ChaCha20Poly1305,
];

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// The packet core's job: the low 16 bits after `pn`'s high bits.
fn reconstruct(pn: u64, truncated: u64) -> u64 {
    (pn & !0xffff) | truncated
}

fn short(buf: &mut [u8], phase: bool, pn: u64) {
    buf[0] = 0x40 | (u8::from(phase) << 2) | 0x01;
    buf[1..9].fill(0xcd);
    buf[9..11].copy_from_slice(&(pn as u16).to_be_bytes());
    buf[11..11 + PAYLOAD].fill(pn as u8);
}

fn one_rtt_batch(n: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325;
    for cipher in CIPHERS {
        let s =
            |f: u8| TrafficSecret::new(cipher, &vec![f ^ n as u8; cipher.secret_len()]).unwrap();
        let mut c = OneRttKeys::new(s(1), s(2), 1_000).unwrap();
        let mut srv = OneRttKeys::new(s(2), s(1), 1_000).unwrap();
        let mut buf = vec![0u8; 11 + PAYLOAD + 16];
        for pn in 0..ONE_RTT_PACKETS {
            if pn > 0 && pn % 64 == 0 {
                c.on_packet_acked(pn - 1);
                c.initiate_key_update().unwrap();
            }
            short(&mut buf, c.send_key_phase(), pn);
            let t = c.ticket(pn).unwrap();
            let len = c.seal(t, &mut buf, 9, PAYLOAD).unwrap();
            h = fnv(h, &buf[..len]);
            let o = srv
                .open_packet(&mut buf, 9, pn, |u| Some(reconstruct(pn, u.truncated_pn)))
                .unwrap();
            h = fnv(h, &[o.generation as u8, o.payload_len as u8]);
            // The reply, so the client sees the server's response to its
            // update before the next one.
            short(&mut buf, srv.send_key_phase(), pn);
            let t = srv.ticket(pn).unwrap();
            let len = srv.seal(t, &mut buf, 9, PAYLOAD).unwrap();
            h = fnv(h, &buf[..len]);
            c.open_packet(&mut buf, 9, pn, |u| Some(reconstruct(pn, u.truncated_pn)))
                .unwrap();
        }
    }
    h
}

fn initial_batch(n: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325;
    let mut buf = vec![0u8; 11 + PAYLOAD + 16];
    for i in 0..INITIAL_RUNS {
        let dcid = (n * INITIAL_RUNS + i).to_le_bytes();
        let secrets = InitialSecrets::new_v1(&dcid).unwrap();
        for side in [Side::Client, Side::Server] {
            let (seal, _) = secrets.keys_for(side);
            let peer = if side == Side::Client {
                Side::Server
            } else {
                Side::Client
            };
            let (_, open) = secrets.keys_for(peer);
            let mut sealer = Sealer::new(seal);
            let mut opener = Opener::new(open);
            short(&mut buf, false, 0);
            buf[0] = 0xc1;
            let t = sealer.next_ticket().unwrap();
            let len = sealer.seal(t, &mut buf, 9, PAYLOAD).unwrap();
            h = fnv(h, &buf[..len]);
            opener
                .open_packet(&mut buf, 9, |u| Some(u.truncated_pn))
                .unwrap();
        }
    }
    h
}

fn header_batch(n: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325;
    for cipher in CIPHERS {
        let s = TrafficSecret::new(cipher, &vec![n as u8; cipher.secret_len()]).unwrap();
        let keys = s.keys();
        let mut buf = [0u8; 64];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31) ^ n as u8;
        }
        buf[0] = 0x41;
        for i in 0..HEADER_OPS {
            buf[30] = i as u8;
            let u = keys.header.remove(&mut buf, 9).unwrap();
            h = fnv(h, &[u.first_byte, u.truncated_pn as u8]);
            u.reapply(&mut buf);
        }
        h = fnv(h, &buf);
    }
    h
}

/// Serve batches: each stdin line `go <n>` runs batch `n` and answers with
/// its digest.
fn worker(workload: &str) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.expect("read request");
        let n: u64 = line
            .strip_prefix("go ")
            .and_then(|n| n.parse().ok())
            .expect("request `go <n>`");
        let digest = match workload {
            "one_rtt" => one_rtt_batch(n),
            "initial" => initial_batch(n),
            "header" => header_batch(n),
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
        "one_rtt" => (4, 6 * ONE_RTT_PACKETS),
        "initial" => (4, INITIAL_RUNS),
        "header" => (4, 3 * HEADER_OPS),
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
    let unit = "op";
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
    // `ratios` is sorted; the interval's ranks are 1-based.
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
        _ => {
            eprintln!(
                "usage: abba worker <one_rtt|initial|header> | abba drive <one_rtt|initial|header> <rust-worker> <vesper-worker>"
            );
            std::process::exit(2);
        }
    }
}
