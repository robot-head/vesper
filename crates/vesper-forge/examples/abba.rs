//! ABBA benchmark of vesper-forge's Vesper port (`vesper_forge` built with
//! `vesper-port`) against the frozen Rust seed (`forge_seed`), both linked
//! into one binary. Not run in CI; scripts/bench.sh drives it and the
//! numbers are in docs/parallel-results/self-host-forge.md.
//!
//! - `abba <workload>`: vesper-bench ABBA (Rust is A, Vesper is B).
//! - `once <workload> <rust|vesper> <batches>`: run batches of one arm, for
//!   callgrind instruction counts.
//!
//! Workloads (each operation resolves fresh copies of the same tokens on
//! one host):
//! - `corpus`: the first 2,000 cases of the random corpus that resolve
//!   (tests/corpus), about 60 tokens each: expansions with splices and
//!   claims inside a caller.
//! - `macro`: one hygienic expansion, a 220-token quotation with nested
//!   `let`s, `handle` blocks and splices of caller syntax.
//! - `chain`: a 7,000-token caller fragment of 40 blocks of 25 nested
//!   `let`s with calls of aliases and globals.
use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, run_abba};

#[macro_use]
#[path = "../tests/corpus/mod.rs"]
mod corpus;

use corpus::{Case, case};

fn chain_source() -> String {
    // 40 blocks of 25 nested `let`s (the fragment's nesting limit is 128),
    // reusing the same names in every block.
    let mut s = String::from("{ ");
    for _ in 0..40 {
        s.push_str("{ ");
        for i in 0..25 {
            match i % 4 {
                0 => s.push_str(&format!("let v{i} = print; ")),
                1 => s.push_str(&format!("let v{i} = v{}(1, \"s\"); ", i - 1)),
                2 => s.push_str(&format!("let v{i} = hash_into(v{}); ", i - 1)),
                _ => s.push_str(&format!("let v{i} = (v{})(v{}); ", i - 3, i - 2)),
            }
        }
        s.push_str("v0(v24) }; ");
    }
    s.push('}');
    s
}

fn macro_text() -> String {
    let mut s = String::new();
    for i in 0..10 {
        s.push_str(&format!(
            "let t{i} = hash_into($, 1); handle Net with tcp {{ phone_home(t{i}, $); x }}; "
        ));
    }
    s.push_str("hash_into($)");
    s
}

macro_rules! workloads {
    ($module:ident, $krate:ident, $world:ident) => {
        world!($world, $krate, crate::Case);

        pub mod $module {
            use ::$krate::*;

            pub struct Input {
                pub host: HygieneHost,
                pub fragments: Vec<Vec<HostToken>>,
            }

            fn single(mut h: HygieneHost, tokens: Vec<HostToken>) -> Input {
                if let Err(e) = h.resolve(tokens.clone()) {
                    panic!("the workload does not resolve: {e:?}");
                }
                Input {
                    host: h,
                    fragments: vec![tokens],
                }
            }

            pub fn input(workload: &str) -> Input {
                match workload {
                    "corpus" => {
                        let (mut h, _) = super::$world::prepare(&crate::Case::default());
                        let mut fragments = Vec::new();
                        let mut s = 0;
                        while fragments.len() < 2000 {
                            let c = crate::case(s);
                            s += 1;
                            if c.tight {
                                continue;
                            }
                            if let Ok(t) = super::$world::prepare_on(&mut h, &c) {
                                if h.resolve(t.clone()).is_ok() {
                                    fragments.push(t);
                                }
                            }
                        }
                        Input { host: h, fragments }
                    }
                    "macro" => {
                        let (mut h, _) = super::$world::prepare(&crate::Case::default());
                        let app = PackageId(0);
                        let mut program = h.lex_source(app, "let tmp = 1; let x = 2; {").unwrap();
                        let args = ["tmp", "x(1)", "print"]
                            .iter()
                            .map(|a| h.lex_source(app, a).unwrap())
                            .collect();
                        let macro_def = DefId {
                            package: PackageId(2),
                            item: 0,
                        };
                        let e = h
                            .begin_expansion(macro_def, InvocationSite::Source(app), args)
                            .unwrap();
                        let mut q = Quotation::new();
                        for (i, part) in crate::macro_text().split('$').enumerate() {
                            if i > 0 {
                                q = q.splice(e.splices[i % e.splices.len()]);
                            }
                            q = q.text(part);
                        }
                        program.extend(h.finish_quotation(e.id, &q).unwrap());
                        program.extend(h.lex_source(app, "}").unwrap());
                        single(h, program)
                    }
                    "chain" => {
                        let (mut h, _) = super::$world::prepare(&crate::Case::default());
                        let tokens = h.lex_source(PackageId(0), &crate::chain_source()).unwrap();
                        single(h, tokens)
                    }
                    _ => panic!("unknown workload {workload}"),
                }
            }

            pub fn run(input: &mut Input) -> u64 {
                let mut n = 0u64;
                for f in &input.fragments {
                    let r = input.host.resolve(f.clone()).unwrap();
                    n += (r.binders.len() + r.references.len() + r.operations.len()) as u64;
                }
                std::hint::black_box(n)
            }

            pub fn tokens(input: &Input) -> usize {
                input.fragments.iter().map(Vec::len).sum()
            }
        }
    };
}
workloads!(rust_build, forge_seed, rust_world);
workloads!(port_build, vesper_forge, port_world);

fn run(workload: &str, arm: &str) -> (usize, Box<dyn FnMut() -> u64>) {
    match arm {
        "rust" => {
            let mut input = rust_build::input(workload);
            (
                rust_build::tokens(&input),
                Box::new(move || rust_build::run(&mut input)),
            )
        }
        "vesper" => {
            let mut input = port_build::input(workload);
            (
                port_build::tokens(&input),
                Box::new(move || port_build::run(&mut input)),
            )
        }
        _ => panic!("unknown arm {arm}"),
    }
}

struct Bench(Box<dyn FnMut() -> u64>);
impl BenchCase for Bench {
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

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn main() {
    assert_eq!(
        vesper_forge::IMPLEMENTATION,
        "vesper",
        "build with --features vesper-port"
    );
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("once") => {
            let (_, mut f) = run(&args[2], &args[3]);
            let batches: usize = args[4].parse().unwrap();
            let mut sum = 0u64;
            for _ in 0..batches {
                sum = sum.wrapping_add(black_box(f()));
            }
            println!("{sum}");
        }
        Some("abba") => {
            let workload = args[2].as_str();
            let ((tr, mut rust), (tp, mut port)) = (run(workload, "rust"), run(workload, "vesper"));
            assert_eq!(tr, tp, "{workload}: the workloads differ");
            assert_eq!(rust(), port(), "{workload}: the builds disagree");
            let config = RunConfig {
                epochs: 30,
                warmup_epochs: 2,
                operations_per_window: 16,
                max_total_calls: 100_000,
                max_elapsed_nanos: 600_000_000_000,
                layout_seed: 1,
            };
            let report = run_abba(config, &mut Bench(rust), &mut Bench(port)).expect("ABBA run");
            let pairs = report.pairs();
            let mut ratios: Vec<f64> = pairs
                .iter()
                .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
                .collect();
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
            let (a, b) = (median(&mut a), median(&mut b));
            println!(
                "{workload}: {tr} tokens, rust {:.1} us, vesper {:.1} us, vesper/rust {:.2}x (median of {} pairs)",
                a / 1e3,
                b / 1e3,
                median(&mut ratios),
                pairs.len()
            );
        }
        _ => {
            eprintln!(
                "usage: abba abba <corpus|macro|chain> | abba once <workload> <rust|vesper> <batches>"
            );
            std::process::exit(2);
        }
    }
}
