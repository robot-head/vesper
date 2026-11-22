//! Anti-slopsquatting, identifier-conflation, namespace age, and trust-cost screening (§10.1–10.4).
//!
//! The screen is the Vesper port (`screen`, `conflation_key` and
//! `damerau_levenshtein` in `vesper/src/lib.ves`); this module holds the
//! types and lends the catalog to it as one text pool.

use serde::{Deserialize, Serialize};
use vesper_loom_calculus::PackageName;

#[cfg(vesper_port)]
use crate::port;
#[cfg(not(vesper_port))]
use crate::reference::slopsquat as engine;

pub const SLOPSQUAT_SCHEMA_VERSION: u32 = 1;
pub const MAX_PACKAGE_NAME_LEN: usize = 256;

/// Catalog metadata evaluated when admitting a dependency into `Loom.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub package: PackageName,
    /// Unix epoch seconds when the publisher namespace was created.
    pub org_created_at: u64,
    /// Caller-supplied maintainer trust-cost score (lower is more trusted).
    pub trust_cost: u32,
    /// Whether the catalog entry carries a verified keyless OIDC/Rekor provenance record.
    pub attested_oidc: bool,
}

/// Configurable anti-slopsquatting and namespace vetting thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarantinePolicy {
    pub schema_version: u32,
    pub max_edit_distance: usize,
    pub min_namespace_age_secs: u64,
    pub max_trust_cost: u32,
    pub require_oidc_provenance: bool,
    /// Mirrors `VESPER_AGENT_MODE=1`: hard-rejects quarantined candidates.
    pub agent_mode: bool,
}

impl Default for QuarantinePolicy {
    fn default() -> Self {
        Self {
            schema_version: SLOPSQUAT_SCHEMA_VERSION,
            max_edit_distance: 2,
            min_namespace_age_secs: 30 * 24 * 60 * 60,
            max_trust_cost: 50,
            require_oidc_provenance: true,
            agent_mode: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineReason {
    MissingFromCatalog,
    OversizedPackageName,
    EditDistanceTyposquat,
    IdentifierConflation,
    StdNamespaceImpersonation,
    ImmatureNamespace,
    ExcessiveTrustCost,
    MissingOidcProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineDecision {
    Admit,
    Quarantined,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarantineFinding {
    pub reason: QuarantineReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similar_to: Option<PackageName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_distance: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarantineReport {
    pub schema_version: u32,
    pub candidate: PackageName,
    pub decision: QuarantineDecision,
    pub findings: Vec<QuarantineFinding>,
}

/// Splits `@org/name` or `std/name` into `(namespace, short_name)`.
pub fn split_package(name: &PackageName) -> (&str, &str) {
    #[cfg(vesper_port)]
    {
        let text = name.as_str();
        let [o0, o1, n0, n1] = port::split(text);
        (&text[o0..o1], &text[n0..n1])
    }
    #[cfg(not(vesper_port))]
    {
        engine::split_package(name)
    }
}

/// Normalizes delimiter swaps (`-`, `_`) and common ASCII lookalikes (`0/o`, `1/l`, `3/e`, `4/a`, `5/s`, `7/t`).
pub fn canonical_conflation_key(text: &str) -> String {
    #[cfg(vesper_port)]
    {
        port::conflation_key(text)
    }
    #[cfg(not(vesper_port))]
    {
        engine::canonical_conflation_key(text)
    }
}

/// Computes optimal string alignment (restricted Damerau-Levenshtein) distance over ASCII package strings
/// using O(min(|a|, |b|)) memory.
pub fn damerau_levenshtein(a: &str, b: &str) -> usize {
    #[cfg(vesper_port)]
    {
        port::damerau_levenshtein(a, b)
    }
    #[cfg(not(vesper_port))]
    {
        engine::damerau_levenshtein(a, b)
    }
}

/// Evaluates whether a package candidate may be added directly, must be quarantined, or must be rejected.
pub fn screen_package_admission(
    candidate: &PackageName,
    catalog: &[CatalogEntry],
    policy: &QuarantinePolicy,
    now_secs: u64,
) -> QuarantineReport {
    #[cfg(vesper_port)]
    {
        port::screen(candidate, catalog, policy, now_secs)
    }
    #[cfg(not(vesper_port))]
    {
        engine::screen_package_admission(candidate, catalog, policy, now_secs)
    }
}
