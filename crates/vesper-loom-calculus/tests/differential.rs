//! Differential corpus for the two builds (port wave P1 lane 16, #913).
//!
//! The same deterministic corpus of manifests, solutions and upgrade pairs
//! runs against the Rust seed copy (`vesper-loom-calculus-seed`) and the
//! Vesper port (`vesper-loom-calculus`). Both must print the same results,
//! so the test compares an FNV-1a digest of every result's `Debug` text
//! with the digest the Rust build produced. Set `LOOM_CALCULUS_DIGEST=1` to
//! print the digest instead of checking it.
use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use vesper_loom_calculus::*;

/// The digests of the corpora under the Rust build.
const EXPECTED_MANIFESTS: u64 = 0xb956_bbba_c98b_69f3;
const EXPECTED_SEMVER: u64 = 0x3f6b_ed77_7a80_09e5;
const EXPECTED_SOLUTIONS: u64 = 0xe464_73b3_1f1f_5349;
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
    fn some(&mut self, xs: &[&str], max: usize) -> BTreeSet<String> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.pick(xs).to_owned()).collect()
    }
}

const NAMES: &[&str] = &[
    "@org/app",
    "@org/core",
    "@org/net",
    "@org/io",
    "@org/ffi",
    "std/bytes",
    "@b/x",
    "@b/y",
];
const CAPS: &[&str] = &[
    "Net",
    "Storage",
    "Clock",
    "Fail",
    "Ffi[LibA]",
    "Ffi[LibB]",
    "UnsafeFfi",
    "UnsafeFfi[Sys]",
    "Ffi",
    "Map[K,V]",
    "Async[io:uring]",
    "bad",
    "X[]",
    "Y]",
    "Z[[a]",
    "Net Raw",
    "@org/Cap",
];
const FEATURES: &[&str] = &[
    "default",
    "tls",
    "uring",
    "fast-path",
    "x_y",
    "bad name",
    "",
    "zstd",
];
const VERSIONS: &[&str] = &[
    "1.0.0",
    "1.2.3",
    "1.2.3-alpha.1",
    "1.2.3-alpha.10",
    "1.2.3-alpha.beta",
    "1.2.3-1",
    "1.2.3-rc.01a",
    "0.9.2",
    "0.0.3",
    "2.0.0",
    "2.0.0+build.01",
    "2.0.0+build.1",
    "2.0.0+00",
    "1.5.0",
];
const REQS: &[&str] = &[
    "^1",
    "^1.2",
    "~1.2",
    "~1.2.3",
    ">=1.2.3-alpha.1",
    "<2",
    "<=1.2.3",
    "=1.2.3",
    "*",
    "0.9",
    "^0.0.3",
    "^0.9.1",
    ">1.0, <1.5",
    "1.*",
    ">1.2.3-alpha.2",
    "~1.2.3-alpha",
    "2",
];
const SYS: &[&str] = &[
    "liburing ^2.6.0",
    "zlib >=1.2, <2",
    "openssl ~3.0.0-beta.1",
    "bad",
    "lib$ 1.0",
    "zlib  1.2.13",
    "libc not-a-version",
    "openssl 3",
];
const SYS_NAMES: &[&str] = &["liburing", "zlib", "openssl", "lib$", "libc"];
const SYS_VERSIONS: &[&str] = &["2.6.1", "1.2.13", "3.0.0-beta.2", "3.1.0", "1.3.0-rc.1"];
const PATHS: &[&str] = &[
    "build.vsp",
    "proto/",
    "/abs",
    "a/../b",
    "ok/dir",
    "c:x",
    "..",
    "",
    "a\\b",
];

fn version(rng: &mut Rng) -> Version {
    Version::parse(rng.pick(VERSIONS)).unwrap()
}
fn req(rng: &mut Rng) -> VersionReq {
    VersionReq::parse(rng.pick(REQS)).unwrap()
}
fn name(s: &str) -> PackageName {
    PackageName::parse(s).unwrap()
}

fn manifest(rng: &mut Rng, own: &str) -> Manifest {
    let mut dependencies = BTreeMap::new();
    for _ in 0..rng.below(4) {
        let target = rng.pick(NAMES);
        if target == own && !rng.chance(20) {
            continue;
        }
        dependencies.insert(
            name(target),
            Dependency {
                version: req(rng),
                effects: rng.some(CAPS, 3),
                optional: rng.chance(30),
                features: rng.some(FEATURES, 2),
                path: None,
            },
        );
    }
    let mut features = BTreeMap::new();
    for _ in 0..rng.below(4) {
        let deps = (0..rng.below(3))
            .map(|_| name(rng.pick(NAMES)))
            .collect::<BTreeSet<_>>();
        features.insert(
            rng.pick(FEATURES).to_owned(),
            Feature {
                enables_effects: rng.some(CAPS, 2),
                deps,
                features: rng.some(FEATURES, 2),
                sys_deps: if rng.chance(30) {
                    rng.some(SYS, 2)
                } else {
                    BTreeSet::new()
                },
            },
        );
    }
    let build_sandbox = rng.chance(25).then(|| BuildSandbox {
        script: rng.chance(50).then(|| rng.pick(PATHS).to_owned()),
        wasi_read_paths: rng.some(PATHS, 2),
        fuel_limit: None,
        allow_network: rng.chance(20),
    });
    Manifest {
        package: Package {
            name: name(own),
            version: version(rng),
            edition: "2026".into(),
            license: None,
            capability_ceiling: rng.some(CAPS, 4),
            forbid_unsafe_ffi: rng.chance(70),
        },
        features,
        dependencies,
        build_sandbox,
        resolver: None,
        verification: None,
    }
}

fn solution(rng: &mut Rng) -> Solution {
    let mut packages = vec![];
    for n in NAMES {
        if rng.chance(75) {
            packages.push(Selection {
                manifest: manifest(rng, n),
                features: rng.some(FEATURES, 2),
            });
        }
        if rng.chance(5) {
            packages.push(Selection {
                manifest: manifest(rng, n),
                features: BTreeSet::new(),
            });
        }
    }
    let roots = (0..1 + rng.below(2))
        .map(|_| RootRequirement {
            name: name(rng.pick(NAMES)),
            version: req(rng),
            features: rng.some(FEATURES, 2),
        })
        .collect();
    let system_packages = (0..rng.below(3))
        .map(|_| SystemSelection {
            name: rng.pick(SYS_NAMES).to_owned(),
            version: Version::parse(rng.pick(SYS_VERSIONS)).unwrap(),
        })
        .collect();
    Solution {
        schema_version: if rng.chance(3) { 2 } else { SCHEMA_VERSION },
        roots,
        packages,
        system_packages,
    }
}

/// A clean solution: mostly valid graphs, so the upgrade guard compares.
/// Ceilings cover every dependency's ceiling and edges allow it.
fn tame(rng: &mut Rng) -> Solution {
    let caps = ["Net", "Storage", "Clock", "Ffi[LibA]"];
    let names = ["@t/app", "@t/a", "@t/b", "@t/c"];
    let mut ceilings: Vec<BTreeSet<String>> = vec![BTreeSet::new(); names.len()];
    let mut packages = vec![];
    for i in (0..names.len()).rev() {
        let mut ceiling = rng.some(&caps, 2);
        let mut dependencies = BTreeMap::new();
        for (j, t) in names.iter().enumerate().skip(i + 1) {
            if rng.chance(60) {
                let mut effects = ceilings[j].clone();
                if rng.chance(90) {
                    effects.extend(rng.some(&caps, 1));
                } else {
                    effects.clear();
                }
                ceiling.extend(effects.iter().cloned());
                dependencies.insert(
                    name(t),
                    Dependency {
                        version: VersionReq::parse("^1").unwrap(),
                        effects,
                        optional: false,
                        features: BTreeSet::new(),
                        path: None,
                    },
                );
            }
        }
        let forbid = !ceiling.contains("Ffi[LibA]") && rng.chance(80);
        ceilings[i] = ceiling.clone();
        packages.push(Selection {
            manifest: Manifest {
                package: Package {
                    name: name(names[i]),
                    version: Version::new(1, rng.below(3) as u64, 0),
                    edition: "2026".into(),
                    license: None,
                    capability_ceiling: ceiling,
                    forbid_unsafe_ffi: forbid,
                },
                features: BTreeMap::new(),
                dependencies,
                build_sandbox: None,
                resolver: None,
                verification: None,
            },
            features: BTreeSet::new(),
        });
    }
    Solution {
        schema_version: SCHEMA_VERSION,
        roots: vec![RootRequirement {
            name: name(if rng.chance(95) { "@t/app" } else { "@t/a" }),
            version: VersionReq::parse("^1").unwrap(),
            features: BTreeSet::new(),
        }],
        packages,
        system_packages: vec![],
    }
}

/// A candidate for `a`: unrelated, or `a` with versions bumped and some
/// authority removed.
fn candidate(rng: &mut Rng, a: &Solution) -> Solution {
    if rng.chance(50) {
        return tame(rng);
    }
    let mut b = a.clone();
    for s in &mut b.packages {
        s.manifest.package.version.patch += 1;
        if rng.chance(30) {
            let ceiling = &mut s.manifest.package.capability_ceiling;
            if let Some(c) = ceiling.iter().next().cloned() {
                ceiling.remove(&c);
            }
        }
        if rng.chance(20) {
            s.manifest.package.forbid_unsafe_ffi = !s.manifest.package.forbid_unsafe_ffi;
        }
    }
    b
}

struct Digest(u64, usize);
impl Digest {
    fn add(&mut self, text: &str) {
        for b in text.bytes() {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
        self.1 += 1;
    }
}

/// Compare a digest with the Rust build's, or print it.
fn check(what: &str, digest: &Digest, expected: u64) {
    if std::env::var_os("LOOM_CALCULUS_DIGEST").is_some() {
        println!(
            "{IMPLEMENTATION} {what} digest {:#018x} over {} results",
            digest.0, digest.1
        );
        return;
    }
    assert_eq!(
        digest.0, expected,
        "the {IMPLEMENTATION} build diverges from the Rust build on the {what} corpus"
    );
}

#[test]
fn manifest_corpus_matches_the_rust_build() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut digest = Digest(0xcbf2_9ce4_8422_2325, 0);
    let mut clean = 0;
    for _ in 0..CASES {
        let own = rng.pick(NAMES);
        let m = manifest(&mut rng, own);
        let d = validate_manifest(&m);
        clean += usize::from(d.is_empty());
        digest.add(&format!("{d:?}"));
    }
    for candidate in NAMES.iter().chain(&[
        "http", "@org", "std/Io", "@a/b/c", "@é/x", "s/x", "@a-/b", "std/", "@/x",
    ]) {
        digest.add(&format!("{:?}", PackageName::parse(*candidate)));
    }
    assert!(clean > 0 && clean < CASES, "manifest outcomes: {clean}");
    check("manifest", &digest, EXPECTED_MANIFESTS);
}

#[test]
fn semver_matrix_matches_the_rust_build() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut digest = Digest(0xcbf2_9ce4_8422_2325, 0);
    let versions = VERSIONS.iter().chain(SYS_VERSIONS);
    let mut matched = 0;
    for v in versions.clone() {
        for r in REQS {
            let mut manifest = manifest(&mut Rng(3), "@org/app");
            manifest.dependencies.clear();
            manifest.features.clear();
            manifest.build_sandbox = None;
            manifest.package.capability_ceiling.clear();
            manifest.package.version = Version::parse(v).unwrap();
            let s = Solution {
                schema_version: SCHEMA_VERSION,
                roots: vec![RootRequirement {
                    name: name("@org/app"),
                    version: VersionReq::parse(r).unwrap(),
                    features: BTreeSet::new(),
                }],
                packages: vec![Selection {
                    manifest,
                    features: BTreeSet::new(),
                }],
                system_packages: vec![],
            };
            let checked = validate_solution(&s);
            matched += usize::from(checked.diagnostics.is_empty());
            digest.add(&format!("{:?}", checked.diagnostics));
        }
        // Version order picks which duplicate selection wins.
        for w in versions.clone() {
            let mut a = manifest(&mut Rng(5), "@org/app");
            a.package.version = Version::parse(v).unwrap();
            let mut b = a.clone();
            b.package.version = Version::parse(w).unwrap();
            let s = Solution {
                schema_version: SCHEMA_VERSION,
                roots: vec![],
                packages: vec![
                    Selection {
                        manifest: a,
                        features: BTreeSet::new(),
                    },
                    Selection {
                        manifest: b,
                        features: BTreeSet::new(),
                    },
                ],
                system_packages: vec![
                    SystemSelection {
                        name: "zlib".into(),
                        version: Version::parse(v).unwrap(),
                    },
                    SystemSelection {
                        name: "zlib".into(),
                        version: Version::parse(w).unwrap(),
                    },
                ],
            };
            digest.add(&format!("{:?}", validate_solution(&s)));
        }
    }
    assert!(matched > 0, "no requirement matched");
    check("semver", &digest, EXPECTED_SEMVER);
}

#[test]
fn solution_corpus_matches_the_rust_build() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut digest = Digest(0xcbf2_9ce4_8422_2325, 0);
    let mut graphs = 0;
    let mut allowed = 0;
    for _ in 0..CASES {
        let s = solution(&mut rng);
        let v = validate_solution(&s);
        graphs += usize::from(v.graph.is_some());
        digest.add(&format!("{v:?}"));
        if let Some(g) = &v.graph {
            digest.add(&g.canonical_json().unwrap());
        }
        let t = solution(&mut rng);
        digest.add(&format!("{:?}", check_upgrade(&s, &t)));
        let a = tame(&mut rng);
        let b = candidate(&mut rng, &a);
        let va = validate_solution(&a);
        graphs += usize::from(va.graph.is_some());
        digest.add(&format!("{va:?}"));
        let report = check_upgrade(&a, &b);
        allowed += usize::from(report.allowed);
        digest.add(&format!("{report:?}"));
        digest.add(&format!("{:?}", check_upgrade(&a, &a)));
    }
    // The corpus must reach both outcomes of every check it digests.
    assert!(graphs > CASES / 2, "too few valid graphs: {graphs}");
    assert!(
        allowed > CASES / 10 && allowed < CASES,
        "upgrade outcomes: {allowed}"
    );
    check("solution", &digest, EXPECTED_SOLUTIONS);
}

#[test]
fn the_port_frees_what_it_allocates() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = port_live_allocations();
    let mut rng = Rng(7);
    for _ in 0..20 {
        let s = solution(&mut rng);
        let t = solution(&mut rng);
        let _ = validate_solution(&s);
        let _ = check_upgrade(&s, &t);
        for selection in &s.packages {
            let _ = validate_manifest(&selection.manifest);
        }
    }
    assert_eq!(port_live_allocations(), before);
}
