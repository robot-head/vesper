//! ABBA benchmark of vesper-fmt's Vesper port (`vesper_fmt` with
//! `vesper-port`) against the frozen Rust seed (`fmt_seed`), both linked into
//! one binary. Not run in CI; scripts/bench.sh drives it and the numbers are
//! in docs/parallel-results/self-host-fmt-highlight.md.
//!
//! - `abba <workload>`: vesper-bench ABBA (Rust is A, Vesper is B).
//! - `once <workload> <rust|vesper> <batches>`: run batches of one arm, for
//!   callgrind instruction counts.
//!
//! Workloads: `largest` (format the largest `.ves` file of the repository,
//! crates/vesper-ebpf/vesper/src/verifier.ves) and `corpus` (format every
//! repository `.ves` file outside the macro profile, which the port hands to
//! Rust). Run from the repository root.
use std::hint::black_box;
use std::path::{Path, PathBuf};

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | ".git" | "node_modules")
                && !name.starts_with("bazel-")
            {
                sources(&path, out);
            }
        } else if name.ends_with(".ves") {
            out.push(path);
        }
    }
}

fn workload(name: &str) -> Vec<String> {
    match name {
        "largest" => vec![
            std::fs::read_to_string("crates/vesper-ebpf/vesper/src/verifier.ves")
                .expect("run from the repository root"),
        ],
        "corpus" => {
            let mut paths = vec![];
            sources(Path::new("."), &mut paths);
            paths.sort();
            paths
                .iter()
                .filter_map(|p| std::fs::read_to_string(p).ok())
                .filter(|s| {
                    vesper_fmt::port_format_source(s, vesper_fmt::Limits::default()).is_some()
                })
                .collect()
        }
        _ => panic!("unknown workload {name}"),
    }
}

fn digest(output: &str) -> u64 {
    output.len() as u64 ^ output.bytes().map(u64::from).sum::<u64>()
}

fn run(name: &str, arm: &str) -> Box<dyn FnMut() -> u64> {
    let input = workload(name);
    match arm {
        "rust" => Box::new(move || {
            input
                .iter()
                .map(
                    |s| match fmt_seed::format_source(black_box(s), fmt_seed::Limits::default()) {
                        Ok(r) => digest(&r.output),
                        Err(_) => 1,
                    },
                )
                .fold(0, u64::wrapping_add)
        }),
        "vesper" => Box::new(move || {
            input
                .iter()
                .map(|s| {
                    match vesper_fmt::port_format_source(
                        black_box(s),
                        vesper_fmt::Limits::default(),
                    )
                    .expect("a source the port formats")
                    {
                        Ok(r) => digest(&r.output),
                        Err(_) => 1,
                    }
                })
                .fold(0, u64::wrapping_add)
        }),
        _ => panic!("unknown arm {arm}"),
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
                "ABBA {workload}: {} epochs; median per op: rust {:.2} ms, vesper {:.2} ms; \
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
        _ => {
            eprintln!("usage: abba abba <workload> | abba once <workload> <rust|vesper> <batches>")
        }
    }
}
