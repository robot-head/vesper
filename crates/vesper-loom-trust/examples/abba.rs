//! ABBA benchmark of vesper-loom-trust (Rust-exit SH6 lane 17, #1412): the
//! Rust implementation (the last Rust revision) against the Vesper port.
//!
//! - `verify`: [`VERIFY_RUNS`] verifications of a three-operator bundle that
//!   is accepted and of one with a tampered extra that is rejected.
//! - `screen`: [`SCREEN_RUNS`] admission screens against a 200-entry catalog
//!   of lookalike names.
//! - `signing`: [`SIGNING_RUNS`] signing messages.
//!
//! The two arms are this example built at two revisions, in separate worker
//! processes; each worker runs one batch per request on stdin, and the
//! driver times each request with `vesper-bench`'s `run_abba`, so both arms
//! carry the same pipe round trip. Every batch returns a digest, and the
//! driver checks that both builds agree.
//!
//! ```text
//! abba worker <verify|screen|signing>                   # one arm
//! abba drive <verify|screen|signing> <rust-worker> <vesper-worker>
//! ```
use ed25519_dalek::{Signer, SigningKey};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_loom_calculus::PackageName;
use vesper_loom_cas::{Digest, Domain, ObjectId, ObjectKind};
use vesper_loom_trust::slopsquat::{CatalogEntry, QuarantinePolicy, screen_package_admission};
use vesper_loom_trust::*;

/// Verifications of each bundle per `verify` batch.
const VERIFY_RUNS: u64 = 16;
/// Screens per `screen` batch.
const SCREEN_RUNS: u64 = 32;
/// Signing messages per `signing` batch.
const SIGNING_RUNS: u64 = 2048;

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn identity() -> BuildIdentity {
    BuildIdentity {
        package: PackageName::parse("@test/package").unwrap(),
        version: "1.2.3-rc.1+source".into(),
        source_tree: ObjectId::new(Domain::Source, ObjectKind::Tree, Digest([4; 32])).to_string(),
        derivation: format!("drv:b3v1:{}", "5a".repeat(32)),
        target: "x86_64-unknown-linux-gnu".into(),
        builder_blake3: "01".repeat(32),
        sandbox_contract: "vesper:sandbox/step@0.1.0".into(),
        output: ObjectId::new(Domain::Artifact, ObjectKind::Blob, Digest([7; 32])).to_string(),
    }
}

fn statement(key: &str, n: u64) -> Statement {
    Statement {
        schema_version: SCHEMA_VERSION,
        key_id: key.into(),
        identity: identity(),
        issued_at: 100 + n % 7,
        expires_at: 300,
    }
}

fn fixture() -> (
    BuildIdentity,
    BuilderPolicy,
    AttestationBundle,
    AttestationBundle,
) {
    let mut builders = Vec::new();
    let mut attestations = Vec::new();
    for k in 0..3u8 {
        let signing = SigningKey::from_bytes(&[k + 1; 32]);
        let id = format!("key-{k}");
        builders.push(TrustedBuilder {
            key_id: id.clone(),
            operator_id: format!("operator-{k}"),
            public_key: hex(&signing.verifying_key().to_bytes()),
            valid_from: 10,
            valid_until: 1000,
            revoked: false,
        });
        let st = statement(&id, 0);
        let signature = hex(&signing
            .sign(&canonical_signing_bytes(&st).unwrap())
            .to_bytes());
        attestations.push(Attestation {
            statement: st,
            signature,
        });
    }
    let policy = BuilderPolicy {
        schema_version: SCHEMA_VERSION,
        minimum_operators: 2,
        max_statement_ttl: 500,
        builders,
    };
    let good = AttestationBundle {
        schema_version: SCHEMA_VERSION,
        attestations,
    };
    let mut bad = good.clone();
    bad.attestations[2].statement.issued_at = 101;
    (identity(), policy, good, bad)
}

fn verify_batch(n: u64) -> u64 {
    let (expected, policy, good, bad) = fixture();
    let mut h = 0xcbf2_9ce4_8422_2325;
    for i in 0..VERIFY_RUNS {
        for bundle in [&good, &bad] {
            let r = verify_attestations(&expected, bundle, &policy, 150 + (n + i) % 3);
            h = fnv(h, &[u8::from(r.accepted), r.diagnostics.len() as u8]);
            for d in &r.diagnostics {
                h = fnv(h, d.message.as_bytes());
            }
        }
    }
    h
}

const ORGS: [&str; 10] = [
    "vesper-core",
    "vesper_core",
    "vesper-c0re",
    "acme",
    "acme1",
    "acne",
    "tools",
    "t00ls",
    "vesper-io",
    "std-x",
];
const PKGS: [&str; 20] = [
    "bytes",
    "byets",
    "bytes-fast",
    "byte5",
    "lz4-pure",
    "lz4pure",
    "uring-reactor",
    "reactor",
    "json",
    "j5on",
    "serde",
    "serde-derive",
    "http",
    "http2",
    "tls",
    "tls-rustls",
    "crypto",
    "crypt0",
    "log",
    "logger",
];

fn catalog() -> Vec<CatalogEntry> {
    let mut c = Vec::new();
    for org in ORGS {
        for pkg in PKGS {
            c.push(CatalogEntry {
                package: PackageName::parse(format!("@{org}/{pkg}")).unwrap(),
                org_created_at: 1_000 + c.len() as u64 * 100_000,
                trust_cost: (c.len() % 70) as u32,
                attested_oidc: c.len() % 9 != 0,
            });
        }
    }
    c
}

fn screen_batch(n: u64) -> u64 {
    let catalog = catalog();
    let policy = QuarantinePolicy::default();
    let mut h = 0xcbf2_9ce4_8422_2325;
    for i in 0..SCREEN_RUNS {
        let k = (n * 31 + i * 7) as usize;
        let candidate = if i % 2 == 0 {
            catalog[k % catalog.len()].package.clone()
        } else {
            PackageName::parse(format!("@{}/{}x", ORGS[k % 10], PKGS[k % 20])).unwrap()
        };
        let r = screen_package_admission(&candidate, &catalog, &policy, 20_000_000);
        h = fnv(h, &[r.findings.len() as u8]);
        for f in &r.findings {
            h = fnv(h, f.message.as_bytes());
        }
    }
    h
}

fn signing_batch(n: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325;
    for i in 0..SIGNING_RUNS {
        let bytes = canonical_signing_bytes(&statement("key-a", n + i)).unwrap();
        h = fnv(h, &bytes);
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
            "verify" => verify_batch(n),
            "screen" => screen_batch(n),
            "signing" => signing_batch(n),
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
        "verify" => (4, 2 * VERIFY_RUNS),
        "screen" => (4, SCREEN_RUNS),
        "signing" => (4, SIGNING_RUNS),
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
                "usage: abba worker <verify|screen|signing> | abba drive <verify|screen|signing> <rust-worker> <vesper-worker>"
            );
            std::process::exit(2);
        }
    }
}
