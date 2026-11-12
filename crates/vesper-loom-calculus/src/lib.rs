//! Declarative Loom manifests and validation of proposed exact-version solutions.
//! This crate neither chooses versions nor executes builds or registry code.
#![doc = include_str!("../README.md")]
use semver::{Version, VersionReq};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

mod upgrade;
pub use upgrade::{
    AuthorityChange, AuthorityScope, UPGRADE_SCHEMA_VERSION, UpgradeDiagnostic, UpgradeReport,
    UpgradeSnapshot, check_upgrade,
};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageName(String);
impl PackageName {
    /// `std/name` is reserved for the standard platform; third parties use `@org/name`.
    pub fn parse(name: impl Into<String>) -> Result<Self, String> {
        let name = name.into();
        let parts = name
            .strip_prefix('@')
            .unwrap_or(&name)
            .split('/')
            .collect::<Vec<_>>();
        let component = |s: &str| {
            !s.is_empty()
                && s.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_')
                })
                && s.as_bytes()[0].is_ascii_lowercase()
        };
        if parts.len() != 2
            || !component(parts[0])
            || !component(parts[1])
            || (!name.starts_with('@') && parts[0] != "std")
        {
            return Err(format!(
                "invalid package name '{name}'; use @org/name or reserved std/name"
            ));
        }
        Ok(Self(name))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for PackageName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: PackageName,
    pub version: Version,
    pub edition: String,
    pub license: Option<String>,
    #[serde(default)]
    pub capability_ceiling: BTreeSet<String>,
    #[serde(default = "yes")]
    pub forbid_unsafe_ffi: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub version: VersionReq,
    #[serde(default)]
    pub effects: BTreeSet<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub features: BTreeSet<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    #[serde(default)]
    pub enables_effects: BTreeSet<String>,
    #[serde(default)]
    pub deps: BTreeSet<PackageName>,
    #[serde(default)]
    pub features: BTreeSet<String>,
    /// Native packages have their own namespace and are never Vesper dependencies.
    #[serde(default)]
    pub sys_deps: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSandbox {
    pub script: Option<String>,
    #[serde(default)]
    pub wasi_read_paths: BTreeSet<String>,
    pub fuel_limit: Option<u64>,
    #[serde(default)]
    pub allow_network: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolverPolicy {
    #[serde(default)]
    pub optimize: Vec<String>,
    pub min_repro_attestations: Option<u32>,
    #[serde(default = "yes")]
    pub block_unvetted_namespaces: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolver {
    pub policy: ResolverPolicy,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    #[serde(default)]
    pub export_lean4_specs: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub package: Package,
    #[serde(default)]
    pub features: BTreeMap<String, Feature>,
    #[serde(default)]
    pub dependencies: BTreeMap<PackageName, Dependency>,
    #[serde(rename = "build-sandbox")]
    pub build_sandbox: Option<BuildSandbox>,
    pub resolver: Option<Resolver>,
    pub verification: Option<Verification>,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    ManifestSyntax,
    InvalidCapability,
    UnsupportedBuildPolicy,
    InvalidFeature,
    UnknownFeature,
    UnknownDependency,
    InvalidSystemRequirement,
    DuplicatePackage,
    DuplicateSystemPackage,
    MissingRoot,
    MissingDependency,
    MissingSystemPackage,
    VersionMismatch,
    CapabilityEscalation,
    EdgeCapabilityEscalation,
    UnsafeFfiForbidden,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: Code,
    pub package: Option<PackageName>,
    pub subject: String,
    pub message: String,
}
fn emit(
    out: &mut Vec<Diagnostic>,
    code: Code,
    package: Option<&PackageName>,
    subject: impl Into<String>,
    message: impl Into<String>,
) {
    out.push(Diagnostic {
        code,
        package: package.cloned(),
        subject: subject.into(),
        message: message.into(),
    });
}
fn capability_valid(s: &str) -> bool {
    if s.is_empty() || !s.as_bytes()[0].is_ascii_uppercase() {
        return false;
    }
    let mut depth = 0usize;
    for c in s.bytes() {
        match c {
            b'[' => depth += 1,
            b']' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            _ if c.is_ascii_alphanumeric()
                || matches!(c, b'_' | b',' | b':' | b'-' | b'/' | b'@' | b'.') => {}
            _ => return false,
        }
    }
    depth == 0 && !s.contains("[]")
}
fn feature_valid(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}
fn check_caps(
    caps: &BTreeSet<String>,
    owner: &PackageName,
    subject: &str,
    out: &mut Vec<Diagnostic>,
) {
    for cap in caps {
        if !capability_valid(cap) {
            emit(
                out,
                Code::InvalidCapability,
                Some(owner),
                subject,
                format!("non-canonical capability '{cap}'"),
            );
        }
    }
}
/// Structural validation also runs on programmatically constructed DTOs.
pub fn validate_manifest(manifest: &Manifest) -> Vec<Diagnostic> {
    let mut out = vec![];
    let owner = &manifest.package.name;
    check_caps(
        &manifest.package.capability_ceiling,
        owner,
        "package.capability_ceiling",
        &mut out,
    );
    if let Some(sandbox) = &manifest.build_sandbox {
        let relative = |path: &str| {
            !path.is_empty()
                && !path.starts_with('/')
                && !path.contains(['\\', ':', '\0'])
                && !path.split('/').any(|part| part == "..")
        };
        if sandbox.allow_network {
            emit(
                &mut out,
                Code::UnsupportedBuildPolicy,
                Some(owner),
                "build-sandbox.allow_network",
                "Loom build components have no network authority",
            );
        }
        for path in sandbox.script.iter().chain(&sandbox.wasi_read_paths) {
            if !relative(path) {
                emit(
                    &mut out,
                    Code::UnsupportedBuildPolicy,
                    Some(owner),
                    "build-sandbox",
                    format!("build input path must be relative and traversal-free: '{path}'"),
                );
            }
        }
    }
    for (name, dep) in &manifest.dependencies {
        check_caps(&dep.effects, owner, name.as_str(), &mut out);
    }
    for (name, feature) in &manifest.features {
        if !feature_valid(name) {
            emit(
                &mut out,
                Code::InvalidFeature,
                Some(owner),
                name,
                "invalid feature name",
            );
        }
        check_caps(&feature.enables_effects, owner, name, &mut out);
        for dep in &feature.deps {
            if !manifest.dependencies.contains_key(dep) {
                emit(
                    &mut out,
                    Code::UnknownDependency,
                    Some(owner),
                    name,
                    format!("feature refers to undeclared dependency {}", dep.as_str()),
                );
            }
        }
        for nested in &feature.features {
            if !manifest.features.contains_key(nested) {
                emit(
                    &mut out,
                    Code::UnknownFeature,
                    Some(owner),
                    name,
                    format!("feature refers to unknown feature '{nested}'"),
                );
            }
        }
        for sys in &feature.sys_deps {
            if parse_system_requirement(sys).is_err() {
                emit(
                    &mut out,
                    Code::InvalidSystemRequirement,
                    Some(owner),
                    name,
                    format!("expected native name followed by a SemVer requirement: '{sys}'"),
                );
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
pub fn parse_manifest(source: &str) -> Result<Manifest, Vec<Diagnostic>> {
    let manifest: Manifest = toml::from_str(source).map_err(|error| {
        vec![Diagnostic {
            code: Code::ManifestSyntax,
            package: None,
            subject: "Loom.toml".into(),
            message: error.to_string(),
        }]
    })?;
    let errors = validate_manifest(&manifest);
    if errors.is_empty() {
        Ok(manifest)
    } else {
        Err(errors)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootRequirement {
    pub name: PackageName,
    pub version: VersionReq,
    #[serde(default)]
    pub features: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub manifest: Manifest,
    #[serde(default)]
    pub features: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemSelection {
    pub name: String,
    pub version: Version,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Solution {
    pub schema_version: u32,
    pub roots: Vec<RootRequirement>,
    pub packages: Vec<Selection>,
    #[serde(default)]
    pub system_packages: Vec<SystemSelection>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub package: PackageName,
    pub requirement: VersionReq,
    pub allowed_capabilities: BTreeSet<String>,
    pub requested_features: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SystemEdge {
    pub name: String,
    pub requirement: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageNode {
    pub version: Version,
    pub features: BTreeSet<String>,
    pub capability_ceiling: BTreeSet<String>,
    /// Conservative union of declared ceilings and active feature effects over
    /// the reachable dependency closure; not an analysis of compiler bodies.
    pub required_capabilities: BTreeSet<String>,
    pub forbid_unsafe_ffi: bool,
    pub dependencies: Vec<DependencyEdge>,
    pub system_dependencies: Vec<SystemEdge>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageGraph {
    pub schema_version: u32,
    pub roots: BTreeSet<PackageName>,
    pub packages: BTreeMap<PackageName, PackageNode>,
    pub system_packages: BTreeMap<String, Version>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validation {
    pub graph: Option<PackageGraph>,
    pub diagnostics: Vec<Diagnostic>,
    pub unused_selections: BTreeSet<PackageName>,
}
fn system_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'+'))
}
fn parse_system_requirement(source: &str) -> Result<(String, VersionReq), ()> {
    let (name, requirement) = source.split_once(char::is_whitespace).ok_or(())?;
    if !system_name_valid(name) {
        return Err(());
    }
    Ok((
        name.into(),
        VersionReq::parse(requirement.trim()).map_err(|_| ())?,
    ))
}
fn expand_features(
    manifest: &Manifest,
    initial: &BTreeSet<String>,
    out: &mut Vec<Diagnostic>,
) -> BTreeSet<String> {
    let mut expanded = BTreeSet::new();
    let mut queue = initial.iter().cloned().collect::<VecDeque<_>>();
    while let Some(name) = queue.pop_front() {
        if !expanded.insert(name.clone()) {
            continue;
        }
        if let Some(feature) = manifest.features.get(&name) {
            queue.extend(feature.features.iter().cloned());
        } else {
            emit(
                out,
                Code::UnknownFeature,
                Some(&manifest.package.name),
                &name,
                "requested feature is not declared",
            );
        }
    }
    expanded
}
/// Validate a candidate graph; this deliberately does not search for versions.
/// Unreachable selections are returned separately and do not grant authority.
///
/// This checks supplied declaration identities, not actual compiler authority.
/// Compiler integration must audit privileged handler installation and FFI, not
/// infer package authority from public outgoing effect rows: local handlers can
/// discharge those rows. Capability arguments are opaque and compared exactly;
/// no package/type alias resolution or kind checking is certified here.
pub fn validate_solution(solution: &Solution) -> Validation {
    let mut out = vec![];
    if solution.schema_version != SCHEMA_VERSION {
        emit(
            &mut out,
            Code::ManifestSyntax,
            None,
            "schema_version",
            "unsupported solution schema version",
        );
    }
    let mut selected = BTreeMap::new();
    let mut selections = solution.packages.iter().collect::<Vec<_>>();
    selections.sort_by_cached_key(|s| {
        (
            s.manifest.package.name.clone(),
            s.manifest.package.version.clone(),
            serde_json::to_string(s).expect("manifest DTOs contain no fallible JSON values"),
        )
    });
    for selection in selections {
        out.extend(validate_manifest(&selection.manifest));
        let name = &selection.manifest.package.name;
        if selected.insert(name.clone(), selection).is_some() {
            emit(
                &mut out,
                Code::DuplicatePackage,
                Some(name),
                name.as_str(),
                "version uniqueness requires one selection per package name",
            );
        }
    }
    let mut systems = BTreeMap::new();
    let mut native_selections = solution.system_packages.iter().collect::<Vec<_>>();
    native_selections.sort_by_key(|s| (s.name.clone(), s.version.clone()));
    for selection in native_selections {
        if !system_name_valid(&selection.name) {
            emit(
                &mut out,
                Code::InvalidSystemRequirement,
                None,
                &selection.name,
                "invalid native package name",
            );
        }
        if systems
            .insert(selection.name.clone(), selection.version.clone())
            .is_some()
        {
            emit(
                &mut out,
                Code::DuplicateSystemPackage,
                None,
                &selection.name,
                "native version uniqueness requires one selection per name",
            );
        }
    }
    let mut features = BTreeMap::<PackageName, BTreeSet<String>>::new();
    let mut queue = VecDeque::new();
    let mut roots = BTreeSet::new();
    for root in &solution.roots {
        roots.insert(root.name.clone());
        if let Some(selection) = selected.get(&root.name) {
            if !root.version.matches(&selection.manifest.package.version) {
                emit(
                    &mut out,
                    Code::VersionMismatch,
                    Some(&root.name),
                    "root",
                    format!(
                        "{} does not satisfy {}",
                        selection.manifest.package.version, root.version
                    ),
                );
            }
            features
                .entry(root.name.clone())
                .or_default()
                .extend(root.features.iter().cloned());
            queue.push_back(root.name.clone());
        } else {
            emit(
                &mut out,
                Code::MissingRoot,
                Some(&root.name),
                "root",
                "root inclusion requires a selected package",
            );
        }
    }
    let mut reachable = BTreeSet::new();
    let mut nodes = BTreeMap::<PackageName, PackageNode>::new();
    while let Some(name) = queue.pop_front() {
        let Some(selection) = selected.get(&name) else {
            continue;
        };
        let manifest = &selection.manifest;
        let first = reachable.insert(name.clone());
        let initial = features.entry(name.clone()).or_default();
        initial.extend(selection.features.iter().cloned());
        let expanded = expand_features(manifest, initial, &mut out);
        if !first && nodes.get(&name).is_some_and(|n| n.features == expanded) {
            continue;
        }
        *initial = expanded.clone();
        let mut enabled = manifest
            .dependencies
            .iter()
            .filter(|(_, d)| !d.optional)
            .map(|(n, _)| n.clone())
            .collect::<BTreeSet<_>>();
        let mut required = manifest.package.capability_ceiling.clone();
        let mut sys = BTreeSet::new();
        for feature in expanded.iter().filter_map(|f| manifest.features.get(f)) {
            enabled.extend(feature.deps.iter().cloned());
            required.extend(feature.enables_effects.iter().cloned());
            for source in &feature.sys_deps {
                if let Ok((sys_name, requirement)) = parse_system_requirement(source) {
                    sys.insert(SystemEdge {
                        name: sys_name,
                        requirement: requirement.to_string(),
                    });
                }
            }
        }
        let mut edges = vec![];
        for dependency in enabled {
            let Some(edge) = manifest.dependencies.get(&dependency) else {
                continue;
            };
            edges.push(DependencyEdge {
                package: dependency.clone(),
                requirement: edge.version.clone(),
                allowed_capabilities: edge.effects.clone(),
                requested_features: edge.features.clone(),
            });
            if let Some(target) = selected.get(&dependency) {
                if !edge.version.matches(&target.manifest.package.version) {
                    emit(
                        &mut out,
                        Code::VersionMismatch,
                        Some(&name),
                        dependency.as_str(),
                        format!(
                            "{} does not satisfy {}",
                            target.manifest.package.version, edge.version
                        ),
                    );
                }
                let enabled_features = features.entry(dependency.clone()).or_default();
                let before = enabled_features.len();
                enabled_features.extend(edge.features.iter().cloned());
                if !reachable.contains(&dependency) || before != enabled_features.len() {
                    queue.push_back(dependency);
                }
            } else {
                emit(
                    &mut out,
                    Code::MissingDependency,
                    Some(&name),
                    dependency.as_str(),
                    "dependency closure requires a selected package",
                );
            }
        }
        for edge in &sys {
            if let Some(version) = systems.get(&edge.name) {
                if !VersionReq::parse(&edge.requirement)
                    .unwrap()
                    .matches(version)
                {
                    emit(
                        &mut out,
                        Code::VersionMismatch,
                        Some(&name),
                        &edge.name,
                        format!(
                            "native version {version} does not satisfy {}",
                            edge.requirement
                        ),
                    );
                }
            } else {
                emit(
                    &mut out,
                    Code::MissingSystemPackage,
                    Some(&name),
                    &edge.name,
                    "active feature requires a native package selection",
                );
            }
        }
        nodes.insert(
            name,
            PackageNode {
                version: manifest.package.version.clone(),
                features: expanded,
                capability_ceiling: manifest.package.capability_ceiling.clone(),
                required_capabilities: required,
                forbid_unsafe_ffi: manifest.package.forbid_unsafe_ffi,
                dependencies: edges,
                system_dependencies: sys.into_iter().collect(),
            },
        );
    }
    // Propagate authority backwards over active edges. Each new capability
    // moves monotonically through the finite graph, including dependency cycles.
    let mut parents = BTreeMap::<PackageName, BTreeSet<PackageName>>::new();
    for (name, node) in &nodes {
        for edge in &node.dependencies {
            if nodes.contains_key(&edge.package) {
                parents
                    .entry(edge.package.clone())
                    .or_default()
                    .insert(name.clone());
            }
        }
    }
    let mut pending = nodes.keys().cloned().collect::<VecDeque<_>>();
    let mut queued = nodes.keys().cloned().collect::<BTreeSet<_>>();
    while let Some(child) = pending.pop_front() {
        queued.remove(&child);
        let required = nodes[&child].required_capabilities.clone();
        for parent in parents.get(&child).into_iter().flatten() {
            let caps = &mut nodes.get_mut(parent).unwrap().required_capabilities;
            let before = caps.len();
            caps.extend(required.iter().cloned());
            if caps.len() != before && queued.insert(parent.clone()) {
                pending.push_back(parent.clone());
            }
        }
    }
    for (name, node) in &nodes {
        let excess = node
            .required_capabilities
            .difference(&node.capability_ceiling)
            .cloned()
            .collect::<Vec<_>>();
        if !excess.is_empty() {
            emit(
                &mut out,
                Code::CapabilityEscalation,
                Some(name),
                "package.capability_ceiling",
                format!(
                    "required capabilities exceed package ceiling: {}",
                    excess.join(", ")
                ),
            );
        }
        if node.forbid_unsafe_ffi
            && node
                .required_capabilities
                .iter()
                .any(|c| matches!(c.split('[').next(), Some("UnsafeFfi" | "Ffi")))
        {
            emit(
                &mut out,
                Code::UnsafeFfiForbidden,
                Some(name),
                "package.forbid_unsafe_ffi",
                "reachable package requires foreign authority",
            );
        }
        for edge in &node.dependencies {
            if let Some(target) = nodes.get(&edge.package) {
                let excess = target
                    .required_capabilities
                    .difference(&edge.allowed_capabilities)
                    .cloned()
                    .collect::<Vec<_>>();
                if !excess.is_empty() {
                    emit(
                        &mut out,
                        Code::EdgeCapabilityEscalation,
                        Some(name),
                        edge.package.as_str(),
                        format!(
                            "dependency exceeds edge capability ceiling: {}",
                            excess.join(", ")
                        ),
                    );
                }
            }
        }
    }
    out.sort();
    out.dedup();
    let unused = selected
        .keys()
        .filter(|n| !reachable.contains(*n))
        .cloned()
        .collect();
    let used_systems = nodes
        .values()
        .flat_map(|n| n.system_dependencies.iter().map(|e| e.name.clone()))
        .collect::<BTreeSet<_>>();
    Validation {
        graph: out.is_empty().then(|| PackageGraph {
            schema_version: SCHEMA_VERSION,
            roots,
            packages: nodes,
            system_packages: systems
                .into_iter()
                .filter(|(n, _)| used_systems.contains(n))
                .collect(),
        }),
        diagnostics: out,
        unused_selections: unused,
    }
}
impl PackageGraph {
    /// Canonical DTO JSON uses ordered maps/sets. This is not an interface MIF
    /// or a cryptographic derivation/provenance certificate.
    pub fn canonical_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}
