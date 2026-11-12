//! Deterministic, bounded offline dependency search over supplied manifests.
#![doc = include_str!("../README.md")]

use semver::VersionReq;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use vesper_loom_calculus::{
    Diagnostic, Manifest, PackageName, RootRequirement, SCHEMA_VERSION, Selection, Solution,
    SystemSelection, validate_manifest, validate_solution,
};

/// An untrusted local catalog. Alternative versions are candidates, not installations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema_version: u32,
    pub packages: Vec<Manifest>,
    #[serde(default)]
    pub system_packages: Vec<SystemSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    pub root: Manifest,
    #[serde(default)]
    pub features: BTreeSet<String>,
    pub catalog: Catalog,
    /// Maximum candidate assignments explored, excluding the fixed root.
    pub max_decisions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidInput,
    UnsupportedPolicy,
    Unsatisfiable,
    BudgetExhausted,
}

/// Diagnostics describe encountered conflicts; they are not a minimal unsat proof.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveDiagnostic {
    pub code: String,
    pub package: Option<PackageName>,
    pub subject: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveError {
    pub kind: ErrorKind,
    pub decisions: usize,
    pub diagnostics: Vec<ResolveDiagnostic>,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} after {} candidate decisions",
            self.kind, self.decisions
        )?;
        for diagnostic in &self.diagnostics {
            write!(f, "; {}: {}", diagnostic.subject, diagnostic.message)?;
        }
        Ok(())
    }
}
impl std::error::Error for ResolveError {}

fn diagnostic(
    code: &str,
    package: Option<&PackageName>,
    subject: &str,
    message: impl Into<String>,
) -> ResolveDiagnostic {
    ResolveDiagnostic {
        code: code.into(),
        package: package.cloned(),
        subject: subject.into(),
        message: message.into(),
    }
}
fn from_calculus(d: Diagnostic) -> ResolveDiagnostic {
    ResolveDiagnostic {
        code: format!("{:?}", d.code),
        package: d.package,
        subject: d.subject,
        message: d.message,
    }
}
fn error(
    kind: ErrorKind,
    decisions: usize,
    mut diagnostics: Vec<ResolveDiagnostic>,
) -> ResolveError {
    diagnostics.sort();
    diagnostics.dedup();
    ResolveError {
        kind,
        decisions,
        diagnostics,
    }
}

fn system_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'+'))
}
fn system_requirement(source: &str) -> Option<(String, VersionReq)> {
    let (name, requirement) = source.split_once(char::is_whitespace)?;
    system_name_valid(name).then_some(())?;
    Some((name.into(), VersionReq::parse(requirement.trim()).ok()?))
}

struct Inputs<'a> {
    packages: BTreeMap<PackageName, Vec<&'a Manifest>>,
    systems: BTreeMap<String, Vec<&'a SystemSelection>>,
}
fn inputs(request: &ResolveRequest) -> Result<Inputs<'_>, ResolveError> {
    let mut invalid = vec![];
    if request.catalog.schema_version != SCHEMA_VERSION {
        invalid.push(diagnostic(
            "unsupported_schema",
            None,
            "catalog.schema_version",
            "unsupported local catalog schema version",
        ));
    }
    let mut packages = BTreeMap::<PackageName, Vec<&Manifest>>::new();
    let mut identities = BTreeSet::new();
    for manifest in &request.catalog.packages {
        let name = &manifest.package.name;
        if !identities.insert((name.clone(), manifest.package.version.clone())) {
            invalid.push(diagnostic(
                "duplicate_candidate",
                Some(name),
                name.as_str(),
                format!("duplicate candidate version {}", manifest.package.version),
            ));
        }
        if name == &request.root.package.name
            && manifest.package.version == request.root.package.version
            && manifest != &request.root
        {
            invalid.push(diagnostic(
                "conflicting_root",
                Some(name),
                name.as_str(),
                "catalog candidate contradicts supplied root at the same identity",
            ));
        }
        packages.entry(name.clone()).or_default().push(manifest);
    }
    for manifest in std::iter::once(&request.root).chain(&request.catalog.packages) {
        invalid.extend(validate_manifest(manifest).into_iter().map(from_calculus));
    }
    let mut systems = BTreeMap::<String, Vec<&SystemSelection>>::new();
    let mut identities = BTreeSet::new();
    for selection in &request.catalog.system_packages {
        if !system_name_valid(&selection.name) {
            invalid.push(diagnostic(
                "invalid_system_name",
                None,
                &selection.name,
                "native identities must use the calculus SemVer namespace",
            ));
        }
        if !identities.insert((selection.name.clone(), selection.version.clone())) {
            invalid.push(diagnostic(
                "duplicate_system_candidate",
                None,
                &selection.name,
                format!("duplicate native candidate version {}", selection.version),
            ));
        }
        systems
            .entry(selection.name.clone())
            .or_default()
            .push(selection);
    }
    if !invalid.is_empty() {
        return Err(error(ErrorKind::InvalidInput, 0, invalid));
    }
    for candidates in packages.values_mut() {
        candidates.sort_by(|a, b| b.package.version.cmp(&a.package.version));
    }
    for candidates in systems.values_mut() {
        candidates.sort_by(|a, b| b.version.cmp(&a.version));
    }
    Ok(Inputs { packages, systems })
}

fn unsupported_policy(manifest: &Manifest) -> Option<ResolveDiagnostic> {
    let policy = &manifest.resolver.as_ref()?.policy;
    (!policy.optimize.is_empty()
        || policy.min_repro_attestations.is_some_and(|n| n > 0)
        || policy.block_unvetted_namespaces)
        .then(|| diagnostic(
            "unsupported_policy",
            Some(&manifest.package.name),
            "resolver.policy",
            format!("offline search cannot enforce optimization, namespace vetting, or reproducibility attestation policies for version {}", manifest.package.version),
        ))
}

type Assigned<'a> = BTreeMap<PackageName, &'a Manifest>;
#[derive(Default)]
struct Closure {
    features: BTreeMap<PackageName, BTreeSet<String>>,
    requirements: BTreeMap<PackageName, Vec<VersionReq>>,
    systems: BTreeMap<String, Vec<VersionReq>>,
}

// Recompute after every decision: late features may activate edges on already
// visited packages, including through cycles. The worklist is monotonic.
fn closure(
    request: &ResolveRequest,
    assigned: &Assigned<'_>,
) -> Result<Closure, Vec<ResolveDiagnostic>> {
    let root = &request.root.package.name;
    let mut out = Closure::default();
    out.features.insert(root.clone(), request.features.clone());
    let mut queue = VecDeque::from([root.clone()]);
    let mut processed = BTreeMap::<PackageName, BTreeSet<String>>::new();
    let mut failures = vec![];
    while let Some(name) = queue.pop_front() {
        let Some(manifest) = assigned.get(&name) else {
            continue;
        };
        let features = out.features.entry(name.clone()).or_default();
        let mut pending = features.iter().cloned().collect::<VecDeque<_>>();
        let mut expanded = BTreeSet::new();
        while let Some(feature) = pending.pop_front() {
            if !expanded.insert(feature.clone()) {
                continue;
            }
            if let Some(definition) = manifest.features.get(&feature) {
                pending.extend(definition.features.iter().cloned());
            } else {
                failures.push(diagnostic(
                    "unknown_feature",
                    Some(&name),
                    &feature,
                    "requested feature is not declared by selected version",
                ));
            }
        }
        *features = expanded.clone();
        if processed.get(&name) == Some(&expanded) {
            continue;
        }
        processed.insert(name.clone(), expanded.clone());
        let mut enabled = manifest
            .dependencies
            .iter()
            .filter(|(_, d)| !d.optional)
            .map(|(n, _)| n.clone())
            .collect::<BTreeSet<_>>();
        for feature in expanded.iter().filter_map(|f| manifest.features.get(f)) {
            enabled.extend(feature.deps.iter().cloned());
        }
        for dependency in enabled {
            let edge = &manifest.dependencies[&dependency];
            let features = out.features.entry(dependency.clone()).or_default();
            let before = features.len();
            features.extend(edge.features.iter().cloned());
            if !processed.contains_key(&dependency) || before != features.len() {
                queue.push_back(dependency);
            }
        }
    }
    // Derive constraints exactly once from the final fixed point.
    for (name, features) in &out.features {
        let Some(manifest) = assigned.get(name) else {
            continue;
        };
        let mut enabled = manifest
            .dependencies
            .iter()
            .filter(|(_, d)| !d.optional)
            .map(|(n, _)| n.clone())
            .collect::<BTreeSet<_>>();
        for feature in features.iter().filter_map(|f| manifest.features.get(f)) {
            enabled.extend(feature.deps.iter().cloned());
            for source in &feature.sys_deps {
                if let Some((name, req)) = system_requirement(source) {
                    out.systems.entry(name).or_default().push(req);
                }
            }
        }
        for dependency in enabled {
            out.requirements
                .entry(dependency.clone())
                .or_default()
                .push(manifest.dependencies[&dependency].version.clone());
        }
    }
    if failures.is_empty() {
        Ok(out)
    } else {
        Err(failures)
    }
}

fn solution(
    request: &ResolveRequest,
    assigned: &Assigned<'_>,
    closure: &Closure,
    systems: Vec<SystemSelection>,
) -> Solution {
    Solution {
        schema_version: SCHEMA_VERSION,
        roots: vec![RootRequirement {
            name: request.root.package.name.clone(),
            version: VersionReq::parse(&format!("={}", request.root.package.version))
                .expect("Version formats as a valid exact requirement"),
            features: request.features.clone(),
        }],
        packages: assigned
            .iter()
            .map(|(name, manifest)| Selection {
                manifest: (*manifest).clone(),
                features: closure.features.get(name).cloned().unwrap_or_default(),
            })
            .collect(),
        system_packages: systems,
    }
}

/// Search an entirely supplied local catalog, newest versions first.
///
/// This is bounded depth-first backtracking, not CDCL or policy optimization.
/// Every successful result passes the calculus validator. No code is executed,
/// network contacted, or package provenance/compiler authority inferred.
pub fn resolve(request: &ResolveRequest) -> Result<Solution, ResolveError> {
    let inputs = inputs(request)?;
    if let Some(policy) = unsupported_policy(&request.root) {
        return Err(error(ErrorKind::UnsupportedPolicy, 0, vec![policy]));
    }
    let root = &request.root.package.name;
    let mut pending = vec![BTreeMap::from([(root.clone(), &request.root)])];
    let mut decisions = 0;
    let mut conflicts = BTreeSet::new();
    let mut unsupported = false;
    while let Some(assigned) = pending.pop() {
        if assigned.len() > 1 {
            if decisions == request.max_decisions {
                return Err(error(
                    ErrorKind::BudgetExhausted,
                    decisions,
                    vec![diagnostic(
                        "budget_exhausted",
                        None,
                        "max_decisions",
                        "search budget exhausted; satisfiability is unknown",
                    )],
                ));
            }
            decisions += 1;
        }
        let blocked = assigned
            .values()
            .filter_map(|m| unsupported_policy(m))
            .collect::<Vec<_>>();
        if !blocked.is_empty() {
            unsupported = true;
            conflicts.extend(blocked);
            continue;
        }
        let closure = match closure(request, &assigned) {
            Ok(closure) => closure,
            Err(errors) => {
                conflicts.extend(errors);
                continue;
            }
        };
        let mut branch_conflicts = vec![];
        for (name, requirements) in &closure.requirements {
            if let Some(manifest) = assigned.get(name) {
                for req in requirements {
                    if !req.matches(&manifest.package.version) {
                        branch_conflicts.push(diagnostic(
                            "version_conflict",
                            Some(name),
                            name.as_str(),
                            format!(
                                "selected {} does not satisfy {req}",
                                manifest.package.version
                            ),
                        ));
                    }
                }
            }
        }
        // Missing selections are expected in a partial assignment; every other
        // diagnostic is monotonic and can already rule out this branch.
        let partial = validate_solution(&solution(request, &assigned, &closure, vec![]));
        branch_conflicts.extend(
            partial
                .diagnostics
                .into_iter()
                .filter(|d| {
                    !matches!(
                        d.code,
                        vesper_loom_calculus::Code::MissingDependency
                            | vesper_loom_calculus::Code::MissingSystemPackage
                    )
                })
                .map(from_calculus),
        );
        if !branch_conflicts.is_empty() {
            conflicts.extend(branch_conflicts);
            continue;
        }
        if let Some((name, requirements)) = closure
            .requirements
            .iter()
            .find(|(name, _)| !assigned.contains_key(*name))
        {
            let candidates = inputs
                .packages
                .get(name)
                .into_iter()
                .flatten()
                .filter(|m| {
                    requirements
                        .iter()
                        .all(|req| req.matches(&m.package.version))
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                conflicts.insert(diagnostic(
                    "no_candidate",
                    Some(name),
                    name.as_str(),
                    format!(
                        "no local version satisfies {}",
                        requirements
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(" and ")
                    ),
                ));
            }
            // Reverse insertion preserves newest-first DFS with a LIFO stack.
            for manifest in candidates.into_iter().rev() {
                let mut next = assigned.clone();
                next.insert(name.clone(), manifest);
                pending.push(next);
            }
            continue;
        }
        let mut systems = vec![];
        let mut native_failure = false;
        for (name, requirements) in &closure.systems {
            if let Some(selection) = inputs
                .systems
                .get(name)
                .into_iter()
                .flatten()
                .find(|s| requirements.iter().all(|req| req.matches(&s.version)))
            {
                systems.push((*selection).clone());
            } else {
                native_failure = true;
                conflicts.insert(diagnostic(
                    "no_system_candidate",
                    None,
                    name,
                    format!(
                        "no local native version satisfies {}",
                        requirements
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(" and ")
                    ),
                ));
            }
        }
        if native_failure {
            continue;
        }
        let result = solution(request, &assigned, &closure, systems);
        let validation = validate_solution(&result);
        if validation.graph.is_some()
            && validation.diagnostics.is_empty()
            && validation.unused_selections.is_empty()
        {
            return Ok(result);
        }
        conflicts.extend(validation.diagnostics.into_iter().map(from_calculus));
    }
    Err(error(
        if unsupported {
            ErrorKind::UnsupportedPolicy
        } else {
            ErrorKind::Unsatisfiable
        },
        decisions,
        conflicts.into_iter().collect(),
    ))
}
