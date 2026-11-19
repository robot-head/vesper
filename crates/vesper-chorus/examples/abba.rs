//! ABBA benchmark of the replaced Rust implementation and the Vesper
//! library of the choreography front end (port wave P3b lane 6, #1068).
//!
//! - `project`: parse and project every role of a fixed set of
//!   choreographies (the textbook examples, the Raft protocols and larger
//!   generated ones with nested choices, loops and third parties).
//! - `explore`: the bounded asynchronous exploration of a smaller set.
//!
//! The Rust arm is this example built at a commit that still has the Rust
//! implementation (scripts/bench.sh), so the arms cannot share a process. Each arm is a worker process (this example
//! built one way or the other) that runs one batch of the workload per
//! request on stdin; the driver times each request with `vesper-bench`'s
//! `run_abba`, so both arms carry the same pipe round trip. Every batch
//! returns a digest of its results, and the driver checks that both builds
//! agree. `abba once <workload> <batches>` runs batches without a driver,
//! for callgrind.
//!
//! ```text
//! crates/vesper-chorus/scripts/bench.sh   # builds both, runs both workloads
//! abba worker <project|explore>           # one arm
//! abba drive <project|explore> <rust-worker> <vesper-worker>
//! ```
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_chorus::check::{ExploreLimits, explore};
use vesper_chorus::raft::{APPEND_CHOREOGRAPHY, SNAPSHOT_CHOREOGRAPHY, VOTE_CHOREOGRAPHY};
use vesper_chorus::{parse_choreography, project_all};

const PROJECT_SOURCES: usize = 24;
const EXPLORE_SOURCES: usize = 6;

const TWO_BUYER: &str = "roles B1, B2, S;
B1 -> S: Title(String); S -> B1: Quote(u64); S -> B2: Quote(u64); B1 -> B2: Share(u64);
B2 -> S { Ok() { B2 -> S: Address(String); S -> B2: Date(u64); } Quit() { } }";

const STREAM: &str = "roles P, C, L;
rec Loop { P -> C { More(u64) { C -> L: Seen(u64); continue Loop; } Stop() { C -> L: Closed(); } } }";

/// A choreography over `roles` roles: `steps` rounds, each a choice of the
/// leader with three arms that every other role learns by label, inside a
/// loop, so projection merges offers for every third party.
fn generated(roles: usize, steps: usize, seed: u64) -> String {
    let names: Vec<String> = (0..roles).map(|i| format!("R{i}")).collect();
    let mut s = format!("roles {};\nrec Top {{\n", names.join(", "));
    let mut x = seed;
    for step in 0..steps {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let d = 1 + (x >> 33) as usize % (roles - 1);
        s.push_str(&format!("R0 -> R{d} {{\n"));
        for arm in ["Fast", "Slow", "Skip"] {
            s.push_str(&format!("{arm}{step}(u64) {{\n"));
            for r in 1..roles {
                if r != d {
                    s.push_str(&format!("R{d} -> R{r}: {arm}{step}N(u64);\n"));
                }
            }
            if arm == "Slow" {
                s.push_str(&format!("R{d}: work{step};\nR{d} -> R0: Done{step}();\n"));
            }
            s.push_str("}\n");
        }
        s.push_str("}\n");
    }
    s.push_str("R0 -> R1 { Again() { ");
    for r in 2..roles {
        s.push_str(&format!("R1 -> R{r}: Again();\n"));
    }
    s.push_str("continue Top; } Stop() { ");
    for r in 2..roles {
        s.push_str(&format!("R1 -> R{r}: Stop();\n"));
    }
    s.push_str("} }\n}\n");
    s
}

fn sources(n: usize, explore: bool) -> Vec<String> {
    let mut v = vec![
        TWO_BUYER.to_owned(),
        STREAM.to_owned(),
        VOTE_CHOREOGRAPHY.to_owned(),
        APPEND_CHOREOGRAPHY.to_owned(),
        SNAPSHOT_CHOREOGRAPHY.to_owned(),
    ];
    let mut seed = 1;
    while v.len() < n {
        let (roles, steps) = if explore {
            (3, 2)
        } else {
            (4 + seed as usize % 9, 2 + seed as usize % 3)
        };
        v.push(generated(roles, steps, seed));
        seed += 1;
    }
    v.truncate(n);
    v
}

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn project_batch(srcs: &[String]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325;
    for s in srcs {
        let c = parse_choreography(s).expect("fixture parses");
        let all = project_all(&c).expect("fixture projects");
        for l in &all {
            h = fnv(h, l.render(&c).as_bytes());
        }
    }
    h
}

fn explore_batch(srcs: &[String]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325;
    let limits = ExploreLimits {
        max_states: 200_000,
        max_queue: 2,
    };
    for s in srcs {
        let c = parse_choreography(s).expect("fixture parses");
        let r = explore(&c, limits);
        h = fnv(h, format!("{r:?}").as_bytes());
    }
    h
}

fn batch(workload: &str, project: &[String], expl: &[String]) -> u64 {
    match workload {
        "project" => project_batch(project),
        "explore" => explore_batch(expl),
        other => panic!("unknown workload {other}"),
    }
}

/// Serve batches: each stdin line `go <n>` runs one batch and answers with
/// its digest.
fn worker(workload: &str) {
    let project = sources(PROJECT_SOURCES, false);
    let expl = sources(EXPLORE_SOURCES, true);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.expect("read request");
        assert!(line.starts_with("go "), "request `go <n>`");
        let digest = batch(workload, &project, &expl);
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
        "project" => (16, PROJECT_SOURCES as u64),
        "explore" => (8, EXPLORE_SOURCES as u64),
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
    let unit = "choreography";
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
        ["once", workload, n] => {
            let project = sources(PROJECT_SOURCES, false);
            let expl = sources(EXPLORE_SOURCES, true);
            let mut h = 0u64;
            for _ in 0..n.parse::<u32>().expect("batches") {
                h = h.wrapping_add(batch(workload, &project, &expl));
            }
            println!("{} {workload} digest {h:x}", vesper_chorus::IMPLEMENTATION);
        }
        _ => {
            eprintln!(
                "usage: abba worker <project|explore> | abba drive <project|explore> <rust-worker> <vesper-worker> | abba once <project|explore> <batches>"
            );
            std::process::exit(2);
        }
    }
}
