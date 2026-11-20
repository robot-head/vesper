//! ABBA benchmark of vesper-lint's Vesper port (`vesper_lint` with
//! `vesper-port`) against the frozen Rust seed (`lint_seed`), both linked
//! into one binary. Not run in CI; the numbers are in
//! docs/parallel-results/self-host-lint.md.
//!
//! - `abba <workload>`: vesper-bench ABBA (Rust is A, Vesper is B).
//! - `once <workload> <rust|vesper|encode> <batches>`: run batches of one
//!   arm, for callgrind instruction counts (`encode` is the port's fact
//!   encoding alone, the boundary's share of the Vesper arm).
//!
//! Workloads: `corpus` (`analyze` the checked facts of every repository
//! `.ves` file that checks as one module, tests/support's extraction) and
//! `policy` (`Policy::parse` of every repository `.toml` file). Run from
//! the repository root.
#[path = "../tests/support/mod.rs"]
mod support;

use std::hint::black_box;
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

fn digest(text: &str) -> u64 {
    text.len() as u64 ^ text.bytes().map(u64::from).sum::<u64>()
}

#[inline(never)]
fn rust_corpus(programs: &[lint_seed::CheckedProgram]) -> u64 {
    programs
        .iter()
        .map(
            |p| match lint_seed::analyze(black_box(p), lint_seed::Limits::default()) {
                Ok(r) => digest(&serde_json::to_string(&r.findings).unwrap()) ^ r.work as u64,
                Err(_) => 1,
            },
        )
        .fold(0, u64::wrapping_add)
}

#[inline(never)]
fn vesper_corpus(programs: &[vesper_lint::CheckedProgram]) -> u64 {
    programs
        .iter()
        .map(
            |p| match vesper_lint::analyze(black_box(p), vesper_lint::Limits::default()) {
                Ok(r) => digest(&serde_json::to_string(&r.findings).unwrap()) ^ r.work as u64,
                Err(_) => 1,
            },
        )
        .fold(0, u64::wrapping_add)
}

#[inline(never)]
fn encode_corpus(programs: &[vesper_lint::CheckedProgram]) -> u64 {
    programs
        .iter()
        .map(|p| vesper_lint::port_encoded_words(black_box(p)) as u64)
        .fold(0, u64::wrapping_add)
}

#[inline(never)]
fn rust_policy(sources: &[String]) -> u64 {
    sources
        .iter()
        .map(|s| {
            match lint_seed::policy::Policy::parse(
                black_box(s),
                lint_seed::policy::PolicyLimits::default(),
            ) {
                Ok(p) => p.usage().work as u64,
                Err(e) => digest(e.message),
            }
        })
        .fold(0, u64::wrapping_add)
}

#[inline(never)]
fn vesper_policy(sources: &[String]) -> u64 {
    sources
        .iter()
        .map(|s| {
            match vesper_lint::policy::Policy::parse(
                black_box(s),
                vesper_lint::policy::PolicyLimits::default(),
            ) {
                Ok(p) => p.usage().work as u64,
                Err(e) => digest(e.message),
            }
        })
        .fold(0, u64::wrapping_add)
}

fn run(name: &str, arm: &str) -> Box<dyn FnMut() -> u64> {
    match name {
        "corpus" => {
            let seed: Vec<_> = support::corpus(256 * 1024)
                .into_iter()
                .map(|(_, p)| p)
                .collect();
            let port: Vec<_> = seed.iter().map(support::to_port).collect();
            match arm {
                "rust" => Box::new(move || rust_corpus(&seed)),
                "vesper" => Box::new(move || vesper_corpus(&port)),
                "encode" => Box::new(move || encode_corpus(&port)),
                _ => panic!("unknown arm {arm}"),
            }
        }
        "policy" => {
            let sources: Vec<String> = support::files("toml")
                .iter()
                .filter_map(|p| std::fs::read_to_string(p).ok())
                .collect();
            match arm {
                "rust" => Box::new(move || rust_policy(&sources)),
                "vesper" => Box::new(move || vesper_policy(&sources)),
                _ => panic!("unknown arm {arm}"),
            }
        }
        _ => panic!("unknown workload {name}"),
    }
}

struct Case(Box<dyn FnMut() -> u64>);
impl BenchCase for Case {
    type Input = ();
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<(), ()> {
        Ok(())
    }
    fn run(&mut self, _: ()) -> Result<u64, ()> {
        Ok((self.0)())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("once") => {
            let mut f = run(&args[2], &args[3]);
            let batches: usize = args[4].parse().unwrap();
            let mut sum = 0u64;
            for _ in 0..batches {
                sum = sum.wrapping_add(f());
            }
            println!("{sum}");
        }
        Some("abba") => {
            let workload = args[2].as_str();
            let epochs = std::env::var("ABBA_EPOCHS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30);
            let (mut rust, mut port) = (run(workload, "rust"), run(workload, "vesper"));
            assert_eq!(rust(), port(), "{workload}: the builds disagree");
            let config = RunConfig {
                epochs,
                warmup_epochs: 2,
                operations_per_window: 2,
                max_total_calls: 100_000,
                max_elapsed_nanos: 600_000_000_000,
                layout_seed: 1,
            };
            let report = run_abba(config, &mut Case(rust), &mut Case(port)).expect("ABBA run");
            let pairs = report.pairs();
            let mut ratios: Vec<f64> = pairs
                .iter()
                .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
                .collect();
            ratios.sort_by(f64::total_cmp);
            let per_op =
                |t: u64, p: &vesper_bench::PairedSample| t as f64 / p.operations_per_arm() as f64;
            let mut a: Vec<f64> = pairs
                .iter()
                .map(|p| per_op(p.baseline_total(), p))
                .collect();
            let mut b: Vec<f64> = pairs
                .iter()
                .map(|p| per_op(p.candidate_total(), p))
                .collect();
            a.sort_by(f64::total_cmp);
            b.sort_by(f64::total_cmp);
            let mid = pairs.len() / 2;
            println!(
                "ABBA {workload}: {} epochs; median per op: rust {:.3} ms, vesper {:.3} ms; \
                 Vesper/Rust median {:.2} (min {:.2}, max {:.2})",
                pairs.len(),
                a[mid] / 1e6,
                b[mid] / 1e6,
                ratios[mid],
                ratios[0],
                ratios[ratios.len() - 1]
            );
            let analysis = analyze_pairs(pairs, 10_000).expect("analysis");
            println!("ABBA {workload}: 2x gate: {:?}", analysis.decision);
        }
        _ => eprintln!(
            "usage: abba abba <workload> | abba once <workload> <rust|vesper|encode> <batches>"
        ),
    }
}
