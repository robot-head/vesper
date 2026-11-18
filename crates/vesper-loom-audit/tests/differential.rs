//! Differential corpus for the two builds (port wave P3a lane 4, #1039).
//!
//! The same deterministic corpus of solutions, catalogs, policies and limits
//! runs against the Rust seed copy (`vesper-loom-audit-seed`) and the Vesper
//! port (`vesper-loom-audit`): `audit`, `parse_catalog`, `parse_policy` and a
//! matrix of `AffectedRange::contains`. Both must print the same results, so
//! the test compares an FNV-1a digest of every result's `Debug` text with the
//! digest the Rust build produced. Set `LOOM_AUDIT_DIGEST=1` to print the
//! digests instead of checking them.
use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use vesper_loom_audit::*;
use vesper_loom_calculus::{
    Dependency, Feature, PackageName, RootRequirement, Selection, Solution, SystemSelection,
    parse_manifest,
};

/// The digests of the corpora under the Rust build.
const EXPECTED_AUDIT: u64 = 0x95e3_c995_9886_2434;
const EXPECTED_RANGES: u64 = 0xafca_9eef_9826_80ae;
const CASES: usize = 400;

/// The leak check counts the port's live allocations process-wide, so the
/// tests of this file run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

struct Fnv(u64);
impl Fnv {
    fn add(&mut self, text: &str) {
        for b in text.bytes().chain([0xff]) {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
}

const LICENSES: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "MIT OR Apache-2.0",
    "GPL-3.0-only",
    "MIT AND GPL-3.0-only",
    "(MIT OR BSD-3-Clause) AND Apache-2.0",
    "GPL-2.0-only WITH Classpath-exception-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "MIT OR Unknown-SPDX",
    "mit",
    "MIT AND",
    "(MIT",
    "((((MIT))))",
    "MIT WITH NewException",
    "LicenseRef-Custom",
    "proprietary terms",
    "BSD-2-Clause OR ISC OR Zlib",
    "MIT)",
];
const GRANTS: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-3-Clause",
    "ISC",
    "GPL-2.0-only WITH Classpath-exception-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "(Zlib)",
];
const BAD_GRANTS: &[&str] = &[
    "MIT OR Apache-2.0",
    "Unknown-ID",
    "MIT WITH Nope",
    "MIT AND",
];
const VERSIONS: &[&str] = &[
    "0.9.0",
    "1.0.0-alpha.1",
    "1.0.0-alpha.10",
    "1.0.0",
    "1.0.0+build.7",
    "1.2.3",
    "1.9.9-rc.1",
    "2.0.0",
    "2.0.0+x",
    "3.1.4",
];
const NATIVES: &[&str] = &["libz", "libssl", "liburing", "libdemo"];

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}
fn name(i: usize) -> PackageName {
    PackageName::parse(format!("@corpus/p{i}")).unwrap()
}
fn severity(r: &mut Rng) -> Severity {
    [
        Severity::Low,
        Severity::Moderate,
        Severity::High,
        Severity::Critical,
    ][r.below(4)]
}

/// A solution of n packages: package i may depend on later packages (some
/// optional, some turned on by the root), and may need native packages
/// through a selected feature.
fn solution(r: &mut Rng) -> Solution {
    let n = 1 + r.below(9);
    let versions: Vec<Version> = (0..n)
        .map(|_| version(r.pick(&["1.0.0", "1.2.3", "1.0.0-alpha.10+sel", "1.9.9-rc.1"])))
        .collect();
    let mut natives = BTreeMap::new();
    let mut packages = Vec::new();
    for i in 0..n {
        let mut manifest = parse_manifest(&format!(
            "[package]\nname = \"@corpus/p{i}\"\nversion = \"{}\"\nedition = \"2026\"\n",
            versions[i]
        ))
        .unwrap();
        manifest.package.license = (!r.chance(10)).then(|| r.pick(LICENSES).to_owned());
        let mut features = BTreeSet::new();
        for (j, v) in versions.iter().enumerate().skip(i + 1) {
            if r.chance(35) {
                let optional = r.chance(25);
                manifest.dependencies.insert(
                    name(j),
                    Dependency {
                        version: VersionReq::parse(&format!("={v}")).unwrap(),
                        effects: BTreeSet::new(),
                        features: BTreeSet::new(),
                        optional,
                        path: None,
                    },
                );
                if optional && r.chance(50) {
                    manifest.features.insert(
                        format!("use{j}"),
                        Feature {
                            deps: [name(j)].into(),
                            ..Feature::default()
                        },
                    );
                    features.insert(format!("use{j}"));
                }
            }
        }
        if r.chance(40) {
            let native = r.pick(NATIVES);
            natives.insert(native, version(r.pick(&["1.0.0", "2.0.0", "1.2.3"])));
            manifest.features.insert(
                "native".into(),
                Feature {
                    sys_deps: [format!("{native} *")].into(),
                    ..Feature::default()
                },
            );
            features.insert("native".into());
        }
        // A missing dependency now and then: an invalid solution.
        if r.chance(3) {
            manifest.dependencies.insert(
                name(n + 5),
                Dependency {
                    version: VersionReq::parse("^1").unwrap(),
                    effects: BTreeSet::new(),
                    features: BTreeSet::new(),
                    optional: false,
                    path: None,
                },
            );
        }
        packages.push(Selection { manifest, features });
    }
    let mut roots = vec![RootRequirement {
        name: name(0),
        version: VersionReq::parse(&format!("={}", versions[0])).unwrap(),
        features: BTreeSet::new(),
    }];
    if n > 3 && r.chance(30) {
        roots.push(RootRequirement {
            name: name(n - 1),
            version: VersionReq::parse(&format!("={}", versions[n - 1])).unwrap(),
            features: BTreeSet::new(),
        });
    }
    if r.chance(3) {
        roots.clear();
    }
    let mut s = Solution {
        schema_version: 1,
        roots,
        packages,
        system_packages: natives
            .into_iter()
            .map(|(name, version)| SystemSelection {
                name: name.into(),
                version,
            })
            .collect(),
    };
    if r.chance(30) {
        s.packages.reverse();
    }
    s
}

fn range(r: &mut Rng) -> AffectedRange {
    let a = r.below(VERSIONS.len());
    let b = a + 1 + r.below(VERSIONS.len());
    AffectedRange {
        introduced: version(VERSIONS[a]),
        fixed: VERSIONS.get(b).map(|v| version(v)),
        include_prerelease: r.chance(50),
    }
}

fn catalog(r: &mut Rng, s: &Solution) -> Catalog {
    let mut advisories = Vec::new();
    for i in 0..r.below(8) {
        let package = if r.chance(70) && !s.packages.is_empty() {
            PackageIdentity::Vesper {
                name: s.packages[r.below(s.packages.len())]
                    .manifest
                    .package
                    .name
                    .clone(),
            }
        } else {
            PackageIdentity::Native {
                name: r.pick(NATIVES).into(),
            }
        };
        let mut affected = vec![range(r)];
        if r.chance(20) {
            affected.push(range(r));
        }
        advisories.push(Advisory {
            id: if r.chance(3) {
                "bad id".into()
            } else {
                format!("LOCAL-{}", (i * 7 + 3) % 10)
            },
            package,
            affected,
            severity: severity(r),
            summary: "corpus declaration".into(),
        });
    }
    let mut native_licenses = Vec::new();
    for _ in 0..r.below(4) {
        native_licenses.push(NativeLicense {
            name: r.pick(NATIVES).into(),
            version: version(r.pick(&["1.0.0", "2.0.0", "1.2.3"])),
            expression: r.pick(LICENSES).into(),
        });
    }
    Catalog {
        schema_version: if r.chance(2) { 2 } else { 1 },
        catalog_id: "corpus".into(),
        advisories,
        native_licenses,
    }
}

fn policy(r: &mut Rng) -> Policy {
    let mut allowed: Vec<String> = GRANTS
        .iter()
        .filter(|_| r.chance(60))
        .map(|g| (*g).to_owned())
        .collect();
    if r.chance(5) {
        allowed.push(r.pick(BAD_GRANTS).into());
    }
    if r.chance(3) && !allowed.is_empty() {
        allowed.push(allowed[0].clone());
    }
    Policy {
        schema_version: 1,
        minimum_severity: severity(r),
        allowed_licenses: allowed,
    }
}

fn limits(r: &mut Rng) -> Limits {
    let mut l = Limits::default();
    match r.below(30) {
        0 => l.max_work = r.below(400),
        1 => l.max_path_nodes = r.below(12),
        2 => l.max_path_depth = r.below(4),
        3 => l.max_license_depth = r.below(3),
        4 => l.max_license_tokens = r.below(4),
        5 => l.max_entries = r.below(4),
        6 => l.max_edges = r.below(4),
        7 => l.max_input_bytes = 300 + r.below(800),
        8 => l.max_string_bytes = 4 + r.below(12),
        9 => l.max_license_depth = 65,
        _ => {}
    }
    l
}

fn digest_audit() -> u64 {
    let mut d = Fnv(0xcbf2_9ce4_8422_2325);
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut reports, mut findings) = (0, 0);
    for _ in 0..CASES {
        let s = solution(&mut r);
        let c = catalog(&mut r, &s);
        let p = policy(&mut r);
        let l = limits(&mut r);
        let result = audit(&s, &c, &p, &l);
        if let Ok(report) = &result {
            reports += 1;
            findings += report.findings.len();
        }
        d.add(&format!("{result:?}"));
        let cj = serde_json::to_vec(&c).unwrap();
        d.add(&format!(
            "{:?}",
            parse_catalog(&cj, &l).map(|c| c.advisories.len())
        ));
        let pj = serde_json::to_vec(&p).unwrap();
        d.add(&format!(
            "{:?}",
            parse_policy(&pj, &l).map(|p| p.allowed_licenses)
        ));
    }
    // The corpus must exercise reports, not only errors.
    assert!(
        reports > CASES / 4 && findings > CASES / 2,
        "{reports} reports, {findings} findings"
    );
    d.0
}

fn digest_ranges() -> u64 {
    let mut d = Fnv(0xcbf2_9ce4_8422_2325);
    let probes = [
        "0.0.1",
        "0.9.0",
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-alpha.2",
        "1.0.0-alpha.10",
        "1.0.0-beta",
        "1.0.0",
        "1.0.0+b",
        "1.2.3",
        "1.9.9-rc.1",
        "1.9.9",
        "2.0.0-alpha.2",
        "2.0.0",
        "2.0.0+x",
        "3.1.4",
        "500.0.0+build",
    ];
    for (i, a) in VERSIONS.iter().enumerate() {
        for b in VERSIONS[i..].iter().map(Some).chain([None]) {
            for pre in [false, true] {
                let range = AffectedRange {
                    introduced: version(a),
                    fixed: b.map(|b| version(b)),
                    include_prerelease: pre,
                };
                for v in probes {
                    d.add(&format!(
                        "{a} {b:?} {pre} {v} {}",
                        range.contains(&version(v))
                    ));
                }
            }
        }
    }
    d.0
}

fn check(label: &str, got: u64, expected: u64) {
    if std::env::var_os("LOOM_AUDIT_DIGEST").is_some() {
        println!("{label} ({IMPLEMENTATION}): {got:#018x}");
    } else {
        assert_eq!(
            got, expected,
            "{label} digest of the {IMPLEMENTATION} build differs from the Rust build"
        );
    }
}

#[test]
fn audit_corpus_matches_the_rust_build() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = port_live_allocations();
    check("audit", digest_audit(), EXPECTED_AUDIT);
    assert_eq!(port_live_allocations(), before, "the port leaked");
}

#[test]
fn range_matrix_matches_the_rust_build() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = port_live_allocations();
    check("ranges", digest_ranges(), EXPECTED_RANGES);
    assert_eq!(port_live_allocations(), before, "the port leaked");
}
