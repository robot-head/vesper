//! The port-backed build (port wave P1 lane 17, #914): resolution, locked
//! validation and optimization run in the Vesper package `vesper/`, which
//! imports `@vesper/loom-calculus` for manifest checks, SemVer matching and
//! solution validation. This module only encodes the request for it and
//! rebuilds the crate's own types from its results. Text parsing and
//! printing (SemVer, the JSON used to compare whole manifests) stay in Rust.
//!
//! # Input words
//!
//! The words start with the calculus tables and use the calculus encodings
//! of manifests and solutions (see `crates/vesper-loom-calculus/src/port.rs`):
//! strings are occurrences, which Vesper ranks so that ids order like the
//! strings, and versions and requirements are table indices. Each root and
//! catalog manifest is followed by its solver fields
//!
//! ```text
//! has_json, [json], has_resolver, [optimize set, has_min, min, block]
//! ```
//!
//! where `json` is the manifest's `serde_json` text, which compares like the
//! manifest. It is written for the root, for catalog manifests with the
//! root's name and version (the only ones `inputs` compares), and for every
//! catalog manifest in `validate_locked`. A request is
//!
//! ```text
//! root manifest, exact root requirement, features set, catalog schema,
//! npackages, manifest*, nsystems, (name, version)*, max_decisions
//! ```
//!
//! `validate_locked` appends the snapshot (a calculus solution) and
//! `nsel, json*`, the JSON of each selection's manifest. `resolve_optimized`
//! appends `max_solutions, nobjectives, objective*` (empty: use the root
//! policy or the defaults) and the metadata, `n, (name, version, cve_count,
//! staleness_secs, trust_cost, repro_attestations, vetted_namespace)*`.
//! `compute_metrics` reads a calculus solution, the root name and the
//! metadata.
//!
//! # Result words
//!
//! All but `compute_metrics` start with `k, occurrence*k`, the
//! representative occurrence of each id, then a tag. An error is
//!
//! ```text
//! 1, kind, decisions, n, (code, package, subject off, len, message off, len)*
//! ```
//!
//! with text in `dp`. A solution is `0, nsel, (manifest, n, feature*)*,
//! nsys, native*`, where a manifest is a catalog position or `u64::MAX` for
//! the root and a native selection a catalog position. The optimizer adds the
//! six metrics, `evaluated_solutions` and `nobjectives, objective*`;
//! `validate_locked` returns the tag alone. `compute_metrics` returns the six
//! metrics.
use crate::optimize::{
    CandidateMetadata, OPTIMIZE_SCHEMA_VERSION, OptimizeObjective, OptimizeRequest,
    OptimizedResolution, SolutionMetrics,
};
use crate::port_binding as binding;
use crate::{ErrorKind, ResolveDiagnostic, ResolveError, ResolveRequest, error};
use semver::{Op, Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use vesper_loom_calculus::{
    Manifest, PackageName, RootRequirement, SCHEMA_VERSION, Selection, Solution, SystemSelection,
};

const NONE: u64 = u64::MAX;

/// Diagnostic codes: the calculus `Code` names (as `from_calculus` prints
/// them), then the solver's own.
const CODES: [&str; 41] = [
    "ManifestSyntax",
    "InvalidCapability",
    "UnsupportedBuildPolicy",
    "InvalidFeature",
    "UnknownFeature",
    "UnknownDependency",
    "InvalidSystemRequirement",
    "DuplicatePackage",
    "DuplicateSystemPackage",
    "MissingRoot",
    "MissingDependency",
    "MissingSystemPackage",
    "VersionMismatch",
    "CapabilityEscalation",
    "EdgeCapabilityEscalation",
    "UnsafeFfiForbidden",
    "unsupported_schema",
    "duplicate_candidate",
    "conflicting_root",
    "invalid_system_name",
    "duplicate_system_candidate",
    "unsupported_policy",
    "unknown_feature",
    "locked_root_mismatch",
    "locked_manifest_mismatch",
    "locked_candidate_missing",
    "locked_features_mismatch",
    "locked_unused_package",
    "locked_package_missing",
    "locked_system_missing",
    "locked_unused_system",
    "locked_system_candidate_missing",
    "budget_exhausted",
    "version_conflict",
    "no_candidate",
    "no_system_candidate",
    "invalid_max_solutions",
    "unknown_optimize_objective",
    "insufficient_repro_attestations",
    "unvetted_namespace",
    "missing_candidate_metadata",
];

const KINDS: [ErrorKind; 5] = [
    ErrorKind::InvalidInput,
    ErrorKind::UnsupportedPolicy,
    ErrorKind::Unsatisfiable,
    ErrorKind::BudgetExhausted,
    ErrorKind::LockedMismatch,
];

const OBJECTIVES: [OptimizeObjective; 5] = [
    OptimizeObjective::ZeroCve,
    OptimizeObjective::MinCapabilitySurface,
    OptimizeObjective::MinDupMajors,
    OptimizeObjective::MinStaleness,
    OptimizeObjective::MinTrustCost,
];

fn objective_word(o: OptimizeObjective) -> u64 {
    OBJECTIVES.iter().position(|x| *x == o).unwrap_or(0) as u64
}

/// `OptimizeObjective::parse`, through the port.
pub(crate) fn objective(raw: &str) -> Option<OptimizeObjective> {
    OBJECTIVES
        .get(binding::objective_code(raw) as usize)
        .copied()
}

fn json(m: &Manifest) -> String {
    serde_json::to_string(m).expect("manifest DTOs contain no fallible JSON values")
}

#[derive(Default)]
struct Enc {
    body: Vec<u64>,
    text: Vec<u8>,
    occ: Vec<(u64, u64)>,
    interned: HashMap<String, u64>,
    vers: Vec<u64>,
    nver: u64,
    reqs: Vec<u64>,
    nreq: u64,
}

impl Enc {
    fn w(&mut self, x: u64) {
        self.body.push(x);
    }
    fn b(&mut self, x: bool) {
        self.w(u64::from(x));
    }
    fn intern(&mut self, s: &str) -> u64 {
        if let Some(&i) = self.interned.get(s) {
            return i;
        }
        let i = self.occ.len() as u64;
        self.occ.push((self.text.len() as u64, s.len() as u64));
        self.text.extend_from_slice(s.as_bytes());
        self.interned.insert(s.to_owned(), i);
        i
    }
    fn s(&mut self, s: &str) {
        let i = self.intern(s);
        self.w(i);
    }
    fn set<'s>(&mut self, items: impl ExactSizeIterator<Item = &'s str>) {
        self.w(items.len() as u64);
        for s in items {
            self.s(s);
        }
    }
    fn version(&mut self, v: &Version) -> u64 {
        let entry = [
            v.major,
            v.minor,
            v.patch,
            self.intern(v.pre.as_str()),
            self.intern(v.build.as_str()),
            self.intern(&v.to_string()),
        ];
        self.vers.extend(entry);
        self.nver += 1;
        self.nver - 1
    }
    fn req(&mut self, r: &VersionReq) -> u64 {
        let text = self.intern(&r.to_string());
        self.reqs.extend([text, r.comparators.len() as u64]);
        for c in &r.comparators {
            let op = match c.op {
                Op::Exact => 0,
                Op::Greater => 1,
                Op::GreaterEq => 2,
                Op::Less => 3,
                Op::LessEq => 4,
                Op::Tilde => 5,
                Op::Caret => 6,
                Op::Wildcard => 7,
                // An operator the calculus port does not know matches nothing.
                _ => 8,
            };
            let pre = self.intern(c.pre.as_str());
            self.reqs.extend([
                op,
                c.major,
                c.minor.unwrap_or(0),
                u64::from(c.minor.is_some()),
                c.patch.unwrap_or(0),
                u64::from(c.patch.is_some()),
                pre,
            ]);
        }
        self.nreq += 1;
        self.nreq - 1
    }
    /// The calculus manifest encoding.
    fn manifest(&mut self, m: &Manifest) {
        self.s(m.package.name.as_str());
        let v = self.version(&m.package.version);
        self.w(v);
        self.set(m.package.capability_ceiling.iter().map(String::as_str));
        self.b(m.package.forbid_unsafe_ffi);
        self.w(m.features.len() as u64);
        for (name, feature) in &m.features {
            self.s(name);
            self.set(feature.enables_effects.iter().map(String::as_str));
            self.set(feature.deps.iter().map(PackageName::as_str));
            self.set(feature.features.iter().map(String::as_str));
            self.w(feature.sys_deps.len() as u64);
            for source in &feature.sys_deps {
                self.s(source);
                // Text decoding stays in Rust: split at the first whitespace
                // and parse the SemVer requirement; Vesper checks the name.
                let split = source
                    .split_once(char::is_whitespace)
                    .and_then(|(name, req)| {
                        VersionReq::parse(req.trim()).ok().map(|req| (name, req))
                    });
                match split {
                    Some((name, req)) => {
                        self.b(true);
                        self.s(name);
                        let r = self.req(&req);
                        self.w(r);
                    }
                    None => {
                        self.b(false);
                        self.s("");
                        self.w(0);
                    }
                }
            }
        }
        self.w(m.dependencies.len() as u64);
        for (name, dep) in &m.dependencies {
            self.s(name.as_str());
            let r = self.req(&dep.version);
            self.w(r);
            self.set(dep.effects.iter().map(String::as_str));
            self.b(dep.optional);
            self.set(dep.features.iter().map(String::as_str));
        }
        match &m.build_sandbox {
            Some(sandbox) => {
                self.b(true);
                self.b(sandbox.allow_network);
                let paths = sandbox
                    .script
                    .iter()
                    .chain(&sandbox.wasi_read_paths)
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                self.set(paths.into_iter());
            }
            None => self.b(false),
        }
    }
    /// The solver fields of a root or catalog manifest.
    fn extra(&mut self, m: &Manifest, with_json: bool) {
        self.b(with_json);
        if with_json {
            self.s(&json(m));
        }
        match &m.resolver {
            Some(resolver) => {
                let policy = &resolver.policy;
                self.b(true);
                self.set(policy.optimize.iter().map(String::as_str));
                self.b(policy.min_repro_attestations.is_some());
                self.w(u64::from(policy.min_repro_attestations.unwrap_or(0)));
                self.b(policy.block_unvetted_namespaces);
            }
            None => self.b(false),
        }
    }
    /// The calculus solution encoding.
    fn solution(&mut self, s: &Solution) {
        self.w(u64::from(s.schema_version));
        self.w(s.roots.len() as u64);
        for root in &s.roots {
            self.s(root.name.as_str());
            let r = self.req(&root.version);
            self.w(r);
            self.set(root.features.iter().map(String::as_str));
        }
        self.w(s.packages.len() as u64);
        for selection in &s.packages {
            let json = serde_json::to_string(selection)
                .expect("manifest DTOs contain no fallible JSON values");
            self.s(&json);
            self.set(selection.features.iter().map(String::as_str));
            self.manifest(&selection.manifest);
        }
        self.w(s.system_packages.len() as u64);
        for system in &s.system_packages {
            self.s(&system.name);
            let v = self.version(&system.version);
            self.w(v);
        }
    }
    fn request(&mut self, r: &ResolveRequest, json_all: bool) {
        let root = &r.root;
        self.manifest(root);
        self.extra(root, true);
        let exact = exact(root);
        let e = self.req(&exact);
        self.w(e);
        self.set(r.features.iter().map(String::as_str));
        self.w(u64::from(r.catalog.schema_version));
        self.w(r.catalog.packages.len() as u64);
        for m in &r.catalog.packages {
            self.manifest(m);
            let same =
                m.package.name == root.package.name && m.package.version == root.package.version;
            self.extra(m, json_all || same);
        }
        self.w(r.catalog.system_packages.len() as u64);
        for system in &r.catalog.system_packages {
            self.s(&system.name);
            let v = self.version(&system.version);
            self.w(v);
        }
        self.w(r.max_decisions as u64);
    }
    fn metadata(&mut self, metadata: &BTreeMap<(PackageName, Version), CandidateMetadata>) {
        self.w(metadata.len() as u64);
        for ((name, version), m) in metadata {
            self.s(name.as_str());
            let v = self.version(version);
            self.w(v);
            self.w(m.cve_count);
            self.w(m.staleness_secs);
            self.w(m.trust_cost);
            self.w(u64::from(m.repro_attestations));
            self.b(m.vetted_namespace);
        }
    }
    fn words(&self) -> Vec<u64> {
        let mut words = Vec::with_capacity(
            3 + 2 * self.occ.len() + self.vers.len() + self.reqs.len() + self.body.len(),
        );
        words.push(self.occ.len() as u64);
        for &(off, len) in &self.occ {
            words.extend([off, len]);
        }
        words.push(self.nver);
        words.extend(&self.vers);
        words.push(self.nreq);
        words.extend(&self.reqs);
        words.extend(&self.body);
        words
    }
}

fn exact(root: &Manifest) -> VersionReq {
    VersionReq::parse(&format!("={}", root.package.version))
        .expect("Version formats as a valid exact requirement")
}

/// Reads one result.
struct Dec {
    words: Vec<u64>,
    dp: Vec<u8>,
    at: usize,
    strings: Vec<String>,
}

impl Dec {
    fn new(enc: &Enc, s: &binding::S) -> Self {
        let r = s.as_ref();
        let mut d = Dec {
            words: r.out().to_vec(),
            dp: r.dp().to_vec(),
            at: 0,
            strings: vec![],
        };
        let k = d.n();
        for _ in 0..k {
            let occ = d.n();
            let (off, len) = enc.occ[occ];
            let s = &enc.text[off as usize..(off + len) as usize];
            d.strings
                .push(String::from_utf8(s.to_vec()).expect("interned strings are UTF-8"));
        }
        d
    }
    fn w(&mut self) -> u64 {
        let x = self.words[self.at];
        self.at += 1;
        x
    }
    fn n(&mut self) -> usize {
        self.w() as usize
    }
    fn string(&mut self) -> String {
        let id = self.n();
        self.strings[id].clone()
    }
    fn text(&mut self) -> String {
        let off = self.n();
        let len = self.n();
        String::from_utf8(self.dp[off..off + len].to_vec()).expect("diagnostic text is UTF-8")
    }
    fn error(&mut self) -> ResolveError {
        let kind = KINDS[self.n()].clone();
        let decisions = self.n();
        let n = self.n();
        let diagnostics = (0..n)
            .map(|_| {
                let code = CODES[self.n()].to_owned();
                let package = match self.w() {
                    NONE => None,
                    id => Some(
                        PackageName::parse(self.strings[id as usize].clone())
                            .expect("diagnostic packages are validated package names"),
                    ),
                };
                let subject = self.text();
                let message = self.text();
                ResolveDiagnostic {
                    code,
                    package,
                    subject,
                    message,
                }
            })
            .collect();
        error(kind, decisions, diagnostics)
    }
    /// A tagged result: the error, or Ok(()) with the words that follow.
    fn tag(&mut self) -> Result<(), ResolveError> {
        match self.w() {
            0 => Ok(()),
            _ => Err(self.error()),
        }
    }
    fn solution(&mut self, request: &ResolveRequest) -> Solution {
        let nsel = self.n();
        let packages = (0..nsel)
            .map(|_| {
                let manifest = match self.w() {
                    NONE => &request.root,
                    i => &request.catalog.packages[i as usize],
                };
                let nf = self.n();
                let features = (0..nf).map(|_| self.string()).collect::<BTreeSet<_>>();
                Selection {
                    manifest: manifest.clone(),
                    features,
                }
            })
            .collect();
        let nsys = self.n();
        let system_packages = (0..nsys)
            .map(|_| request.catalog.system_packages[self.n()].clone())
            .collect::<Vec<SystemSelection>>();
        Solution {
            schema_version: SCHEMA_VERSION,
            roots: vec![RootRequirement {
                name: request.root.package.name.clone(),
                version: exact(&request.root),
                features: request.features.clone(),
            }],
            packages,
            system_packages,
        }
    }
    fn metrics(&mut self) -> SolutionMetrics {
        SolutionMetrics {
            total_cves: self.w(),
            capability_surface: self.w(),
            duplicate_majors: self.w(),
            total_staleness_secs: self.w(),
            total_trust_cost: self.w(),
            unknown_metadata_candidates: self.w(),
        }
    }
}

pub(crate) fn resolve(request: &ResolveRequest) -> Result<Solution, ResolveError> {
    let mut enc = Enc::default();
    enc.request(request, false);
    let s = binding::resolve(&enc.words(), &enc.text);
    let mut d = Dec::new(&enc, &s);
    d.tag()?;
    Ok(d.solution(request))
}

pub(crate) fn validate_locked(
    request: &ResolveRequest,
    locked: &Solution,
) -> Result<(), ResolveError> {
    let mut enc = Enc::default();
    enc.request(request, true);
    enc.solution(locked);
    enc.w(locked.packages.len() as u64);
    for selection in &locked.packages {
        enc.s(&json(&selection.manifest));
    }
    let s = binding::validate_locked(&enc.words(), &enc.text);
    Dec::new(&enc, &s).tag()
}

pub(crate) fn resolve_optimized(
    request: &OptimizeRequest,
) -> Result<OptimizedResolution, ResolveError> {
    let mut enc = Enc::default();
    enc.request(&request.resolve, false);
    enc.w(request.max_solutions as u64);
    enc.w(request.objectives.len() as u64);
    for &o in &request.objectives {
        enc.w(objective_word(o));
    }
    enc.metadata(&request.metadata);
    let s = binding::resolve_optimized(&enc.words(), &enc.text);
    let mut d = Dec::new(&enc, &s);
    d.tag()?;
    let solution = d.solution(&request.resolve);
    let metrics = d.metrics();
    let evaluated_solutions = d.n();
    let n = d.n();
    let objective_order = (0..n).map(|_| OBJECTIVES[d.n()]).collect();
    Ok(OptimizedResolution {
        schema_version: OPTIMIZE_SCHEMA_VERSION,
        solution,
        metrics,
        evaluated_solutions,
        objective_order,
    })
}

pub(crate) fn compute_metrics(
    sol: &Solution,
    root: &PackageName,
    metadata: &BTreeMap<(PackageName, Version), CandidateMetadata>,
) -> SolutionMetrics {
    let mut enc = Enc::default();
    enc.solution(sol);
    enc.s(root.as_str());
    enc.metadata(metadata);
    let s = binding::compute_metrics(&enc.words(), &enc.text);
    let r = s.as_ref();
    let out = r.out();
    let w = |i: usize| out.get(i).unwrap_or(0);
    SolutionMetrics {
        total_cves: w(0),
        capability_surface: w(1),
        duplicate_majors: w(2),
        total_staleness_secs: w(3),
        total_trust_cost: w(4),
        unknown_metadata_candidates: w(5),
    }
}

/// Live Vesper allocations, for leak tests.
pub fn live_allocations() -> i64 {
    binding::live_allocations()
}
