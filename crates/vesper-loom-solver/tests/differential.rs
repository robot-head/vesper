//! Differential corpus for the two builds (port wave P1 lane 17, #914).
//!
//! The same deterministic corpus of resolve requests runs against the Rust
//! seed copy (`vesper-loom-solver-seed`) and the Vesper port
//! (`vesper-loom-solver`): `resolve`, `validate_locked` on the result and
//! on edited snapshots, `resolve_optimized` and `compute_metrics`. Both must
//! print the same results, so the test compares an FNV-1a digest of every
//! result's `Debug` text with the digest the Rust build produced. Set
//! `LOOM_SOLVER_DIGEST=1` to print the digests instead of checking them.
use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use vesper_loom_calculus::{
    Dependency, Feature, Manifest, Package, PackageName, Resolver, ResolverPolicy, SCHEMA_VERSION,
    Solution, SystemSelection,
};
use vesper_loom_solver::optimize::{
    CandidateMetadata, OptimizeObjective, OptimizeRequest, compute_metrics, resolve_optimized,
};
use vesper_loom_solver::{
    Catalog, IMPLEMENTATION, ResolveRequest, port_live_allocations, resolve, validate_locked,
};

/// The digests of the corpora under the Rust build.
const EXPECTED_RESOLVE: u64 = 0xac7b_b46f_4a05_46ef;
const EXPECTED_OPTIMIZE: u64 = 0x7c8a_ca93_3b77_1a7d;
const CASES: usize = 300;

/// The leak check counts the port's live allocations process-wide, so the
/// tests of this file run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// The state, and whether this request may contain invalid inputs.
struct Rng(u64, bool);
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
    /// A chance for an invalid input, only in wild requests.
    fn wild(&mut self, percent: u64) -> bool {
        self.1 && self.chance(percent)
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
    fn some(&mut self, xs: &[&str], max: usize) -> BTreeSet<String> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.pick(xs).to_owned()).collect()
    }
}

const NAMES: &[&str] = &["@t/a", "@t/b", "@t/c", "@t/d", "@t/e", "std/io"];
const ROOT: &str = "@t/root";
const CAPS: &[&str] = &["Net", "Storage", "Clock", "Fail", "Ffi[LibA]", "UnsafeFfi"];
const FEATURES: &[&str] = &["default", "tls", "fast", "zstd", "extra"];
const VERSIONS: &[&str] = &[
    "1.0.0",
    "1.1.0",
    "1.2.3",
    "1.2.3-rc.1",
    "2.0.0",
    "2.1.0+build.7",
    "0.9.0",
    "3.0.0",
];
const REQS: &[&str] = &[
    "^1",
    "^1.1",
    "~1.2",
    ">=1.0, <2",
    "*",
    "^2",
    "=1.2.3",
    "^0.9",
    ">=2",
    "<1.2",
    "^3",
];
const SYS: &[&str] = &["zlib ^1.2", "zlib >=1.3", "liburing ^2", "openssl ~3.0"];
const BAD_SYS: &[&str] = &["bad", "lib$ 1.0"];
const SYS_NAMES: &[&str] = &["zlib", "liburing", "openssl"];
const SYS_VERSIONS: &[&str] = &["1.2.13", "1.3.1", "2.6.1", "3.0.2", "3.1.0"];
const OPTIMIZE: &[&str] = &[
    "zero_cve",
    "min_capability_surface",
    "min_dup_majors",
    "min_staleness",
    "min_trust_cost",
    "bogus",
];
const OBJECTIVES: &[OptimizeObjective] = &[
    OptimizeObjective::ZeroCve,
    OptimizeObjective::MinCapabilitySurface,
    OptimizeObjective::MinDupMajors,
    OptimizeObjective::MinStaleness,
    OptimizeObjective::MinTrustCost,
];

fn name(s: &str) -> PackageName {
    PackageName::parse(s).unwrap()
}
fn req(rng: &mut Rng) -> VersionReq {
    VersionReq::parse(rng.pick(REQS)).unwrap()
}

fn policy(rng: &mut Rng) -> Option<Resolver> {
    if !rng.chance(12) {
        return None;
    }
    Some(Resolver {
        policy: ResolverPolicy {
            optimize: if rng.chance(30) {
                (0..1 + rng.below(2))
                    .map(|_| rng.pick(OPTIMIZE).to_owned())
                    .collect()
            } else {
                vec![]
            },
            min_repro_attestations: rng.chance(40).then(|| rng.below(3) as u32),
            block_unvetted_namespaces: rng.chance(40),
        },
    })
}

fn manifest(rng: &mut Rng, own: &str, version: &str) -> Manifest {
    let mut dependencies = BTreeMap::new();
    for _ in 0..rng.below(4) {
        let target = rng.pick(NAMES);
        if target == own && !rng.chance(10) {
            continue;
        }
        dependencies.insert(
            name(target),
            Dependency {
                version: req(rng),
                effects: if rng.chance(60) {
                    CAPS.iter().map(|s| (*s).to_owned()).collect()
                } else {
                    rng.some(CAPS, 2)
                },
                optional: rng.chance(25),
                features: if rng.chance(25) {
                    rng.some(FEATURES, 1)
                } else {
                    BTreeSet::new()
                },
                path: None,
            },
        );
    }
    let mut features = BTreeMap::<String, Feature>::new();
    for _ in 0..rng.below(3) {
        let mut deps = BTreeSet::new();
        for d in dependencies.keys() {
            if rng.chance(40) {
                deps.insert(d.clone());
            }
        }
        // Sub-features name earlier features (rarely an undeclared one).
        let declared = features.keys().map(String::as_str).collect::<Vec<_>>();
        let sub = if rng.wild(10) {
            rng.some(FEATURES, 1)
        } else if declared.is_empty() || rng.chance(50) {
            BTreeSet::new()
        } else {
            rng.some(&declared, 1)
        };
        let mut sys_deps = if rng.chance(25) {
            rng.some(SYS, 2)
        } else {
            BTreeSet::new()
        };
        if rng.wild(5) {
            sys_deps.insert(rng.pick(BAD_SYS).to_owned());
        }
        features.insert(
            rng.pick(FEATURES).to_owned(),
            Feature {
                enables_effects: rng.some(CAPS, 1),
                deps,
                features: sub,
                sys_deps,
            },
        );
    }
    Manifest {
        package: Package {
            name: name(own),
            version: Version::parse(version).unwrap(),
            edition: "2026".into(),
            license: rng.chance(20).then(|| "MIT".to_owned()),
            capability_ceiling: if rng.chance(70) {
                CAPS.iter().map(|s| (*s).to_owned()).collect()
            } else {
                rng.some(CAPS, 3)
            },
            forbid_unsafe_ffi: rng.chance(10),
            vsys: vec![],
        },
        features,
        dependencies,
        build_sandbox: None,
        resolver: policy(rng),
        verification: None,
    }
}

fn request(rng: &mut Rng) -> ResolveRequest {
    rng.1 = rng.chance(20);
    let mut root = manifest(rng, ROOT, "1.0.0");
    if rng.chance(60) {
        root.resolver = None;
    }
    let mut packages = vec![];
    for own in NAMES {
        let mut versions = BTreeSet::new();
        for _ in 0..rng.below(4) {
            let version = rng.pick(VERSIONS);
            if versions.insert(version) {
                packages.push(manifest(rng, own, version));
            }
        }
    }
    if rng.chance(4) {
        let twin = if rng.chance(50) {
            root.clone()
        } else {
            manifest(rng, ROOT, "1.0.0")
        };
        packages.push(twin);
    }
    if rng.wild(20) && !packages.is_empty() {
        let duplicate = packages[0].clone();
        packages.push(duplicate);
    }
    let mut system_packages = vec![];
    for _ in 0..rng.below(6) {
        let selection = SystemSelection {
            name: if rng.wild(10) {
                "lib$"
            } else {
                rng.pick(SYS_NAMES)
            }
            .to_owned(),
            version: Version::parse(rng.pick(SYS_VERSIONS)).unwrap(),
        };
        if rng.wild(20) || !system_packages.contains(&selection) {
            system_packages.push(selection);
        }
    }
    let declared = root.features.keys().map(String::as_str).collect::<Vec<_>>();
    let features = if rng.wild(30) || declared.is_empty() {
        rng.some(FEATURES, 1)
    } else {
        rng.some(&declared, 2)
    };
    ResolveRequest {
        features,
        root,
        catalog: Catalog {
            schema_version: if rng.wild(10) { 2 } else { SCHEMA_VERSION },
            packages,
            system_packages,
        },
        max_decisions: if rng.chance(15) { rng.below(4) } else { 500 },
    }
}

fn metadata(
    rng: &mut Rng,
    request: &ResolveRequest,
) -> BTreeMap<(PackageName, Version), CandidateMetadata> {
    let mut out = BTreeMap::new();
    for m in &request.catalog.packages {
        if !rng.chance(85) {
            continue;
        }
        let meta = CandidateMetadata {
            cve_count: rng.below(3) as u64,
            staleness_secs: rng.below(1000) as u64,
            trust_cost: if rng.chance(5) {
                u64::MAX - 3
            } else {
                rng.below(50) as u64
            },
            repro_attestations: rng.below(4) as u32,
            vetted_namespace: rng.chance(70),
        };
        out.insert((m.package.name.clone(), m.package.version.clone()), meta);
    }
    out
}

/// Edits of a resolved snapshot that `validate_locked` must judge.
fn edits(rng: &mut Rng, request: &ResolveRequest, solution: &Solution) -> Vec<Solution> {
    let mut out = vec![];
    if solution.packages.len() > 1 {
        let mut s = solution.clone();
        s.packages.remove(1 + rng.below(s.packages.len() - 1));
        out.push(s);
    }
    let mut s = solution.clone();
    if let Some(p) = s.packages.last_mut() {
        p.features.insert(rng.pick(FEATURES).to_owned());
    }
    out.push(s);
    let mut s = solution.clone();
    if let Some(other) = request
        .catalog
        .packages
        .get(rng.below(request.catalog.packages.len().max(1)))
    {
        s.packages.push(vesper_loom_calculus::Selection {
            manifest: other.clone(),
            features: BTreeSet::new(),
        });
    }
    out.push(s);
    let mut s = solution.clone();
    if let Some(p) = s.packages.last_mut() {
        p.manifest.package.license = Some("changed".into());
    }
    out.push(s);
    let mut s = solution.clone();
    s.system_packages.push(SystemSelection {
        name: rng.pick(SYS_NAMES).to_owned(),
        version: Version::parse(rng.pick(SYS_VERSIONS)).unwrap(),
    });
    out.push(s);
    let mut s = solution.clone();
    s.roots[0].features.insert("tls".into());
    out.push(s);
    out
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
    if std::env::var_os("LOOM_SOLVER_DIGEST").is_some() {
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
fn resolve_corpus_matches_the_rust_build() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut rng = Rng(0x2545_f491_4f6c_dd1d, false);
    let mut digest = Digest(0xcbf2_9ce4_8422_2325, 0);
    let mut solved = 0;
    let mut kinds = BTreeSet::new();
    let mut locked_ok = 0;
    for _ in 0..CASES {
        let request = request(&mut rng);
        let result = resolve(&request);
        digest.add(&format!("{result:?}"));
        match &result {
            Ok(solution) => {
                solved += 1;
                let checked = validate_locked(&request, solution);
                locked_ok += usize::from(checked.is_ok());
                digest.add(&format!("{checked:?}"));
                for edited in edits(&mut rng, &request, solution) {
                    let checked = validate_locked(&request, &edited);
                    if let Err(e) = &checked {
                        kinds.insert(format!("locked {:?}", e.kind));
                    }
                    digest.add(&format!("{checked:?}"));
                }
            }
            Err(e) => {
                kinds.insert(format!("{:?}", e.kind));
            }
        }
    }
    assert!(solved > CASES / 5, "too few solutions: {solved}");
    assert_eq!(
        locked_ok, solved,
        "a resolved snapshot failed validate_locked"
    );
    assert!(kinds.len() >= 5, "outcomes: {kinds:?}");
    check("resolve", &digest, EXPECTED_RESOLVE);
}

#[test]
fn optimize_corpus_matches_the_rust_build() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15, false);
    let mut digest = Digest(0xcbf2_9ce4_8422_2325, 0);
    let mut optimized = 0;
    for _ in 0..CASES {
        let resolve = request(&mut rng);
        let objectives = if rng.chance(50) {
            vec![]
        } else {
            (0..1 + rng.below(3))
                .map(|_| OBJECTIVES[rng.below(OBJECTIVES.len())])
                .collect()
        };
        let metadata = metadata(&mut rng, &resolve);
        let request = OptimizeRequest {
            resolve,
            objectives,
            metadata,
            max_solutions: if rng.chance(5) { 0 } else { 1 + rng.below(40) },
        };
        let result = resolve_optimized(&request);
        optimized += usize::from(result.is_ok());
        digest.add(&format!("{result:?}"));
        if let Ok(best) = &result {
            let root = &request.resolve.root.package.name;
            digest.add(&format!(
                "{:?}",
                compute_metrics(&best.solution, root, &request.metadata)
            ));
            // A snapshot that is not a valid graph takes the fallback path.
            let mut broken = best.solution.clone();
            broken.roots.clear();
            if let Some(p) = broken.packages.last() {
                let mut twin = p.clone();
                twin.manifest.package.version.major += 1;
                broken.packages.push(twin);
            }
            digest.add(&format!(
                "{:?}",
                compute_metrics(&broken, root, &request.metadata)
            ));
        }
    }
    for raw in OPTIMIZE {
        digest.add(&format!("{:?}", OptimizeObjective::parse(raw)));
    }
    assert!(
        optimized > CASES / 10,
        "too few optimized results: {optimized}"
    );
    check("optimize", &digest, EXPECTED_OPTIMIZE);
}

#[test]
fn the_port_frees_what_it_allocates() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = port_live_allocations();
    let mut rng = Rng(7, false);
    for _ in 0..20 {
        let request = request(&mut rng);
        if let Ok(solution) = resolve(&request) {
            let _ = validate_locked(&request, &solution);
            let root = request.root.package.name.clone();
            let _ = compute_metrics(&solution, &root, &BTreeMap::new());
        }
        let _ = resolve_optimized(&OptimizeRequest {
            resolve: request,
            objectives: vec![],
            metadata: BTreeMap::new(),
            max_solutions: 4,
        });
    }
    assert_eq!(port_live_allocations(), before);
}
