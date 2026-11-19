//! Multi-objective Pareto/lexicographic dependency optimization (§10.2–§10.4).

use crate::{ResolveError, ResolveRequest};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use vesper_loom_calculus::{PackageName, Solution};

pub const OPTIMIZE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizeObjective {
    ZeroCve,
    MinCapabilitySurface,
    MinDupMajors,
    MinStaleness,
    MinTrustCost,
}

impl OptimizeObjective {
    pub fn parse(raw: &str) -> Result<Self, String> {
        crate::engine::objective(raw)
            .ok_or_else(|| format!("unknown optimization objective '{raw}'"))
    }
}

/// Caller-supplied advisory, staleness, trust, and attestation metadata for a catalog candidate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateMetadata {
    #[serde(default)]
    pub cve_count: u64,
    #[serde(default)]
    pub staleness_secs: u64,
    #[serde(default)]
    pub trust_cost: u64,
    #[serde(default)]
    pub repro_attestations: u32,
    #[serde(default)]
    pub vetted_namespace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizeRequest {
    pub resolve: ResolveRequest,
    pub objectives: Vec<OptimizeObjective>,
    pub metadata: BTreeMap<(PackageName, Version), CandidateMetadata>,
    pub max_solutions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolutionMetrics {
    pub total_cves: u64,
    pub capability_surface: u64,
    pub duplicate_majors: u64,
    pub total_staleness_secs: u64,
    pub total_trust_cost: u64,
    #[serde(default)]
    pub unknown_metadata_candidates: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizedResolution {
    pub schema_version: u32,
    pub solution: Solution,
    pub metrics: SolutionMetrics,
    pub evaluated_solutions: usize,
    pub objective_order: Vec<OptimizeObjective>,
}

pub fn compute_metrics(
    sol: &Solution,
    root: &PackageName,
    metadata: &BTreeMap<(PackageName, Version), CandidateMetadata>,
) -> SolutionMetrics {
    crate::engine::compute_metrics(sol, root, metadata)
}

/// Searches valid dependency graphs and returns the lexicographically optimal solution.
pub fn resolve_optimized(request: &OptimizeRequest) -> Result<OptimizedResolution, ResolveError> {
    crate::engine::resolve_optimized(request)
}
