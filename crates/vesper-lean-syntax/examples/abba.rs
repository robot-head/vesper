//! ABBA benchmark of vesper-lean-syntax's Vesper port (`vesper_lean_syntax`,
//! this package) against the frozen Rust seed (`lean_seed`), both linked
//! into one binary. Not run in CI; scripts/bench.sh drives it and the
//! numbers are in docs/parallel-results/self-host-toolchain-pilots.md.
//!
//! - `abba <workload>`: vesper-bench ABBA (Rust is A, Vesper is B).
//! - `once <workload> <rust|vesper> <batches>`: run batches of one arm, for
//!   callgrind instruction counts.
//!
//! Workloads: `term` (render one large term, about 40k nodes and 1 MB of
//! text), `module` (render a module of 400 definitions) and `free`
//! (`Term::free_references` of the large term).
use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

macro_rules! workloads {
    ($module:ident, $krate:ident) => {
        pub mod $module {
            use super::*;
            use ::$krate::*;

            pub struct Input {
                pub term: Term,
                pub module: Module,
                pub params: Vec<Binder>,
            }

            fn id(s: &str) -> Ident {
                Ident::new(s).unwrap()
            }
            fn r(s: &str) -> Term {
                Term::Ref(Name::single(s).unwrap())
            }

            /// A balanced tree of the common node kinds, `depth` levels deep.
            fn tree(depth: u32, k: &mut u64) -> Term {
                *k += 1;
                if depth == 0 {
                    return match *k % 4 {
                        0 => r("x"),
                        1 => Term::Nat(NatLiteral::from(*k)),
                        2 => Term::String(format!("s{k}\n\"q\"")),
                        _ => r("Nat.succ"),
                    };
                }
                let d = depth - 1;
                match *k % 5 {
                    0 => Term::app(r("f"), [tree(d, k), tree(d, k)]),
                    1 => Term::binary(BinaryOp::Add, tree(d, k), tree(d, k)),
                    2 => Term::Lambda {
                        binders: vec![Binder::explicit(id("x"), r("Nat"))],
                        body: Box::new(Term::If {
                            condition: Box::new(tree(d, k)),
                            yes: Box::new(tree(d, k)),
                            no: Box::new(r("x")),
                        }),
                    },
                    3 => Term::Match {
                        scrutinee: Box::new(tree(d, k)),
                        arms: vec![
                            MatchArm {
                                pattern: Pattern::Constructor {
                                    name: Name::single("some").unwrap(),
                                    arguments: vec![Pattern::Bind(id("y"))],
                                },
                                body: tree(d, k),
                            },
                            MatchArm {
                                pattern: Pattern::Wildcard,
                                body: r("y"),
                            },
                        ],
                    },
                    _ => Term::Do {
                        statements: vec![DoStatement::Let {
                            binding: LetBinding {
                                pattern: Pattern::Bind(id("z")),
                                ty: None,
                                value: tree(d, k),
                            },
                            mutable: true,
                        }],
                        result: Box::new(tree(d, k)),
                    },
                }
            }

            pub fn input() -> Input {
                let mut k = 0;
                let term = tree(15, &mut k);
                let declarations = (0..400)
                    .map(|i| {
                        let mut k = i;
                        Declaration::Definition(Definition {
                            name: Name::single(format!("def{i}")).unwrap(),
                            binders: vec![Binder::explicit(id("x"), r("Nat"))],
                            result: r("Nat"),
                            body: tree(6, &mut k),
                            kind: DefinitionKind::Def,
                        })
                    })
                    .collect();
                Input {
                    term,
                    module: Module {
                        imports: vec![ModuleName::new("Init").unwrap()],
                        declarations,
                    },
                    params: vec![Binder::explicit(id("f"), r("Nat"))],
                }
            }

            pub fn term(input: &Input) -> u64 {
                black_box(input.term.render().unwrap().len() as u64)
            }

            pub fn module(input: &Input) -> u64 {
                black_box(input.module.render().unwrap().len() as u64)
            }

            pub fn free(input: &Input) -> u64 {
                black_box(Term::free_references(&input.params, &input.term).len() as u64)
            }
        }
    };
}
workloads!(rust_build, lean_seed);
workloads!(port_build, vesper_lean_syntax);

fn run(workload: &str, arm: &str) -> Box<dyn FnMut() -> u64> {
    macro_rules! arm {
        ($m:ident) => {{
            let input = $m::input();
            match workload {
                "term" => Box::new(move || $m::term(&input)),
                "module" => Box::new(move || $m::module(&input)),
                "free" => Box::new(move || $m::free(&input)),
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
