//! ABBA benchmark of vesper-diagnostics' Vesper port (`vesper_diagnostics`,
//! this package) against the frozen Rust seed (`diagnostics_seed`), both
//! linked into one binary. Not run in CI; scripts/bench.sh drives it and the
//! numbers are in docs/parallel-results/self-host-diagnostics.md.
//!
//! - `abba <workload>`: vesper-bench ABBA (Rust is A, Vesper is B).
//! - `once <workload> <rust|vesper> <batches>`: run batches of one arm, for
//!   callgrind instruction counts.
//!
//! Every workload uses one 200-label report: 25 diagnostics of 8 labels
//! (some multi-line, overlapping, with wide chars and tabs) over a 9 KiB
//! source. A single diagnostic with 200 labels exceeds the crate's work
//! budget (it charges the fourth power of a diagnostic's label count) in
//! both builds.
//!
//! Workloads: `prepare` (`prepare`), `terminal` (`Prepared::terminal`),
//! `colored` (`terminal_colored`), `json` (`lsp_json`) and `lsp` (`to_lsp`,
//! which the port answers by parsing its JSON).
use std::{hint::black_box, sync::Arc};

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_core::ast::Diagnostic;
use vesper_span::LineIndex;

fn source() -> String {
    let pieces = [
        "let value = compute(a, b);",
        "\tif 名前 == 😀 { return; }",
        "    match x { Some(y) => y, None => 0 }",
        "// コメント with ｗｉｄｅ text",
        "fn f(e\u{301}: u64) -> u64 { e\u{301} + 1 }",
    ];
    let mut s = String::new();
    let mut i = 0usize;
    while s.len() < 9 * 1024 {
        s.push_str(pieces[(i * 7 + i / 3) % pieces.len()]);
        s.push_str(if i % 5 == 4 { "\r\n" } else { "\n" });
        i += 1;
    }
    s
}

/// The char boundaries of `s` outside a CRLF.
fn boundaries(s: &str) -> Vec<usize> {
    (0..=s.len())
        .filter(|&i| {
            s.is_char_boundary(i)
                && !(i > 0 && s.as_bytes()[i - 1] == b'\r' && s.as_bytes().get(i) == Some(&b'\n'))
        })
        .collect()
}

fn diagnostics(s: &str) -> Vec<Diagnostic> {
    let valid = boundaries(s);
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |n: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % n as u64) as usize
    };
    (0..25)
        .map(|d| {
            // Each diagnostic covers its own stretch of about 360 bytes.
            let base = d * valid.len() / 25;
            let width = valid.len() / 25;
            let mut span = |long: bool| {
                let a = base + next(width);
                let len = if long { 40 + next(120) } else { 1 + next(12) };
                let b = (a + len).min(valid.len() - 1);
                valid[a]..valid[b]
            };
            let mut diagnostic =
                Diagnostic::new(&format!("E{d:04}"), "value moved here", span(false));
            for l in 0..7 {
                diagnostic =
                    diagnostic.with_related(span(l % 3 == 0), format!("label {l} of the report"));
            }
            diagnostic.with_help("Consider borrowing the value.\nThen use it again.")
        })
        .collect()
}

macro_rules! workloads {
    ($module:ident, $krate:ident) => {
        pub mod $module {
            use super::*;
            use ::$krate::*;

            pub struct Input {
                pub index: &'static LineIndex,
                pub data: Vec<Diagnostic>,
                pub prepared: Prepared<'static>,
            }

            pub fn input() -> Input {
                let text = source();
                // Leaked so the prepared report can live beside it.
                let index: &'static LineIndex = Box::leak(Box::new(
                    LineIndex::new(Arc::from(text.as_str()), vesper_span::Limits::default())
                        .unwrap(),
                ));
                let data = diagnostics(&text);
                let labels: usize = data.iter().map(|d| 1 + d.related.len()).sum();
                assert_eq!(labels, 200);
                let prepared = prepare(index, &data, Limits::default()).unwrap();
                Input {
                    index,
                    data,
                    prepared,
                }
            }

            pub fn run(input: &Input, workload: &str) -> u64 {
                let p = &input.prepared;
                match workload {
                    "prepare" => {
                        let p = prepare(input.index, &input.data, Limits::default()).unwrap();
                        black_box(p.data().len() as u64)
                    }
                    "terminal" => black_box(p.terminal("main.vsp").unwrap().len() as u64),
                    "colored" => black_box(p.terminal_colored("main.vsp").unwrap().len() as u64),
                    "json" => black_box(p.lsp_json("file:///main.vsp").unwrap().len() as u64),
                    "lsp" => black_box(p.to_lsp("file:///main.vsp").unwrap().len() as u64),
                    _ => panic!("unknown workload {workload}"),
                }
            }
        }
    };
}
workloads!(rust_build, diagnostics_seed);
workloads!(port_build, vesper_diagnostics);

fn run(workload: &str, arm: &str) -> Box<dyn FnMut() -> u64> {
    let workload = workload.to_string();
    match arm {
        "rust" => {
            let input = rust_build::input();
            Box::new(move || rust_build::run(&input, &workload))
        }
        "vesper" => {
            let input = port_build::input();
            Box::new(move || port_build::run(&input, &workload))
        }
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
            // `json` writes the keys in another order; its length still agrees.
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
