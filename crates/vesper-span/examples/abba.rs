//! ABBA benchmark of vesper-span's Vesper port (`vesper_span`, this
//! package) against the frozen Rust seed (`span_seed`), both linked into
//! one binary. Not run in CI; scripts/bench.sh drives it and the numbers
//! are in docs/parallel-results/self-host-toolchain-pilots.md.
//!
//! - `abba <workload>`: vesper-bench ABBA (Rust is A, Vesper is B).
//! - `once <workload> <rust|vesper> <batches>`: run batches of one arm, for
//!   callgrind instruction counts.
//!
//! Workloads: `lookup` (4096 `position` and `offset` calls on a 64 KiB
//! mixed-script source), `index` (`LineIndex::new` of that source) and
//! `edits` (`SourceEdits::new` and `apply` of 64 edits).
use std::{hint::black_box, sync::Arc};

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

const OPS: usize = 4096;

fn source() -> String {
    let pieces = [
        "let x = 1;",
        " 名前",
        "😀",
        "\r\n",
        "\n",
        "\tfoo(bar, baz)",
        "é",
    ];
    let mut s = String::new();
    let mut i = 0usize;
    while s.len() < 64 * 1024 - 16 {
        s.push_str(pieces[(i * 7 + i / 3) % pieces.len()]);
        i += 1;
    }
    s
}

macro_rules! workloads {
    ($module:ident, $krate:ident) => {
        pub mod $module {
            use super::*;
            use ::$krate::*;

            pub struct Input {
                pub index: LineIndex,
                pub offsets: Vec<usize>,
                pub text: Arc<str>,
                pub edits: Vec<SourceEdit>,
            }

            pub fn input() -> Input {
                let text: Arc<str> = Arc::from(source().as_str());
                let index = LineIndex::new(text.clone(), Limits::default()).unwrap();
                let mut offsets = Vec::with_capacity(OPS);
                let mut x = 0x9e37_79b9_7f4a_7c15u64;
                while offsets.len() < OPS {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let at = (x % text.len() as u64) as usize;
                    if index.position(at).is_ok() {
                        offsets.push(at);
                    }
                }
                let edits = spread_edits(&offsets);
                Input {
                    index,
                    offsets,
                    text,
                    edits,
                }
            }

            /// 64 insertions spread over the file: the offsets are random, so
            /// sort them first and keep ones at least 8 bytes apart.
            fn spread_edits(offsets: &[usize]) -> Vec<SourceEdit> {
                let mut sorted = offsets.to_vec();
                sorted.sort_unstable();
                sorted.dedup();
                let step = (sorted.len() / 64).max(1);
                let mut edits: Vec<SourceEdit> = Vec::new();
                for &at in sorted.iter().step_by(step) {
                    let spaced = edits.last().is_none_or(|e| at > e.span.start + 8);
                    if spaced && edits.len() < 64 {
                        edits.push(SourceEdit {
                            span: ByteSpan { start: at, end: at },
                            expected: String::new(),
                            replacement: "/* edit */".into(),
                        });
                    }
                }
                assert_eq!(edits.len(), 64, "the edits workload needs 64 edits");
                edits
            }

            pub fn lookup(input: &Input) -> u64 {
                let mut sum = 0u64;
                for &at in &input.offsets {
                    let p = input.index.position(at).unwrap();
                    sum = sum.wrapping_add(input.index.offset(p).unwrap() as u64);
                    sum = sum.wrapping_add(u64::from(p.character));
                }
                black_box(sum)
            }

            pub fn index(input: &Input) -> u64 {
                let index = LineIndex::new(input.text.clone(), Limits::default()).unwrap();
                black_box(index.position(input.text.len()).unwrap().line as u64)
            }

            pub fn edits(input: &Input) -> u64 {
                // Each edit scans the 64 KiB file twice: 8 MiB of work for 64.
                let limits = Limits {
                    max_work: 16 * 1024 * 1024,
                    ..Limits::default()
                };
                let batch = SourceEdits::new(&input.index, &input.edits, limits).unwrap();
                black_box(batch.apply().unwrap().len() as u64)
            }
        }
    };
}
workloads!(rust_build, span_seed);
workloads!(port_build, vesper_span);

fn run(workload: &str, arm: &str) -> Box<dyn FnMut() -> u64> {
    macro_rules! arm {
        ($m:ident) => {{
            let input = $m::input();
            match workload {
                "lookup" => Box::new(move || $m::lookup(&input)),
                "index" => Box::new(move || $m::index(&input)),
                "edits" => Box::new(move || $m::edits(&input)),
                _ => panic!("unknown workload {workload}"),
            }
        }};
    }
    match arm {
        "rust" => arm!(rust_build),
        "vesper" => arm!(port_build),
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
            let (mut rust, mut port) = (run(workload, "rust"), run(workload, "vesper"));
            assert_eq!(rust(), port(), "{workload}: the builds disagree");
            let config = RunConfig {
                epochs: 30,
                warmup_epochs: 2,
                operations_per_window: 16,
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
                "ABBA {workload}: {} epochs; median per op: rust {:.0} ns, vesper {:.0} ns; \
                 Vesper/Rust median {:.2} (min {:.2}, max {:.2})",
                pairs.len(),
                a[mid],
                b[mid],
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
