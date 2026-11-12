//! Checked-source-to-Pact adapter with a coherence-gated evidence session.
//!
//! The bridge turns successfully parsed and checked Vesper source packages
//! into a Pact [`ProgramDto`](vesper_pact::ProgramDto):
//!
//! * [`CheckedPackage::check`] runs the real parser and checker and rejects
//!   any diagnostic;
//! * definitions get deterministic package-qualified [`DefPath`] identities
//!   and dense, sorted Pact id tables with reverse maps ([`IdTables`]);
//! * constructor mode schemes are derived from the checker's `World::mode`
//!   and verified against it on every well-formed argument-mode combination
//!   (unrepresentable cases are `Unsupported`, never approximated);
//! * trait, impl and model declarations enter through the versioned
//!   [`BindingsDto`], either supplied directly or lowered from source
//!   declarations ([`CheckedPackage::source_bindings`]); every type they
//!   mention must resolve to a checked source declaration, a builtin or a
//!   parameter;
//! * [`SessionBuilder::build`] validates the program and runs
//!   `check_coherence`; only the resulting opaque [`CoherentSession`] answers
//!   queries, and any incoherence blocks it.
//!
//! Queries preserve all five solver outcomes. A `Proven` answer becomes
//! [`Evidence`] only after its derivation re-checks against the session;
//! [`CoherentSession::consume`] re-checks again and rejects evidence from a
//! different declaration [`Revision`].
//!
//! Everything except [`CheckedPackage`] and the [`SessionBuilder`] wrapper
//! lives in the root-free `vesper-pact-binding` crate, which the compiler
//! uses directly for source trait elaboration. The bridge depends on the
//! compiler; the compiler never depends on the bridge.

pub mod source;

pub use source::CheckedPackage;
pub use vesper_pact_binding::binding::*;
pub use vesper_pact_binding::{
    BUILTIN_PACKAGE, BridgeDiagnostic, BridgeError, BridgeOutcome, Certificate, CoherentSession,
    ConformanceEntry, Consumed, DefKind, DefPath, EffectEntry, Engine, ErrorKind, Evidence,
    IdTables, LoweredProgram, MAX_TUPLE_ARITY, PackageIdentity, Revision, Selection, SourceParam,
    SourceSpan, TraitEntry, TranslatedQuery, TypeEntry, binding, error, identity, modes, session,
    tables,
};

use std::sync::Arc;
use vesper_pact::SolverConfig;

/// Collects checked packages and their declaration bindings. Wraps the
/// root-free builder with the checker prelude.
#[derive(Clone, Debug)]
pub struct SessionBuilder(vesper_pact_binding::SessionBuilder);

impl Default for SessionBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionBuilder {
    pub fn new() -> Self {
        SessionBuilder(vesper_pact_binding::SessionBuilder::new(Arc::new(
            vesper::pact_frontend::prelude_world(),
        )))
    }

    pub fn package(self, package: CheckedPackage, bindings: BindingsDto) -> Self {
        SessionBuilder(self.0.package(package.source_package().clone(), bindings))
    }

    /// Solver budgets. Raising `max_depth` also needs a larger thread stack
    /// (see the Pact report).
    pub fn solver_config(self, config: SolverConfig) -> Self {
        SessionBuilder(self.0.solver_config(config))
    }

    /// Lowers sources and bindings to Pact without validation or coherence.
    pub fn lower(&self) -> Result<LoweredProgram, BridgeError> {
        self.0.lower()
    }

    /// Lowers, validates (`Program::new`) and runs `check_coherence`. Any
    /// diagnostic blocks the gate.
    pub fn build(&self) -> Result<CoherentSession, BridgeError> {
        self.0.build()
    }
}
