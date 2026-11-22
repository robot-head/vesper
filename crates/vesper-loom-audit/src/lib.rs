//! Bounded offline audits of declared Loom dependency graphs.
#![doc = include_str!("../README.md")]

pub(crate) mod input;

// The port-backed build (package vesper-loom-audit) runs catalog and policy
// validation, the license expressions, the preflight, the dependency paths
// and the findings in the Vesper package in vesper/; the seed copy
// (vesper-loom-audit-seed) that the compiler links keeps this Rust code. See
// "Crates the compiler links" in docs/maintenance/porting-a-crate.md.
#[cfg(vesper_port)]
mod port;
// Generated and committed, but kept outside src/ so formatters, which check
// every file under src/, never reflow it (see the porting recipe).
#[cfg(vesper_port)]
#[allow(unsafe_code, clippy::all)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;

use semver::Version;
use serde::{Deserialize, Serialize};
use std::fmt;
use vesper_loom_calculus::{Diagnostic, PackageName, Solution};

/// Which build this is: `"vesper"` for the port-backed package
/// `vesper-loom-audit`, `"rust"` for the seed copy `vesper-loom-audit-seed`
/// that the compiler links.
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
    pub(crate) fn validate(&self) -> Result<(), Error> {
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
        #[cfg(vesper_port)]
        {
            port::range_contains(self, version)
        }
        #[cfg(not(vesper_port))]
        {
            crate::engine::range_contains(self, version)
        }
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

#[cfg(vesper_port)]
fn validate_catalog(catalog: &Catalog, limits: &Limits) -> Result<(), Error> {
    port::validate_catalog(catalog, limits)
}
#[cfg(vesper_port)]
fn validate_policy(policy: &Policy, limits: &Limits) -> Result<(), Error> {
    port::validate_policy(policy, limits)
}
#[cfg(not(vesper_port))]
fn validate_catalog(catalog: &Catalog, limits: &Limits) -> Result<(), Error> {
    crate::engine::validate_catalog(catalog, limits)
}
#[cfg(not(vesper_port))]
fn validate_policy(policy: &Policy, limits: &Limits) -> Result<(), Error> {
    crate::engine::validate_policy(policy, limits).map(|_| ())
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

/// Audits only the graph validated from this exact Solution. Caller-created
/// PackageGraph DTOs cannot bypass calculus validation by claiming validity.
pub fn audit(
    solution: &Solution,
    catalog: &Catalog,
    policy: &Policy,
    limits: &Limits,
) -> Result<AuditReport, Error> {
    #[cfg(vesper_port)]
    {
        port::audit(solution, catalog, policy, limits)
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::audit(solution, catalog, policy, limits)
    }
}

// A content context is an input binding, never feed/source authenticity.
pub(crate) fn digest<T: Serialize>(domain: &'static str, value: &T) -> Result<String, Error> {
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
