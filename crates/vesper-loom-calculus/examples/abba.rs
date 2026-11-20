//! ABBA benchmark of `validate_solution` and `check_upgrade`, comparing the
//! Rust seed copy with the Vesper port (port wave P1 lane 16, #913).
//!
//! One binary holds one build, so the arms are two binaries:
//!
//! ```sh
//! cargo build --release -p vesper-loom-calculus-seed --example abba_rust
//! cp target/release/examples/abba_rust /tmp/rust_arm
//! cargo build --release -p vesper-loom-calculus --example abba
//! cp target/release/examples/abba /tmp/port_arm
//! /tmp/rust_arm driver /tmp/rust_arm /tmp/port_arm
//! ```
//!
//! `window N` prints the nanoseconds of N passes over the workload after one
//! warm-up pass. `driver A B` runs epochs of windows in the order A B B A,
//! pairs each epoch's A and B totals and reports the median regression of B
//! over A with `vesper_bench::analyze_pairs`, plus the 2x stop rule of the
//! port plan. `LOOM_ABBA_SIZE` sets the packages per graph (default 64).
use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::time::Instant;
use vesper_loom_calculus::*;

const PASSES: usize = 20;
const EPOCHS: usize = 9;
const CAPS: [&str; 6] = ["Net", "Storage", "Clock", "Fail", "Ffi[LibA]", "Async[io]"];

fn size() -> usize {
    std::env::var("LOOM_ABBA_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}

fn name(i: usize) -> PackageName {
    PackageName::parse(format!("@bench/p{i:04}")).unwrap()
}

/// A layered graph: package i depends on up to three later packages, has a
/// feature that turns on an optional dependency and a native requirement,
/// and its ceiling covers everything below it, so the graph validates.
fn solution(n: usize, bump: u64) -> Solution {
    let mut ceilings: Vec<BTreeSet<String>> = vec![BTreeSet::new(); n];
    let mut packages = Vec::with_capacity(n);
    for i in (0..n).rev() {
        let mut ceiling: BTreeSet<String> = [CAPS[i % CAPS.len()].to_owned()].into();
        let mut dependencies = BTreeMap::new();
        for (k, j) in [i + 1, i + 3, i + 7].into_iter().enumerate() {
            if j >= n {
                continue;
            }
            ceiling.extend(ceilings[j].iter().cloned());
            dependencies.insert(
                name(j),
                Dependency {
                    version: VersionReq::parse("^1.2").unwrap(),
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
        ceilings[i] = ceiling.clone();
        packages.push(Selection {
            manifest: Manifest {
                package: Package {
                    name: name(i),
                    version: Version::new(1, 2 + bump, i as u64),
                    edition: "2026".into(),
                    license: None,
                    capability_ceiling: ceiling.clone(),
                    forbid_unsafe_ffi: !ceiling.contains("Ffi[LibA]"),
                    vsys: vec![],
                },
                features,
                dependencies,
                build_sandbox: None,
                resolver: None,
                verification: None,
            },
            features: if i == 0 {
                ["fast".to_owned()].into()
            } else {
                BTreeSet::new()
            },
        });
    }
    Solution {
        schema_version: SCHEMA_VERSION,
        roots: vec![RootRequirement {
            name: name(0),
            version: VersionReq::parse("^1").unwrap(),
            features: BTreeSet::new(),
        }],
        packages,
        system_packages: vec![SystemSelection {
            name: "liburing".into(),
            version: Version::new(2, 6, 1),
        }],
    }
}

fn pass(a: &Solution, b: &Solution) -> usize {
    let v = validate_solution(a);
    assert!(v.diagnostics.is_empty(), "{:?}", v.diagnostics);
    let r = check_upgrade(a, b);
    assert!(r.allowed, "{:?}", r.diagnostics);
    v.graph.map_or(0, |g| g.packages.len()) + r.changes.len()
}

fn window(passes: usize) -> u128 {
    let n = size();
    let a = solution(n, 0);
    let b = solution(n, 1);
    std::hint::black_box(pass(&a, &b));
    let start = Instant::now();
    for _ in 0..passes {
        std::hint::black_box(pass(&a, &b));
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
                "median B/A {:.3} ({} packages per graph)",
                ratios[EPOCHS / 2],
                size()
            );
            let analysis = vesper_bench::analyze_pairs(&pairs, 10_000).unwrap();
            println!(
                "2x gate (B at most +10000 bp over A): {:?}; median regression bp {:?}",
                analysis.decision,
                analysis.median_upper.regression_bp_bounds()
            );
        }
        _ => {
            eprintln!("usage: abba window N | driver RUST_BIN PORT_BIN");
            std::process::exit(2);
        }
    }
}
