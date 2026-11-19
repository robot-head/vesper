//! Deterministic, bounded offline dependency search over supplied manifests.
#![doc = include_str!("../README.md")]

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use vesper_loom_calculus::{Manifest, PackageName, Solution, SystemSelection};

pub mod optimize;

// This file is the API both builds share; `crate::engine` does the work.
// The package vesper-loom-solver (the only build outside the compiler) runs
// resolution, locked validation and optimization in the Vesper package in
// vesper/: its build script sets cfg(vesper_port) and `engine` is `port`.
// The frozen seed copy that the compiler links (vesper-loom-solver-seed)
// includes this file from seed/src/lib.rs, whose `engine` is the Rust
// implementation in seed/src/engine.rs. See "Crates the compiler links" in
// docs/maintenance/porting-a-crate.md.
#[cfg(vesper_port)]
mod port;
#[cfg(vesper_port)]
use port as engine;
// Generated and committed, but kept outside src/ so formatters, which check
// every file under src/, never reflow it (see the porting recipe).
#[cfg(vesper_port)]
#[allow(unsafe_code, clippy::all)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;

/// Which build this is: `"vesper"` for the port-backed package
/// `vesper-loom-solver`, `"rust"` for the seed copy
/// `vesper-loom-solver-seed` that the compiler links.
pub const IMPLEMENTATION: &str = if cfg!(vesper_port) { "vesper" } else { "rust" };

/// Live allocations of the Vesper build (always 0 for the Rust build), for
/// leak tests.
pub fn port_live_allocations() -> i64 {
    crate::engine::live_allocations()
}

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
    LockedMismatch,
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

pub(crate) fn error(
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

/// Validate an existing snapshot against the supplied request without search.
///
/// The snapshot must contain exactly the requested root and its minimal feature,
/// dependency and native-package closure. Package metadata must match the current
/// root/catalog in full; matching names and versions alone are insufficient.
/// Older catalog selections remain valid when they still satisfy the request.
/// `max_decisions` is ignored and every error reports zero decisions.
///
/// This checks offline declaration consistency, not source identity, provenance,
/// signatures, compiler authority or the availability of native installations.
pub fn validate_locked(request: &ResolveRequest, locked: &Solution) -> Result<(), ResolveError> {
    crate::engine::validate_locked(request, locked)
}

/// Search an entirely supplied local catalog, newest versions first.
///
/// This is bounded depth-first backtracking, not CDCL or policy optimization.
/// Every successful result passes the calculus validator. No code is executed,
/// network contacted, or package provenance/compiler authority inferred.
pub fn resolve(request: &ResolveRequest) -> Result<Solution, ResolveError> {
    crate::engine::resolve(request)
}
