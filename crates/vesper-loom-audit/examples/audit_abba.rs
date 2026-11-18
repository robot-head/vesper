//! ABBA benchmark of `audit`, comparing the Rust seed copy with the Vesper
//! port (port wave P3a lane 4, #1039).
//!
//! One binary holds one build, so the arms are two binaries:
//!
//! ```sh
//! cargo build --release -p vesper-loom-audit-seed --example audit_abba_rust
//! cp target/release/examples/audit_abba_rust /tmp/rust_arm
//! cargo build --release -p vesper-loom-audit --example audit_abba
//! cp target/release/examples/audit_abba /tmp/port_arm
//! /tmp/rust_arm driver /tmp/rust_arm /tmp/port_arm
//! ```
//!
//! `window N` prints the nanoseconds of N passes over the workload after one
//! warm-up pass, and `once` prints one report summary. `driver A B` runs
//! epochs of windows in the order A B B A, pairs each epoch's A and B totals
//! and reports the median ratio of B over A with
//! `vesper_bench::analyze_pairs`. `LOOM_AUDIT_SIZE` sets the packages of the
//! solution (default 64); the catalog holds four advisories per package
//! (one in eight affecting the selected version) and two native claims per
//! native package.
use semver::{Version, VersionReq};
use std::collections::BTreeSet;
use std::process::Command;
use std::time::Instant;
use vesper_loom_audit::{
    Advisory, AffectedRange, Catalog, Limits, NativeLicense, PackageIdentity, Policy, Severity,
    audit,
};
use vesper_loom_calculus::{
    Dependency, Feature, PackageName, RootRequirement, Selection, Solution, SystemSelection,
    parse_manifest,
};

const PASSES: usize = 10;
const EPOCHS: usize = 9;
const LICENSES: [&str; 6] = [
    "MIT",
    "MIT OR Apache-2.0",
    "(MIT OR BSD-3-Clause) AND Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "GPL-3.0-only OR MIT AND Apache-2.0",
    "ISC",
];

fn size() -> usize {
    std::env::var("LOOM_AUDIT_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}

fn name(i: usize) -> PackageName {
    PackageName::parse(format!("@bench/p{i:04}")).unwrap()
}

fn native(i: usize) -> String {
    format!("libn{:03}", i % 16)
}

/// A layered graph: package i depends on i + 1, i + 3 and i + 7, every
/// fourth package needs a native package, and the root is package 0.
fn inputs(n: usize) -> (Solution, Catalog, Policy, Limits) {
    let mut packages = Vec::new();
    let mut natives = BTreeSet::new();
    for i in 0..n {
        let mut manifest = parse_manifest(&format!(
            "[package]\nname = \"{}\"\nversion = \"1.{}.0\"\nedition = \"2026\"\nlicense = \"{}\"\n",
            name(i).as_str(),
            i % 5,
            LICENSES[i % LICENSES.len()]
        ))
        .unwrap();
        for j in [i + 1, i + 3, i + 7] {
            if j < n {
                manifest.dependencies.insert(
                    name(j),
                    Dependency {
                        version: VersionReq::parse("^1").unwrap(),
                        effects: BTreeSet::new(),
                        features: BTreeSet::new(),
                        optional: false,
                        path: None,
                    },
                );
            }
        }
        let mut features = BTreeSet::new();
        if i % 4 == 3 {
            natives.insert(native(i));
            manifest.features.insert(
                "native".into(),
                Feature {
                    sys_deps: [format!("{} ^1", native(i))].into(),
                    ..Feature::default()
                },
            );
            features.insert("native".to_owned());
        }
        packages.push(Selection { manifest, features });
    }
    let solution = Solution {
        schema_version: 1,
        roots: vec![RootRequirement {
            name: name(0),
            version: VersionReq::parse("^1").unwrap(),
            features: BTreeSet::new(),
        }],
        packages,
        system_packages: natives
            .iter()
            .map(|n| SystemSelection {
                name: n.clone(),
                version: Version::new(1, 2, 0),
            })
            .collect(),
    };
    let mut advisories = Vec::new();
    for i in 0..n {
        for k in 0..4 {
            let hit = (i * 4 + k) % 8 == 0;
            let package = if k == 3 {
                PackageIdentity::Native { name: native(i) }
            } else {
                PackageIdentity::Vesper { name: name(i) }
            };
            advisories.push(Advisory {
                id: format!("BENCH-{i:04}-{k}"),
                package,
                affected: vec![
                    AffectedRange {
                        introduced: Version::new(0, 1, 0),
                        fixed: Some(Version::new(0, 9, 0)),
                        include_prerelease: false,
                    },
                    AffectedRange {
                        introduced: if hit {
                            Version::new(1, 0, 0)
                        } else {
                            Version::new(3, 0, 0)
                        },
                        fixed: Some(Version::new(4, 0, 0)),
                        include_prerelease: k == 1,
                    },
                ],
                severity: [
                    Severity::Low,
                    Severity::Moderate,
                    Severity::High,
                    Severity::Critical,
                ][k],
                summary: format!("benchmark declaration {i}/{k}"),
            });
        }
    }
    let native_licenses = natives
        .iter()
        .flat_map(|n| {
            [
                NativeLicense {
                    name: n.clone(),
                    version: Version::new(1, 2, 0),
                    expression: "MIT OR Zlib".into(),
                },
                NativeLicense {
                    name: n.clone(),
                    version: Version::new(1, 1, 0),
                    expression: "GPL-2.0-only".into(),
                },
            ]
        })
        .collect();
    let catalog = Catalog {
        schema_version: 1,
        catalog_id: "bench".into(),
        advisories,
        native_licenses,
    };
    let policy = Policy {
        schema_version: 1,
        minimum_severity: Severity::Moderate,
        allowed_licenses: vec!["MIT".into(), "Apache-2.0".into(), "ISC".into()],
    };
    let limits = Limits {
        max_path_nodes: 1 << 24,
        max_entries: 1 << 20,
        max_work: usize::MAX,
        max_input_bytes: 1 << 30,
        ..Limits::default()
    };
    (solution, catalog, policy, limits)
}

fn window(passes: usize) -> u128 {
    let (s, c, p, l) = inputs(size());
    std::hint::black_box(audit(&s, &c, &p, &l).unwrap());
    let start = Instant::now();
    for _ in 0..passes {
        std::hint::black_box(audit(&s, &c, &p, &l).unwrap());
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
            let (s, c, p, l) = inputs(size());
            let report = audit(&s, &c, &p, &l).unwrap();
            println!(
                "{} {:?} findings={} vesper={} native={} paths={}",
                vesper_loom_audit::IMPLEMENTATION,
                report.outcome,
                report.findings.len(),
                report.reachable_vesper_packages,
                report.reachable_native_packages,
                report.findings.iter().map(|f| f.paths.len()).sum::<usize>()
            );
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
            println!("median B/A {:.3} ({} packages)", ratios[EPOCHS / 2], size());
            let analysis = vesper_bench::analyze_pairs(&pairs, 10_000).unwrap();
            println!(
                "2x gate (B at most +10000 bp over A): {:?}; median regression bp {:?}",
                analysis.decision,
                analysis.median_upper.regression_bp_bounds()
            );
        }
        _ => {
            eprintln!("usage: audit_abba window N | once | driver RUST_BIN PORT_BIN");
            std::process::exit(2);
        }
    }
}
