//! Bounded offline audits of declared Loom dependency graphs.
#![doc = include_str!("../README.md")]

mod input;
mod license;

use semver::Version;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use vesper_loom_calculus::{Diagnostic, PackageGraph, PackageName, Solution, validate_solution};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_string_bytes: usize,
    pub max_json_depth: usize,
    pub max_json_nodes: usize,
    pub max_entries: usize,
    pub max_packages: usize,
    pub max_edges: usize,
    pub max_roots: usize,
    pub max_path_depth: usize,
    pub max_path_nodes: usize,
    pub max_license_depth: usize,
    pub max_license_tokens: usize,
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1 << 20,
            max_string_bytes: 2048,
            max_json_depth: 32,
            max_json_nodes: 65536,
            max_entries: 4096,
            max_packages: 2048,
            max_edges: 8192,
            max_roots: 32,
            max_path_depth: 128,
            max_path_nodes: 65536,
            max_license_depth: 32,
            max_license_tokens: 256,
            max_work: 10_000_000,
        }
    }
}
impl Limits {
    fn validate(&self) -> Result<(), Error> {
        if self.max_json_depth > 64 || self.max_license_depth > 64 || self.max_license_tokens > 1024
        {
            return Err(Error::Limit(
                "unsafe parser depth/token limit (maximum 64/64/1024)".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum Error {
    InvalidInput(String),
    InvalidCatalog(String),
    InvalidPolicy(String),
    InvalidSolution(Vec<Diagnostic>),
    Limit(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(s) => write!(f, "invalid audit input: {s}"),
            Self::InvalidCatalog(s) => write!(f, "invalid local advisory catalog: {s}"),
            Self::InvalidPolicy(s) => write!(f, "invalid audit policy: {s}"),
            Self::InvalidSolution(ds) => write!(
                f,
                "invalid Loom solution: {} validation diagnostic(s)",
                ds.len()
            ),
            Self::Limit(s) => write!(f, "audit budget exhausted: {s}"),
        }
    }
}
impl std::error::Error for Error {}

/// Separate identity domains even if names happen to be similar. Native means
/// the declarative system selection, never an observed installed binary.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "ecosystem", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageIdentity {
    Vesper { name: PackageName },
    Native { name: String },
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedPackage {
    pub identity: PackageIdentity,
    pub version: Version,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Moderate,
    High,
    Critical,
}

/// Inclusive lower / exclusive upper SemVer precedence interval. No OSV event
/// syntax, npm ranges or comparator strings are accepted as this schema.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedRange {
    pub introduced: Version,
    #[serde(deserialize_with = "required_optional_version")]
    pub fixed: Option<Version>,
    pub include_prerelease: bool,
}
fn required_optional_version<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Option<Version>, D::Error> {
    Option::<Version>::deserialize(decoder)
}
impl AffectedRange {
    pub fn contains(&self, version: &Version) -> bool {
        (self.include_prerelease || version.pre.is_empty())
            && version.cmp_precedence(&self.introduced) != Ordering::Less
            && self
                .fixed
                .as_ref()
                .is_none_or(|fixed| version.cmp_precedence(fixed) == Ordering::Less)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Advisory {
    pub id: String,
    pub package: PackageIdentity,
    pub affected: Vec<AffectedRange>,
    pub severity: Severity,
    pub summary: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLicense {
    pub name: String,
    pub version: Version,
    pub expression: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema_version: u32,
    pub catalog_id: String,
    pub advisories: Vec<Advisory>,
    pub native_licenses: Vec<NativeLicense>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema_version: u32,
    pub minimum_severity: Severity,
    /// Exact supported SPDX license or license WITH exception grants.
    pub allowed_licenses: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Clean,
    Findings,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FindingKind {
    Vulnerability {
        advisory_id: String,
        severity: Severity,
        summary: String,
    },
    LicenseDenied {
        expression: String,
    },
    LicenseUnknown {
        expression: Option<String>,
        reason: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub package: SelectedPackage,
    pub issue: FindingKind,
    /// One shortest deterministic dependency path from EACH reaching root.
    /// This is package reachability, not a call-graph exploitability proof.
    pub paths: Vec<Vec<SelectedPackage>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditContext {
    pub contract: String,
    pub solution_digest: String,
    pub catalog_digest: String,
    pub policy_digest: String,
    pub limits_digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReport {
    pub schema_version: u32,
    pub context: AuditContext,
    pub catalog_id: String,
    pub outcome: Outcome,
    pub has_policy_violations: bool,
    pub reachable_vesper_packages: usize,
    pub reachable_native_packages: usize,
    pub findings: Vec<Finding>,
}

pub fn parse_solution(bytes: &[u8], limits: &Limits) -> Result<Solution, Error> {
    input::parse(bytes, limits)
}
pub fn parse_catalog(bytes: &[u8], limits: &Limits) -> Result<Catalog, Error> {
    let catalog = input::parse(bytes, limits)?;
    validate_catalog(&catalog, limits)?;
    Ok(catalog)
}
pub fn parse_policy(bytes: &[u8], limits: &Limits) -> Result<Policy, Error> {
    let policy = input::parse(bytes, limits)?;
    validate_policy(&policy, limits)?;
    Ok(policy)
}

struct Work {
    remaining: usize,
}
impl Work {
    fn spend(&mut self, amount: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(amount)
            .ok_or_else(|| Error::Limit("logical work".into()))?;
        Ok(())
    }
}
fn native_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
}
fn identity_valid(identity: &PackageIdentity) -> bool {
    match identity {
        PackageIdentity::Vesper { .. } => true,
        PackageIdentity::Native { name } => native_name(name),
    }
}
fn validate_catalog(catalog: &Catalog, limits: &Limits) -> Result<(), Error> {
    input::check_size(catalog, limits)?;
    let bad = |s: &str| Error::InvalidCatalog(s.into());
    if catalog.schema_version != SCHEMA_VERSION {
        return Err(bad("unsupported schema version"));
    }
    if catalog.catalog_id.is_empty() || catalog.catalog_id.len() > limits.max_string_bytes {
        return Err(bad("invalid catalog identity"));
    }
    let mut remaining = Work {
        remaining: limits.max_entries,
    };
    remaining.spend(catalog.advisories.len())?;
    remaining.spend(catalog.native_licenses.len())?;
    let mut ids = BTreeSet::new();
    for advisory in &catalog.advisories {
        if advisory.id.is_empty()
            || advisory.id.len() > limits.max_string_bytes
            || !advisory
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
            || !ids.insert(&advisory.id)
        {
            return Err(bad("empty, invalid or duplicate advisory ID"));
        }
        if !identity_valid(&advisory.package) {
            return Err(bad("invalid package identity"));
        }
        if advisory.summary.len() > limits.max_string_bytes {
            return Err(bad("advisory summary exceeds string limit"));
        }
        if advisory.affected.is_empty() {
            return Err(bad("advisory must have an affected range"));
        }
        remaining.spend(advisory.affected.len())?;
        let mut ranges = advisory.affected.iter().collect::<Vec<_>>();
        ranges.sort_by(|a, b| a.introduced.cmp_precedence(&b.introduced));
        let mut previous: Option<&AffectedRange> = None;
        for range in ranges {
            if range
                .fixed
                .as_ref()
                .is_some_and(|v| range.introduced.cmp_precedence(v) != Ordering::Less)
            {
                return Err(bad("affected interval must be nonempty and increasing"));
            }
            if previous.is_some_and(|prev| {
                prev.fixed.as_ref().is_none_or(|fixed| {
                    fixed.cmp_precedence(&range.introduced) == Ordering::Greater
                })
            }) {
                return Err(bad("overlapping or duplicate affected intervals"));
            }
            previous = Some(range);
        }
    }
    let mut native = BTreeSet::new();
    for record in &catalog.native_licenses {
        if !native_name(&record.name) || !native.insert((&record.name, &record.version)) {
            return Err(bad("invalid or duplicate native license identity/version"));
        }
        if record.expression.len() > limits.max_string_bytes {
            return Err(bad("native license exceeds string limit"));
        }
    }
    Ok(())
}
fn validate_policy(policy: &Policy, limits: &Limits) -> Result<BTreeSet<String>, Error> {
    input::check_size(policy, limits)?;
    if policy.schema_version != SCHEMA_VERSION {
        return Err(Error::InvalidPolicy("unsupported schema version".into()));
    }
    if policy.allowed_licenses.len() > limits.max_entries {
        return Err(Error::Limit("license grants".into()));
    }
    license::allowlist(&policy.allowed_licenses, limits)
}

/// Bound the existing calculus validator before invoking it: no audit result is
/// returned for an invalid solution. The complexity estimate is conservative
/// logical work, not wall-clock or instruction metering.
fn preflight(solution: &Solution, limits: &Limits, work: &mut Work) -> Result<(), Error> {
    if solution.roots.is_empty() {
        return Err(Error::InvalidInput(
            "audit requires at least one declared root package".into(),
        ));
    }
    input::check_size(solution, limits)?;
    if solution
        .packages
        .len()
        .saturating_add(solution.system_packages.len())
        > limits.max_packages
        || solution.roots.len() > limits.max_roots
    {
        return Err(Error::Limit("package/root count".into()));
    }
    let mut edges = 0usize;
    let mut declarations = solution.roots.len().saturating_add(1);
    for package in &solution.packages {
        let m = &package.manifest;
        edges = edges.saturating_add(m.dependencies.len());
        declarations = declarations
            .saturating_add(m.package.capability_ceiling.len())
            .saturating_add(package.features.len());
        for f in m.features.values() {
            edges = edges
                .saturating_add(f.sys_deps.len())
                .saturating_add(f.deps.len())
                .saturating_add(f.features.len());
            declarations = declarations
                .saturating_add(f.enables_effects.len())
                .saturating_add(1);
        }
        for d in m.dependencies.values() {
            declarations = declarations
                .saturating_add(d.effects.len())
                .saturating_add(d.features.len());
        }
    }
    if edges > limits.max_edges {
        return Err(Error::Limit("declared edge count".into()));
    }
    let complexity = solution
        .packages
        .len()
        .saturating_add(1)
        .saturating_mul(declarations.saturating_add(1))
        .saturating_mul(
            edges
                .saturating_add(declarations)
                .saturating_add(solution.packages.len())
                .saturating_add(1),
        );
    work.spend(complexity)
}

/// Audits only the graph validated from this exact Solution. Caller-created
/// PackageGraph DTOs cannot bypass calculus validation by claiming validity.
pub fn audit(
    solution: &Solution,
    catalog: &Catalog,
    policy: &Policy,
    limits: &Limits,
) -> Result<AuditReport, Error> {
    limits.validate()?;
    validate_catalog(catalog, limits)?;
    let grants = validate_policy(policy, limits)?;
    let mut work = Work {
        remaining: limits.max_work,
    };
    preflight(solution, limits, &mut work)?;
    let validated = validate_solution(solution);
    let Some(graph) = validated.graph else {
        return Err(Error::InvalidSolution(validated.diagnostics));
    };
    let paths = dependency_paths(&graph, limits, &mut work)?;
    let licenses: BTreeMap<_, _> = solution
        .packages
        .iter()
        .map(|s| {
            (
                s.manifest.package.name.clone(),
                s.manifest.package.license.as_deref(),
            )
        })
        .collect();
    let native_licenses: BTreeMap<_, _> = catalog
        .native_licenses
        .iter()
        .map(|l| ((l.name.as_str(), &l.version), l.expression.as_str()))
        .collect();
    let mut findings = Vec::new();
    let mut report_path_nodes = 0usize;
    let mut unknown = false;
    let mut denied = false;
    for (package, package_paths) in paths {
        let mut issues = Vec::new();
        for advisory in &catalog.advisories {
            work.spend(1)?;
            if advisory.package != package.identity {
                continue;
            }
            let mut affected = false;
            for range in &advisory.affected {
                work.spend(1)?;
                affected |= range.contains(&package.version);
            }
            if affected && advisory.severity >= policy.minimum_severity {
                denied = true;
                issues.push(FindingKind::Vulnerability {
                    advisory_id: advisory.id.clone(),
                    severity: advisory.severity,
                    summary: advisory.summary.clone(),
                });
            }
        }
        issues.sort_by(|a, b| match (a, b) {
            (
                FindingKind::Vulnerability { advisory_id: a, .. },
                FindingKind::Vulnerability { advisory_id: b, .. },
            ) => a.cmp(b),
            _ => Ordering::Equal,
        });
        let expression = match &package.identity {
            PackageIdentity::Vesper { name } => licenses.get(name).copied().flatten(),
            PackageIdentity::Native { name } => native_licenses
                .get(&(name.as_str(), &package.version))
                .copied(),
        };
        work.spend(expression.map_or(1, str::len))?;
        match license::assess(expression, &grants, limits) {
            Ok(true) => {}
            Ok(false) => {
                denied = true;
                issues.push(FindingKind::LicenseDenied {
                    expression: expression.unwrap().into(),
                });
            }
            Err(license::Failure::Limit(reason)) => return Err(Error::Limit(reason)),
            Err(license::Failure::Unknown(reason)) => {
                unknown = true;
                issues.push(FindingKind::LicenseUnknown {
                    expression: expression.map(str::to_string),
                    reason,
                });
            }
        }
        for issue in issues {
            let nodes = package_paths.iter().map(Vec::len).sum();
            work.spend(nodes)?;
            report_path_nodes = report_path_nodes
                .checked_add(nodes)
                .ok_or_else(|| Error::Limit("report path nodes".into()))?;
            if report_path_nodes > limits.max_path_nodes || findings.len() >= limits.max_entries {
                return Err(Error::Limit("report path nodes/findings".into()));
            }
            findings.push(Finding {
                package: package.clone(),
                issue,
                paths: package_paths.clone(),
            });
            if findings.len() > limits.max_entries {
                return Err(Error::Limit("finding count".into()));
            }
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
        outcome: if unknown {
            Outcome::Unknown
        } else if denied {
            Outcome::Findings
        } else {
            Outcome::Clean
        },
        has_policy_violations: denied,
        reachable_vesper_packages: graph.packages.len(),
        reachable_native_packages: graph.system_packages.len(),
        findings,
    })
}

// A content context is an input binding, never feed/source authenticity.
fn digest<T: Serialize>(domain: &'static str, value: &T) -> Result<String, Error> {
    struct Sink(blake3::Hasher);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink(blake3::Hasher::new_derive_key(domain));
    serde_json::to_writer(&mut sink, value).map_err(|e| Error::InvalidInput(e.to_string()))?;
    Ok(sink.0.finalize().to_hex().to_string())
}

type Paths = BTreeMap<SelectedPackage, Vec<Vec<SelectedPackage>>>;
fn dependency_paths(
    graph: &PackageGraph,
    limits: &Limits,
    work: &mut Work,
) -> Result<Paths, Error> {
    let select = |name: &PackageName| SelectedPackage {
        identity: PackageIdentity::Vesper { name: name.clone() },
        version: graph.packages[name].version.clone(),
    };
    let mut result: Paths = BTreeMap::new();
    let mut path_nodes = 0usize;
    for root in &graph.roots {
        let mut queue = VecDeque::from([root.clone()]);
        let mut parents = BTreeMap::from([(root.clone(), None::<PackageName>)]);
        while let Some(name) = queue.pop_front() {
            work.spend(1)?;
            let mut reversed = Vec::new();
            let mut cursor = Some(&name);
            while let Some(node) = cursor {
                work.spend(1)?;
                if reversed.len() >= limits.max_path_depth {
                    return Err(Error::Limit("dependency path depth".into()));
                }
                reversed.push(select(node));
                cursor = parents[node].as_ref();
            }
            reversed.reverse();
            path_nodes = path_nodes
                .checked_add(reversed.len())
                .ok_or_else(|| Error::Limit("dependency path nodes".into()))?;
            if path_nodes > limits.max_path_nodes {
                return Err(Error::Limit("dependency path nodes".into()));
            }
            result
                .entry(select(&name))
                .or_default()
                .push(reversed.clone());
            for edge in &graph.packages[&name].system_dependencies {
                work.spend(1)?;
                if reversed.len() >= limits.max_path_depth {
                    return Err(Error::Limit("native dependency path depth".into()));
                }
                let native = SelectedPackage {
                    identity: PackageIdentity::Native {
                        name: edge.name.clone(),
                    },
                    version: graph.system_packages[&edge.name].clone(),
                };
                work.spend(reversed.len().saturating_add(1))?;
                let mut path = reversed.clone();
                path.push(native.clone());
                // Multiple parents of one native package: retain a shortest
                // path from this root, with lexicographic tie breaking.
                let paths = result.entry(native).or_default();
                if let Some(previous) = paths
                    .iter_mut()
                    .find(|p| p[0].identity == select(root).identity)
                {
                    if (path.len(), &path) < (previous.len(), &*previous) {
                        *previous = path;
                    }
                } else {
                    path_nodes = path_nodes
                        .checked_add(path.len())
                        .ok_or_else(|| Error::Limit("path nodes".into()))?;
                    if path_nodes > limits.max_path_nodes {
                        return Err(Error::Limit("path nodes".into()));
                    }
                    paths.push(path);
                }
            }
            let mut dependencies = graph.packages[&name]
                .dependencies
                .iter()
                .map(|d| d.package.clone())
                .collect::<Vec<_>>();
            dependencies.sort();
            for dependency in dependencies {
                work.spend(1)?;
                if !parents.contains_key(&dependency) {
                    parents.insert(dependency.clone(), Some(name.clone()));
                    queue.push_back(dependency);
                }
            }
        }
    }
    Ok(result)
}
