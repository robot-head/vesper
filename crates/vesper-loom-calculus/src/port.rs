//! The port-backed build (port wave P1 lane 16, #913): the calculus runs in
//! the Vesper package `vesper/` and this module only encodes the decoded
//! DTOs for it and rebuilds the crate's own types from its results. TOML and
//! JSON decoding and SemVer text parsing stay in Rust.
//!
//! # Input words
//!
//! Every string is an occurrence: an index into the string table at the
//! front, whose entries are `(offset, length)` ranges of the text bytes.
//! Vesper sorts the table by bytes, so the rank of a string (its id) orders
//! like the string; sets are written in `BTreeSet` order, which is then
//! ascending by id.
//!
//! ```text
//! nocc, (off, len)*
//! nver, (major, minor, patch, pre, build, text)*          strings: pre, build, text
//! nreq, (text, ncmp, (op, major, minor, has_minor, patch, has_patch, pre)*)*
//! body
//! ```
//!
//! `op` is Exact 0, Greater 1, GreaterEq 2, Less 3, LessEq 4, Tilde 5,
//! Caret 6, Wildcard 7. A set is `n, string*`. A manifest is
//!
//! ```text
//! name, version, ceiling set, forbid_unsafe_ffi,
//! nfeat, (name, enables_effects set, deps set, features set,
//!         nsys, (raw, ok, name, requirement)*)*,
//! ndep, (name, requirement, effects set, optional, features set)*,
//! has_sandbox, [allow_network, paths]       paths: script, then wasi_read_paths
//! ```
//!
//! where a native requirement is `ok = 0` when it does not split at the
//! first whitespace into a name and a parsable SemVer requirement. A
//! solution is
//!
//! ```text
//! schema_version, nroots, (name, requirement, features set)*,
//! nsel, (canonical JSON, features set, manifest)*, nsys, (name, version)*
//! ```
//!
//! The body of `validate_manifest` is one manifest, of `validate_solution`
//! one solution, of `check_upgrade` the baseline and then the candidate.
//! Dependencies are numbered in the order they are written.
//!
//! # Result words
//!
//! All begin with `k, occurrence*k`: the representative occurrence of each
//! id. Text ranges index `pool`. An absent id is `u64::MAX`.
//!
//! ```text
//! diagnostics: n, (code, package, subject off, len, message off, len)*
//! validate_manifest: diagnostics
//! validate_solution: diagnostics, nunused, id*, has_graph,
//!   [nroots, id*, nnodes, (name, version, features, ceiling, required,
//!    forbid, nedges, dependency*, nsys, (name, requirement text)*)*,
//!    nnative, (name, version)*]
//! check_upgrade: allowed, n, (code, package, dependency, snapshot,
//!   message off, len)*, nchanges, (scope, package, dependency, before,
//!   after, added, removed, before_forbid, after_forbid, relaxed)*
//! ```
//!
//! Upgrade codes index `UPGRADE_CODES`; snapshot is 0 none, 1 baseline, 2
//! candidate; a forbid word is 0 none, 1 false, 2 true.
use crate::port_binding as binding;
use crate::{
    AuthorityChange, AuthorityScope, Code, Dependency, DependencyEdge, Diagnostic, Manifest,
    PackageGraph, PackageName, PackageNode, SCHEMA_VERSION, Solution, SystemEdge,
    UPGRADE_SCHEMA_VERSION, UpgradeDiagnostic, UpgradeReport, UpgradeSnapshot, Validation,
};
use semver::{Op, Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const NONE: u64 = u64::MAX;

const CODES: [Code; 16] = [
    Code::ManifestSyntax,
    Code::InvalidCapability,
    Code::UnsupportedBuildPolicy,
    Code::InvalidFeature,
    Code::UnknownFeature,
    Code::UnknownDependency,
    Code::InvalidSystemRequirement,
    Code::DuplicatePackage,
    Code::DuplicateSystemPackage,
    Code::MissingRoot,
    Code::MissingDependency,
    Code::MissingSystemPackage,
    Code::VersionMismatch,
    Code::CapabilityEscalation,
    Code::EdgeCapabilityEscalation,
    Code::UnsafeFfiForbidden,
];

const UPGRADE_CODES: [&str; 4] = [
    "capability_escalation",
    "invalid_snapshot",
    "root_mismatch",
    "unsafe_ffi_policy_relaxed",
];

const SCOPES: [AuthorityScope; 4] = [
    AuthorityScope::RequiredCapabilities,
    AuthorityScope::CapabilityCeiling,
    AuthorityScope::DependencyCeiling,
    AuthorityScope::UnsafeFfiPolicy,
];

/// `PackageName::parse`'s spelling check.
pub(crate) fn name_valid(name: &str) -> bool {
    binding::package_name_valid(name)
}

#[derive(Default)]
struct Enc<'a> {
    body: Vec<u64>,
    text: Vec<u8>,
    occ: Vec<(u64, u64)>,
    interned: HashMap<String, u64>,
    vers: Vec<u64>,
    nver: u64,
    reqs: Vec<u64>,
    nreq: u64,
    versions: Vec<&'a Version>,
    deps: Vec<(&'a PackageName, &'a Dependency)>,
}

impl<'a> Enc<'a> {
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
    fn version(&mut self, v: &'a Version) -> u64 {
        let entry = [
            v.major,
            v.minor,
            v.patch,
            self.intern(v.pre.as_str()),
            self.intern(v.build.as_str()),
            self.intern(&v.to_string()),
        ];
        self.vers.extend(entry);
        self.versions.push(v);
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
                // An operator this port does not know matches nothing.
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
    fn manifest(&mut self, m: &'a Manifest) {
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
            self.deps.push((name, dep));
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
    fn solution(&mut self, s: &'a Solution) {
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

/// Reads one result.
struct Dec<'e, 'a> {
    enc: &'e Enc<'a>,
    words: Vec<u64>,
    pool: Vec<u8>,
    at: usize,
    strings: Vec<String>,
}

impl<'e, 'a> Dec<'e, 'a> {
    fn new(enc: &'e Enc<'a>, l: &binding::L) -> Self {
        let r = l.as_ref();
        let mut d = Dec {
            enc,
            words: r.out().to_vec(),
            pool: r.pool().to_vec(),
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
    fn b(&mut self) -> bool {
        self.w() != 0
    }
    fn string(&mut self) -> String {
        let id = self.n();
        self.strings[id].clone()
    }
    fn name(&mut self) -> PackageName {
        PackageName(self.string())
    }
    fn opt_name(&mut self) -> Option<PackageName> {
        match self.w() {
            NONE => None,
            id => Some(PackageName(self.strings[id as usize].clone())),
        }
    }
    fn text(&mut self) -> String {
        let off = self.n();
        let len = self.n();
        String::from_utf8(self.pool[off..off + len].to_vec()).expect("pool text is UTF-8")
    }
    fn set(&mut self) -> BTreeSet<String> {
        let n = self.n();
        (0..n).map(|_| self.string()).collect()
    }
    fn names(&mut self) -> BTreeSet<PackageName> {
        let n = self.n();
        (0..n).map(|_| self.name()).collect()
    }
    fn version(&mut self) -> Version {
        let i = self.n();
        self.enc.versions[i].clone()
    }
    fn diagnostics(&mut self) -> Vec<Diagnostic> {
        let n = self.n();
        (0..n)
            .map(|_| {
                let code = CODES[self.n()].clone();
                let package = self.opt_name();
                let subject = self.text();
                let message = self.text();
                Diagnostic {
                    code,
                    package,
                    subject,
                    message,
                }
            })
            .collect()
    }
    fn validation(&mut self) -> Validation {
        let diagnostics = self.diagnostics();
        let unused_selections = self.names();
        let graph = self.b().then(|| {
            let roots = self.names();
            let n = self.n();
            let mut packages = BTreeMap::new();
            for _ in 0..n {
                let name = self.name();
                let version = self.version();
                let features = self.set();
                let capability_ceiling = self.set();
                let required_capabilities = self.set();
                let forbid_unsafe_ffi = self.b();
                let ne = self.n();
                let dependencies = (0..ne)
                    .map(|_| {
                        let (package, dep) = self.enc.deps[self.n()];
                        DependencyEdge {
                            package: package.clone(),
                            requirement: dep.version.clone(),
                            allowed_capabilities: dep.effects.clone(),
                            requested_features: dep.features.clone(),
                        }
                    })
                    .collect();
                let ns = self.n();
                let system_dependencies = (0..ns)
                    .map(|_| SystemEdge {
                        name: self.string(),
                        requirement: self.string(),
                    })
                    .collect();
                packages.insert(
                    name,
                    PackageNode {
                        version,
                        features,
                        capability_ceiling,
                        required_capabilities,
                        forbid_unsafe_ffi,
                        dependencies,
                        system_dependencies,
                    },
                );
            }
            let ns = self.n();
            let system_packages = (0..ns).map(|_| (self.string(), self.version())).collect();
            PackageGraph {
                schema_version: SCHEMA_VERSION,
                roots,
                packages,
                system_packages,
            }
        });
        Validation {
            graph,
            diagnostics,
            unused_selections,
        }
    }
    fn forbid(&mut self) -> Option<bool> {
        match self.w() {
            0 => None,
            1 => Some(false),
            _ => Some(true),
        }
    }
    fn upgrade(&mut self) -> UpgradeReport {
        let allowed = self.b();
        let n = self.n();
        let diagnostics = (0..n)
            .map(|_| {
                let code = UPGRADE_CODES[self.n()].to_owned();
                let package = self.opt_name();
                let dependency = self.opt_name();
                let snapshot = match self.w() {
                    0 => None,
                    1 => Some(UpgradeSnapshot::Baseline),
                    _ => Some(UpgradeSnapshot::Candidate),
                };
                let message = self.text();
                UpgradeDiagnostic {
                    code,
                    package,
                    dependency,
                    snapshot,
                    message,
                }
            })
            .collect();
        let n = self.n();
        let changes = (0..n)
            .map(|_| AuthorityChange {
                scope: SCOPES[self.n()],
                package: self.name(),
                dependency: self.opt_name(),
                before: self.set(),
                after: self.set(),
                added: self.set(),
                removed: self.set(),
                before_forbid_unsafe_ffi: self.forbid(),
                after_forbid_unsafe_ffi: self.forbid(),
                unsafe_ffi_relaxed: self.b(),
            })
            .collect();
        UpgradeReport {
            schema_version: UPGRADE_SCHEMA_VERSION,
            allowed,
            diagnostics,
            changes,
        }
    }
}

pub(crate) fn validate_manifest(manifest: &Manifest) -> Vec<Diagnostic> {
    let mut enc = Enc::default();
    enc.manifest(manifest);
    let l = binding::validate_manifest(&enc.words(), &enc.text);
    Dec::new(&enc, &l).diagnostics()
}

pub(crate) fn validate_solution(solution: &Solution) -> Validation {
    let mut enc = Enc::default();
    enc.solution(solution);
    let l = binding::validate_solution(&enc.words(), &enc.text);
    Dec::new(&enc, &l).validation()
}

pub(crate) fn check_upgrade(baseline: &Solution, candidate: &Solution) -> UpgradeReport {
    let mut enc = Enc::default();
    enc.solution(baseline);
    enc.solution(candidate);
    let l = binding::check_upgrade(&enc.words(), &enc.text);
    Dec::new(&enc, &l).upgrade()
}

/// Live Vesper allocations, for leak tests.
pub fn live_allocations() -> i64 {
    binding::live_allocations()
}
