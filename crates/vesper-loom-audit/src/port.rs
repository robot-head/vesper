//! The port-backed build (port wave P3a lane 4, #1039): catalog and policy
//! validation, the license expressions, the audit preflight, the dependency
//! paths and the findings run in the Vesper package `vesper/`, which imports
//! `@vesper/loom-calculus` for its decoder, SemVer order and solution
//! validator. This module only encodes the inputs for it and rebuilds the
//! crate's own types from its results. JSON parsing, the serialized-size
//! checks (`input::check_size`) and the BLAKE3 context digests stay in Rust.
//!
//! # Input words
//!
//! The words start with the calculus tables and use the calculus encoding
//! of a solution (see `crates/vesper-loom-calculus/src/port.rs`): strings
//! are occurrences, which Vesper ranks so that ids order like the strings,
//! and versions are table indices. Every body starts with the head
//!
//! ```text
//! the 13 Limits fields in declaration order, catalog_ok, policy_ok, solution_ok
//! ```
//!
//! where the three flags are the results of `input::check_size`. Then
//!
//! ```text
//! catalog: schema, id, nadv, (id, kind, name, nrange,
//!          (introduced, has_fixed, fixed, include_prerelease)*, severity,
//!          summary)*, nnative, (name, version, expression)*
//! policy:  schema, minimum_severity, nallow, expression*
//! ```
//!
//! with kind 0 for a Vesper identity and 1 for a native one and severities
//! numbered Low 0 to Critical 3. `validate_catalog` reads the head and a
//! catalog, `validate_policy` the head and a policy, and `audit` the head,
//! the catalog, the policy, the solution and `nsel, (has_license,
//! [license])*`. `range_contains` reads a range and a version.
//!
//! # Result words
//!
//! An error is `1, kind, message offset, length` with the text in `dp` and
//! the kind numbered like `Error`; for `InvalidSolution` the diagnostics
//! come from the calculus validator. A successful validation is `0`. A
//! report is
//!
//! ```text
//! 0, k, occurrence*k, outcome, has_policy_violations, nvesper, nnative,
//! ngroups, (package, npaths, (n, package*)*, nissues, issue*)*
//! ```
//!
//! where the table maps string ids to representative occurrences, a package
//! is `kind, name, version` and an issue is `0, advisory` (a catalog
//! position), `1, source, index` (denied) or `2, source, index, reason
//! offset, length` (unknown). A source is 0 for an absent license, 1 for a
//! selection's (index into `packages`) and 2 for a native claim's (index
//! into `native_licenses`).
use crate::port_binding as binding;
use crate::{
    AffectedRange, AuditContext, AuditReport, Catalog, Error, Finding, FindingKind, Limits,
    Outcome, PackageIdentity, Policy, SCHEMA_VERSION, SelectedPackage, digest, input,
};
use semver::{Op, Version, VersionReq};
use std::collections::HashMap;
use vesper_loom_calculus::{Manifest, PackageName, Solution, validate_solution};

#[derive(Default)]
struct Enc<'a> {
    body: Vec<u64>,
    text: Vec<u8>,
    occ: Vec<(u64, u64)>,
    interned: HashMap<&'a str, u64>,
    owned: HashMap<String, u64>,
    vers: Vec<u64>,
    nver: u64,
    reqs: Vec<u64>,
    nreq: u64,
    versions: Vec<&'a Version>,
}

impl<'a> Enc<'a> {
    fn w(&mut self, x: u64) {
        self.body.push(x);
    }
    fn b(&mut self, x: bool) {
        self.w(u64::from(x));
    }
    fn push_occ(&mut self, s: &str) -> u64 {
        let i = self.occ.len() as u64;
        self.occ.push((self.text.len() as u64, s.len() as u64));
        self.text.extend_from_slice(s.as_bytes());
        i
    }
    /// A borrowed string: interned without copying the key.
    fn intern(&mut self, s: &'a str) -> u64 {
        if let Some(&i) = self.interned.get(s) {
            return i;
        }
        let i = self.push_occ(s);
        self.interned.insert(s, i);
        i
    }
    /// A string built here (version and requirement text, JSON).
    fn intern_owned(&mut self, s: String) -> u64 {
        if let Some(&i) = self.interned.get(s.as_str()) {
            return i;
        }
        if let Some(&i) = self.owned.get(&s) {
            return i;
        }
        let i = self.push_occ(&s);
        self.owned.insert(s, i);
        i
    }
    fn s(&mut self, s: &'a str) {
        let i = self.intern(s);
        self.w(i);
    }
    fn set(&mut self, items: impl ExactSizeIterator<Item = &'a str>) {
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
            self.intern_owned(v.to_string()),
        ];
        self.vers.extend(entry);
        self.versions.push(v);
        self.nver += 1;
        self.nver - 1
    }
    fn req(&mut self, r: &VersionReq) -> u64 {
        let text = self.intern_owned(r.to_string());
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
                _ => 8,
            };
            let pre = self.intern_owned(c.pre.as_str().to_owned());
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
    /// The calculus solution encoding.
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
            let j = self.intern_owned(json);
            self.w(j);
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
    fn head(&mut self, l: &Limits, catalog_ok: bool, policy_ok: bool, solution_ok: bool) {
        for x in [
            l.max_input_bytes,
            l.max_string_bytes,
            l.max_json_depth,
            l.max_json_nodes,
            l.max_entries,
            l.max_packages,
            l.max_edges,
            l.max_roots,
            l.max_path_depth,
            l.max_path_nodes,
            l.max_license_depth,
            l.max_license_tokens,
            l.max_work,
        ] {
            self.w(x as u64);
        }
        self.b(catalog_ok);
        self.b(policy_ok);
        self.b(solution_ok);
    }
    fn range(&mut self, r: &'a AffectedRange) {
        let intro = self.version(&r.introduced);
        self.w(intro);
        match &r.fixed {
            Some(fixed) => {
                let v = self.version(fixed);
                self.b(true);
                self.w(v);
            }
            None => {
                self.b(false);
                self.w(0);
            }
        }
        self.b(r.include_prerelease);
    }
    fn identity(&mut self, identity: &'a PackageIdentity) {
        match identity {
            PackageIdentity::Vesper { name } => {
                self.w(0);
                self.s(name.as_str());
            }
            PackageIdentity::Native { name } => {
                self.w(1);
                self.s(name);
            }
        }
    }
    fn catalog(&mut self, c: &'a Catalog) {
        self.w(u64::from(c.schema_version));
        self.s(&c.catalog_id);
        self.w(c.advisories.len() as u64);
        for a in &c.advisories {
            self.s(&a.id);
            self.identity(&a.package);
            self.w(a.affected.len() as u64);
            for r in &a.affected {
                self.range(r);
            }
            self.w(a.severity as u64);
            self.s(&a.summary);
        }
        self.w(c.native_licenses.len() as u64);
        for n in &c.native_licenses {
            self.s(&n.name);
            let v = self.version(&n.version);
            self.w(v);
            self.s(&n.expression);
        }
    }
    fn policy(&mut self, p: &'a Policy) {
        self.w(u64::from(p.schema_version));
        self.w(p.minimum_severity as u64);
        self.set(p.allowed_licenses.iter().map(String::as_str));
    }
    fn licenses(&mut self, s: &'a Solution) {
        self.w(s.packages.len() as u64);
        for selection in &s.packages {
            match selection.manifest.package.license.as_deref() {
                Some(license) => {
                    self.b(true);
                    self.s(license);
                }
                None => self.b(false),
            }
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
    rocc: Vec<u64>,
}

impl<'e, 'a> Dec<'e, 'a> {
    fn new(enc: &'e Enc<'a>, a: &binding::A) -> Self {
        let r = a.as_ref();
        Dec {
            enc,
            words: r.out().to_vec(),
            pool: r.dp().to_vec(),
            at: 0,
            rocc: vec![],
        }
    }
    fn w(&mut self) -> u64 {
        let x = self.words[self.at];
        self.at += 1;
        x
    }
    fn n(&mut self) -> usize {
        self.w() as usize
    }
    fn text(&mut self) -> String {
        let off = self.n();
        let len = self.n();
        String::from_utf8(self.pool[off..off + len].to_vec()).expect("pool text is UTF-8")
    }
    /// `0`, or the error.
    fn status(&mut self, solution: Option<&Solution>) -> Result<(), Error> {
        if self.w() == 0 {
            return Ok(());
        }
        let kind = self.w();
        let message = self.text();
        Err(match kind {
            0 => Error::InvalidInput(message),
            1 => Error::InvalidCatalog(message),
            2 => Error::InvalidPolicy(message),
            3 => Error::InvalidSolution(
                validate_solution(solution.expect("only audit validates a solution")).diagnostics,
            ),
            _ => Error::Limit(message),
        })
    }
    fn table(&mut self) {
        let k = self.n();
        self.rocc = (0..k).map(|_| self.w()).collect();
    }
    fn string(&mut self) -> &'e str {
        let id = self.n();
        let (off, len) = self.enc.occ[self.rocc[id] as usize];
        std::str::from_utf8(&self.enc.text[off as usize..(off + len) as usize])
            .expect("interned strings are UTF-8")
    }
    fn package(&mut self) -> SelectedPackage {
        let kind = self.w();
        let name = self.string();
        let identity = if kind == 0 {
            PackageIdentity::Vesper {
                name: PackageName::parse(name).expect("graph names are package names"),
            }
        } else {
            PackageIdentity::Native { name: name.into() }
        };
        let v = self.n();
        SelectedPackage {
            identity,
            version: self.enc.versions[v].clone(),
        }
    }
}

pub(crate) fn validate_catalog(catalog: &Catalog, limits: &Limits) -> Result<(), Error> {
    let mut enc = Enc::default();
    enc.head(
        limits,
        input::check_size(catalog, limits).is_ok(),
        true,
        true,
    );
    enc.catalog(catalog);
    let a = binding::validate_catalog(&enc.words(), &enc.text);
    Dec::new(&enc, &a).status(None)
}

pub(crate) fn validate_policy(policy: &Policy, limits: &Limits) -> Result<(), Error> {
    let mut enc = Enc::default();
    enc.head(
        limits,
        true,
        input::check_size(policy, limits).is_ok(),
        true,
    );
    enc.policy(policy);
    let a = binding::validate_policy(&enc.words(), &enc.text);
    Dec::new(&enc, &a).status(None)
}

pub(crate) fn range_contains(range: &AffectedRange, version: &Version) -> bool {
    let mut enc = Enc::default();
    enc.range(range);
    let v = enc.version(version);
    enc.w(v);
    binding::range_contains(&enc.words(), &enc.text)
}

pub(crate) fn audit(
    solution: &Solution,
    catalog: &Catalog,
    policy: &Policy,
    limits: &Limits,
) -> Result<AuditReport, Error> {
    // An oversized input is never encoded: the Rust order of checks is
    // reproduced here (limits, catalog, policy, roots, solution size), each
    // port call seeing only inputs that fit the byte budget.
    let sizes = [
        input::check_size(catalog, limits),
        input::check_size(policy, limits),
        input::check_size(solution, limits),
    ];
    if let Some(oversized) = sizes.iter().position(Result::is_err) {
        limits.validate()?;
        if oversized > 0 {
            validate_catalog(catalog, limits)?;
        }
        if oversized > 1 {
            validate_policy(policy, limits)?;
            if solution.roots.is_empty() {
                return Err(Error::InvalidInput(
                    "audit requires at least one declared root package".into(),
                ));
            }
        }
        return Err(Error::Limit("programmatic input bytes".into()));
    }
    let mut enc = Enc::default();
    enc.head(limits, true, true, true);
    enc.catalog(catalog);
    enc.policy(policy);
    enc.solution(solution);
    enc.licenses(solution);
    let a = binding::audit(&enc.words(), &enc.text);
    let mut d = Dec::new(&enc, &a);
    d.status(Some(solution))?;
    d.table();
    let outcome = match d.w() {
        0 => Outcome::Clean,
        1 => Outcome::Findings,
        _ => Outcome::Unknown,
    };
    let has_policy_violations = d.w() != 0;
    let reachable_vesper_packages = d.n();
    let reachable_native_packages = d.n();
    let ngroups = d.n();
    let expression = |source: u64, index: usize| -> Option<&str> {
        match source {
            1 => solution.packages[index].manifest.package.license.as_deref(),
            2 => Some(catalog.native_licenses[index].expression.as_str()),
            _ => None,
        }
    };
    let mut findings = Vec::new();
    for _ in 0..ngroups {
        let package = d.package();
        let npaths = d.n();
        let paths = (0..npaths)
            .map(|_| {
                let n = d.n();
                (0..n).map(|_| d.package()).collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let nissues = d.n();
        for _ in 0..nissues {
            let issue = match d.w() {
                0 => {
                    let advisory = &catalog.advisories[d.n()];
                    FindingKind::Vulnerability {
                        advisory_id: advisory.id.clone(),
                        severity: advisory.severity,
                        summary: advisory.summary.clone(),
                    }
                }
                1 => {
                    let source = d.w();
                    let index = d.n();
                    FindingKind::LicenseDenied {
                        expression: expression(source, index)
                            .expect("a denied license is declared")
                            .into(),
                    }
                }
                _ => {
                    let source = d.w();
                    let index = d.n();
                    FindingKind::LicenseUnknown {
                        expression: expression(source, index).map(str::to_string),
                        reason: d.text(),
                    }
                }
            };
            findings.push(Finding {
                package: package.clone(),
                issue,
                paths: paths.clone(),
            });
        }
    }
    Ok(AuditReport {
        schema_version: SCHEMA_VERSION,
        context: AuditContext {
            contract: "vesper.loom.offline-audit/1".into(),
            solution_digest: digest("vesper.loom.offline-audit/1 solution", solution)?,
            catalog_digest: digest("vesper.loom.offline-audit/1 catalog", catalog)?,
            policy_digest: digest("vesper.loom.offline-audit/1 policy", policy)?,
            limits_digest: digest("vesper.loom.offline-audit/1 limits", limits)?,
        },
        catalog_id: catalog.catalog_id.clone(),
        outcome,
        has_policy_violations,
        reachable_vesper_packages,
        reachable_native_packages,
        findings,
    })
}

/// Live Vesper allocations, for leak tests.
pub fn live_allocations() -> i64 {
    binding::live_allocations()
}
