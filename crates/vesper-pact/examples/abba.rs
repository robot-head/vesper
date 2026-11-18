//! ABBA benchmark of the Pact engine, comparing the Rust build with the
//! Vesper port (port wave P3a lane 3, #1038).
//!
//! One binary holds one build, so the arms are two binaries:
//!
//! ```sh
//! cargo build --release -p vesper-pact-seed --example abba_rust
//! cp target/release/examples/abba_rust /tmp/rust_arm
//! cargo build --release -p vesper-pact --features vesper-port --example abba
//! cp target/release/examples/abba /tmp/port_arm
//! /tmp/rust_arm driver /tmp/rust_arm /tmp/port_arm
//! ```
//!
//! A pass validates the program (`Program::new`), checks coherence, solves
//! the workload's queries with one fresh `Solver` and rechecks every proof
//! with `check_derivation`. `PACT_ABBA_WORLD` picks the workload: `fixture`
//! (default) is tests/fixtures/world.json with the queries below, `large` a
//! generated world of `PACT_ABBA_SIZE` (default 64) generic type
//! constructors, each with a canonical impl whose where-clause recurses on
//! its parameter, and queries over nested types of depth 12.
//!
//! `window N` prints the nanoseconds of N passes after one warm-up pass,
//! `once` runs one pass (for callgrind), and `driver A B` runs epochs of
//! windows in the order A B B A, pairs each epoch's totals and reports the
//! median B/A with `vesper_bench::analyze_pairs`.
use std::process::Command;
use std::time::Instant;
use vesper_pact::*;

const PASSES: usize = 20;
const EPOCHS: usize = 9;

fn size() -> usize {
    std::env::var("PACT_ABBA_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}

fn large() -> bool {
    std::env::var("PACT_ABBA_WORLD").is_ok_and(|w| w == "large")
}

fn ctor(id: u32, args: Vec<GenericArg>) -> Ty {
    Ty::Ctor {
        ctor: TypeCtorId(id),
        args,
    }
}

fn implements(trait_id: u32, self_ty: Ty) -> Goal {
    Goal::implements(
        TraitRef {
            trait_id: TraitId(trait_id),
            self_ty,
            args: vec![],
        },
        Brand::Canonical,
    )
}

fn query(goal: Goal) -> QueryDto {
    QueryDto::new(Env::default(), vec![], goal)
}

/// The fixture world and queries over its `Clone`, `Send` and `Ord`
/// impls: nested `Vec`, `Option` and `List` types, one generic query
/// under a placeholder and one with an inference variable.
fn fixture() -> (ProgramDto, Vec<QueryDto>) {
    let dto: ProgramDto =
        serde_json::from_str(include_str!("../tests/fixtures/world.json")).unwrap();
    let source: QueryDto =
        serde_json::from_str(include_str!("../tests/fixtures/fold_source_query.json")).unwrap();
    let (u64_, vec, option, list) = (0, 2, 3, 6);
    let mut queries = vec![source];
    for depth in 1..=6 {
        for (trait_id, wrap) in [(0, vec), (1, option), (2, list), (0, option)] {
            let mut ty = ctor(u64_, vec![]);
            for _ in 0..depth {
                ty = ctor(wrap, vec![GenericArg::Ty(ty)]);
            }
            queries.push(query(implements(trait_id, ty)));
        }
    }
    queries.push(QueryDto::new(
        Env {
            placeholders: vec![PlaceholderDecl::ty(None)],
            assumptions: vec![implements(0, Ty::Var(Var::Placeholder(0)))],
            model_scopes: vec![],
        },
        vec![],
        implements(
            0,
            ctor(vec, vec![GenericArg::Ty(Ty::Var(Var::Placeholder(0)))]),
        ),
    ));
    queries.push(QueryDto::new(
        Env::default(),
        vec![ParamKind::Type],
        implements(0, ctor(vec, vec![GenericArg::Ty(Ty::Var(Var::Infer(0)))])),
    ));
    (dto, queries)
}

/// The generated world: type 0 is a leaf, types 1..=n take one type
/// parameter (mode joined from it), and trait 0 has a canonical impl for
/// the leaf and one per generic type that requires the parameter's impl.
fn generated(n: usize) -> (ProgramDto, Vec<QueryDto>) {
    let mut p = ProgramDto::new();
    p.packages = vec![PackageDecl { id: PackageId(0) }];
    p.type_ctors.push(TypeCtorDecl {
        id: TypeCtorId(0),
        package: PackageId(0),
        params: vec![],
        mode: ModeScheme {
            base: Mode::Copy,
            joins: vec![],
        },
    });
    for i in 1..=n {
        p.type_ctors.push(TypeCtorDecl {
            id: TypeCtorId(i as u32),
            package: PackageId(0),
            params: vec![ParamKind::Type],
            mode: ModeScheme {
                base: if i % 3 == 0 { Mode::Affine } else { Mode::Copy },
                joins: vec![0],
            },
        });
    }
    p.traits.push(TraitDecl {
        id: TraitId(0),
        package: PackageId(0),
        params: vec![],
        assoc: vec![],
        coinductive: false,
    });
    p.impls.push(ImplDecl {
        id: ImplId(0),
        decl: ConformanceDecl {
            package: PackageId(0),
            params: vec![],
            trait_ref: TraitRef {
                trait_id: TraitId(0),
                self_ty: ctor(0, vec![]),
                args: vec![],
            },
            where_clauses: vec![],
            assoc_values: vec![],
        },
    });
    let param = || Ty::Var(Var::Bound(0));
    for i in 1..=n {
        p.impls.push(ImplDecl {
            id: ImplId(i as u32),
            decl: ConformanceDecl {
                package: PackageId(0),
                params: vec![ParamKind::Type],
                trait_ref: TraitRef {
                    trait_id: TraitId(0),
                    self_ty: ctor(i as u32, vec![GenericArg::Ty(param())]),
                    args: vec![],
                },
                where_clauses: vec![implements(0, param())],
                assoc_values: vec![],
            },
        });
    }
    let mut queries = vec![];
    for q in 0..n {
        let mut ty = ctor(0, vec![]);
        for k in 0..12 {
            let id = 1 + (q * 7 + k * 5) % n;
            ty = ctor(id as u32, vec![GenericArg::Ty(ty)]);
        }
        queries.push(query(Goal::ModeBound {
            ty: ty.clone(),
            bound: ModeTerm::Const(Mode::Affine),
        }));
        queries.push(query(implements(0, ty)));
    }
    (p, queries)
}

fn workload() -> (ProgramDto, Vec<QueryDto>) {
    if large() {
        generated(size())
    } else {
        fixture()
    }
}

fn pass(dto: &ProgramDto, queries: &[QueryDto]) -> usize {
    let program = Program::new(dto.clone()).unwrap();
    let mut n = check_coherence(&program).len();
    let mut solver = Solver::new(&program);
    for q in queries {
        if let Outcome::Proven(s) = solver.solve(q).unwrap() {
            check_derivation(&program, &q.env, &s.derivation).unwrap();
            n += 1;
        }
    }
    n + solver.table_len()
}

fn window(passes: usize) -> u128 {
    let (dto, queries) = workload();
    std::hint::black_box(pass(&dto, &queries));
    let start = Instant::now();
    for _ in 0..passes {
        std::hint::black_box(pass(&dto, &queries));
    }
    start.elapsed().as_nanos()
}

fn run(bin: &str) -> u64 {
    let out = Command::new(bin)
        .args(["window", &PASSES.to_string()])
        .output()
        .expect("run arm");
    assert!(out.status.success(), "{bin} failed");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("window") => println!("{}", window(args[2].parse().unwrap())),
        Some("once") => {
            let (dto, queries) = workload();
            println!("{} ({})", pass(&dto, &queries), IMPLEMENTATION);
        }
        Some("driver") => {
            let (a, b) = (&args[2], &args[3]);
            let mut pairs = Vec::new();
            let mut ratios = Vec::new();
            for epoch in 0..EPOCHS {
                let a1 = run(a);
                let b1 = run(b);
                let b2 = run(b);
                let a2 = run(a);
                let (ta, tb) = (a1 + a2, b1 + b2);
                ratios.push(tb as f64 / ta as f64);
                println!(
                    "epoch {epoch}: A {:.1} us/pass, B {:.1} us/pass, B/A {:.3}",
                    ta as f64 / (2 * PASSES) as f64 / 1e3,
                    tb as f64 / (2 * PASSES) as f64 / 1e3,
                    tb as f64 / ta as f64
                );
                pairs.push(vesper_bench::PairedSample::new(ta, tb, 2 * PASSES as u64).unwrap());
            }
            ratios.sort_by(f64::total_cmp);
            println!(
                "median B/A {:.3} ({})",
                ratios[EPOCHS / 2],
                if large() {
                    format!("generated world, {} types", size())
                } else {
                    "fixture world".to_owned()
                }
            );
            let analysis = vesper_bench::analyze_pairs(&pairs, 10_000).unwrap();
            println!(
                "2x gate (B at most +10000 bp over A): {:?}; median regression bp {:?}",
                analysis.decision,
                analysis.median_upper.regression_bp_bounds()
            );
        }
        _ => {
            eprintln!("usage: abba window N | once | driver RUST_BIN PORT_BIN");
            std::process::exit(2);
        }
    }
}
