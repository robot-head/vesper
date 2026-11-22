//! Declarative Loom manifests and validation of proposed exact-version solutions.
//! This crate neither chooses versions nor executes builds or registry code.
#![doc = include_str!("../README.md")]
use semver::{Version, VersionReq};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};

// The port-backed build (package vesper-loom-calculus) runs the calculus in
// the Vesper package in vesper/; the seed copy (vesper-loom-calculus-seed)
// that the compiler links keeps this Rust code. See "Crates the compiler
// links" in docs/maintenance/porting-a-crate.md.
#[cfg(vesper_port)]
mod port;
// Generated and committed, but kept outside src/ so formatters, which check
// every file under src/, never reflow it (see the porting recipe).
#[cfg(vesper_port)]
#[allow(unsafe_code, clippy::all)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;

/// The Vesper TOML reader's canonical dump, for the differential tests.
#[cfg(vesper_port)]
#[doc(hidden)]
pub use port::{manifest_toml, read_toml, semver_text, toml_accepts};

/// Which build this is: `"vesper"` for the port-backed package
/// `vesper-loom-calculus`, `"rust"` for the seed copy
/// `vesper-loom-calculus-seed` that the compiler links.
pub const IMPLEMENTATION: &str = if cfg!(vesper_port) { "vesper" } else { "rust" };

/// Live allocations of the Vesper build (always 0 for the Rust build), for
/// leak tests.
pub fn port_live_allocations() -> i64 {
    #[cfg(vesper_port)]
    {
        port::live_allocations()
    }
    #[cfg(not(vesper_port))]
    {
        0
    }
}

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
        #[cfg(vesper_port)]
        let valid = port::name_valid(&name);
        #[cfg(not(vesper_port))]
        let valid = crate::engine::package_name_valid(&name);
        if !valid {
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The package's `.vsys` libraries (manuscript §16.3), as paths relative
    /// to this manifest. Every package that depends on it can call them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vsys: Vec<String>,
}
/// As derived, except that an empty `vsys` is left out, so the debug text of
/// a manifest without `.vsys` imports (which differential digests hash) is
/// what it was before the field existed.
impl std::fmt::Debug for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("Package");
        s.field("name", &self.name)
            .field("version", &self.version)
            .field("edition", &self.edition)
            .field("license", &self.license)
            .field("capability_ceiling", &self.capability_ceiling)
            .field("forbid_unsafe_ffi", &self.forbid_unsafe_ffi);
        if !self.vsys.is_empty() {
            s.field("vsys", &self.vsys);
        }
        s.finish()
    }
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
    /// Local path dependency: the directory holding the dependency's
    /// `Loom.toml`, relative to this manifest (manuscript ch 11.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
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
/// Structural validation also runs on programmatically constructed DTOs.
pub fn validate_manifest(manifest: &Manifest) -> Vec<Diagnostic> {
    #[cfg(vesper_port)]
    {
        port::validate_manifest(manifest)
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::validate_manifest(manifest)
    }
}
pub fn parse_manifest(source: &str) -> Result<Manifest, Vec<Diagnostic>> {
    let syntax = |message: String| {
        vec![Diagnostic {
            code: Code::ManifestSyntax,
            package: None,
            subject: "Loom.toml".into(),
            message,
        }]
    };
    // The port reads the TOML and decodes the manifest itself, with
    // `toml::from_str`'s error text, and hands back its JSON.
    #[cfg(vesper_port)]
    let manifest: Manifest = {
        let json = port::manifest_toml(source);
        match json.strip_prefix('!') {
            Some(message) => return Err(syntax(message.to_owned())),
            None => serde_json::from_str(&json).expect("the port writes a manifest's JSON"),
        }
    };
    #[cfg(not(vesper_port))]
    let manifest: Manifest = toml::from_str(source).map_err(|error| syntax(error.to_string()))?;
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
/// Validate a candidate graph; this deliberately does not search for versions.
/// Unreachable selections are returned separately and do not grant authority.
///
/// This checks supplied declaration identities, not actual compiler authority.
/// Compiler integration must audit privileged handler installation and FFI, not
/// infer package authority from public outgoing effect rows: local handlers can
/// discharge those rows. Capability arguments are opaque and compared exactly;
/// no package/type alias resolution or kind checking is certified here.
pub fn validate_solution(solution: &Solution) -> Validation {
    #[cfg(vesper_port)]
    {
        port::validate_solution(solution)
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::validate_solution(solution)
    }
}
impl PackageGraph {
    /// Canonical DTO JSON uses ordered maps/sets. This is not an interface MIF
    /// or a cryptographic derivation/provenance certificate.
    pub fn canonical_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}
