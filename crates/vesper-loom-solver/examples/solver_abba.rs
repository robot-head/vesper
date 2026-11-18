//! ABBA benchmark of `resolve` + `validate_locked` and of
//! `resolve_optimized`, comparing the Rust seed copy with the Vesper port
//! (port wave P1 lane 17, #914).
//!
//! One binary holds one build, so the arms are two binaries:
//!
//! ```sh
//! cargo build --release -p vesper-loom-solver-seed --example solver_abba_rust
//! cp target/release/examples/solver_abba_rust /tmp/rust_arm
//! cargo build --release -p vesper-loom-solver --example solver_abba
//! cp target/release/examples/solver_abba /tmp/port_arm
//! /tmp/rust_arm driver /tmp/rust_arm /tmp/port_arm
//! ```
//!
//! `window N` prints the nanoseconds of N passes over the workload after one
//! warm-up pass. `driver A B` runs epochs of windows in the order A B B A,
//! pairs each epoch's A and B totals and reports the median regression of B
//! over A with `vesper_bench::analyze_pairs`, plus the 2x stop rule of the
//! port plan. `LOOM_ABBA_SIZE` sets the packages per catalog (default 64),
//! `LOOM_ABBA_MODE` the workload: `resolve` (default: resolve, then
//! validate_locked on the result) or `optimize` (resolve_optimized over at
//! most 16 solutions).
use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::time::Instant;
use vesper_loom_calculus::{
    Dependency, Feature, Manifest, Package, PackageName, SCHEMA_VERSION, SystemSelection,
};
use vesper_loom_solver::optimize::{CandidateMetadata, OptimizeRequest, resolve_optimized};
use vesper_loom_solver::{Catalog, ResolveRequest, resolve, validate_locked};

const PASSES: usize = 10;
const EPOCHS: usize = 9;
const CAPS: [&str; 6] = ["Net", "Storage", "Clock", "Fail", "Ffi[LibA]", "Async[io]"];

fn size() -> usize {
    std::env::var("LOOM_ABBA_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}

fn optimize() -> bool {
    std::env::var("LOOM_ABBA_MODE").is_ok_and(|m| m == "optimize")
}

fn name(i: usize) -> PackageName {
    PackageName::parse(format!("@bench/p{i:04}")).unwrap()
}

/// A layered catalog: package i depends on up to three later packages
/// (one optional, turned on by a feature that also needs a native
/// package), every package has versions 1.0.0, 1.1.0 and 1.2.0, and the
/// newest version of every eighth package needs a `^2` that does not
/// exist, so the search backtracks there. Ceilings cover everything below,
/// so every complete assignment validates.
fn request(n: usize) -> ResolveRequest {
    let mut ceilings: Vec<BTreeSet<String>> = vec![BTreeSet::new(); n];
    let mut manifests: Vec<Vec<Manifest>> = vec![vec![]; n];
    for i in (0..n).rev() {
        let mut ceiling: BTreeSet<String> = [CAPS[i % CAPS.len()].to_owned()].into();
        for j in [i + 1, i + 3, i + 7] {
            if j < n {
                ceiling.extend(ceilings[j].iter().cloned());
            }
        }
        ceilings[i] = ceiling.clone();
        for minor in 0..3u64 {
            let mut dependencies = BTreeMap::new();
            for (k, j) in [i + 1, i + 3, i + 7].into_iter().enumerate() {
                if j >= n {
                    continue;
                }
                let broken = minor == 2 && i % 8 == 7 && k == 0;
                dependencies.insert(
                    name(j),
                    Dependency {
                        version: VersionReq::parse(if broken { "^2" } else { "^1" }).unwrap(),
                        effects: ceilings[j].clone(),
                        optional: k == 2,
                        features: if k == 0 {
                            ["fast".to_owned()].into()
                        } else {
                            BTreeSet::new()
                        },
                        path: None,
                    },
                );
            }
            let optional = dependencies
                .iter()
                .filter(|(_, d)| d.optional)
                .map(|(n, _)| n.clone())
                .collect();
            let features = BTreeMap::from([
                (
                    "fast".to_owned(),
                    Feature {
                        enables_effects: BTreeSet::new(),
                        deps: optional,
                        features: ["uring".to_owned()].into(),
                        sys_deps: BTreeSet::new(),
                    },
                ),
                (
                    "uring".to_owned(),
                    Feature {
                        enables_effects: BTreeSet::new(),
                        deps: BTreeSet::new(),
                        features: BTreeSet::new(),
                        sys_deps: ["liburing ^2.6.0".to_owned()].into(),
                    },
                ),
            ]);
            manifests[i].push(Manifest {
                package: Package {
                    name: name(i),
                    version: Version::new(1, minor, i as u64),
                    edition: "2026".into(),
                    license: None,
                    capability_ceiling: ceiling.clone(),
                    forbid_unsafe_ffi: !ceiling.contains("Ffi[LibA]"),
                },
                features,
                dependencies,
                build_sandbox: None,
                resolver: None,
                verification: None,
            });
        }
    }
    let root = manifests[0].pop().unwrap();
    ResolveRequest {
        root,
        features: ["fast".to_owned()].into(),
        catalog: Catalog {
            schema_version: SCHEMA_VERSION,
            packages: manifests.into_iter().skip(1).flatten().collect(),
            system_packages: vec![
                SystemSelection {
                    name: "liburing".into(),
                    version: Version::new(2, 6, 1),
                },
                SystemSelection {
                    name: "liburing".into(),
                    version: Version::new(2, 5, 0),
                },
            ],
        },
        max_decisions: 1_000_000,
    }
}

fn metadata(request: &ResolveRequest) -> BTreeMap<(PackageName, Version), CandidateMetadata> {
    request
        .catalog
        .packages
        .iter()
        .map(|m| {
            let v = &m.package.version;
            (
                (m.package.name.clone(), v.clone()),
                CandidateMetadata {
                    cve_count: (v.minor + v.patch) % 3,
                    staleness_secs: 100 * (2 - v.minor),
                    trust_cost: v.patch % 5,
                    repro_attestations: 1,
                    vetted_namespace: true,
                },
            )
        })
        .collect()
}

fn pass(request: &ResolveRequest, optimized: Option<&OptimizeRequest>) -> usize {
    match optimized {
        Some(o) => {
            let r = resolve_optimized(o).unwrap();
            r.solution.packages.len() + r.evaluated_solutions
        }
        None => {
            let s = resolve(request).unwrap();
            validate_locked(request, &s).unwrap();
            s.packages.len()
        }
    }
}

fn window(passes: usize) -> u128 {
    let request = request(size());
    let optimized = optimize().then(|| OptimizeRequest {
        metadata: metadata(&request),
        resolve: request.clone(),
        objectives: vec![],
        max_solutions: 16,
    });
    std::hint::black_box(pass(&request, optimized.as_ref()));
    let start = Instant::now();
    for _ in 0..passes {
        std::hint::black_box(pass(&request, optimized.as_ref()));
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
                "median B/A {:.3} ({} packages per catalog, {})",
                ratios[EPOCHS / 2],
                size(),
                if optimize() { "optimize" } else { "resolve" }
            );
            let analysis = vesper_bench::analyze_pairs(&pairs, 10_000).unwrap();
            println!(
                "2x gate (B at most +10000 bp over A): {:?}; median regression bp {:?}",
                analysis.decision,
                analysis.median_upper.regression_bp_bounds()
            );
        }
        _ => {
            eprintln!("usage: solver_abba window N | driver RUST_BIN PORT_BIN");
            std::process::exit(2);
        }
    }
}
