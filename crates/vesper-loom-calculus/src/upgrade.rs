//! A conservative upgrade guard over declared snapshot metadata.
//!
//! This compares already validated graphs, not actual compiler-body authority,
//! package provenance, source identity, ABI compatibility or trust policy.
use crate::{PackageName, Solution};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const UPGRADE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeSnapshot {
    Baseline,
    Candidate,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeDiagnostic {
    pub code: String,
    pub package: Option<PackageName>,
    pub dependency: Option<PackageName>,
    pub snapshot: Option<UpgradeSnapshot>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityScope {
    RequiredCapabilities,
    CapabilityCeiling,
    DependencyCeiling,
    UnsafeFfiPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityChange {
    pub scope: AuthorityScope,
    pub package: PackageName,
    pub dependency: Option<PackageName>,
    pub before: BTreeSet<String>,
    pub after: BTreeSet<String>,
    pub added: BTreeSet<String>,
    pub removed: BTreeSet<String>,
    /// Only present for UnsafeFfiPolicy changes.
    pub before_forbid_unsafe_ffi: Option<bool>,
    pub after_forbid_unsafe_ffi: Option<bool>,
    pub unsafe_ffi_relaxed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeReport {
    pub schema_version: u32,
    /// Permits these declared authority changes only, not the upgrade itself.
    pub allowed: bool,
    pub diagnostics: Vec<UpgradeDiagnostic>,
    pub changes: Vec<AuthorityChange>,
}

/// Reject additions to declared package/edge authority across the same root identities.
///
/// Both complete snapshots must validate first. Unreachable selections do not
/// contribute authority, though structural validation still applies to them.
/// Native-version changes alone are outside this guard. No compiler-body audit,
/// signature check, provenance/trust validation or SemVer certification occurs.
pub fn check_upgrade(baseline: &Solution, candidate: &Solution) -> UpgradeReport {
    #[cfg(vesper_port)]
    {
        crate::port::check_upgrade(baseline, candidate)
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::upgrade::check_upgrade(baseline, candidate)
    }
}
