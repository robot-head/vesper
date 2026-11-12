//! Root-free binding core between checked Vesper source and Pact.
//!
//! This crate holds everything of the Pact bridge that does not need the
//! compiler facade: versioned binding DTOs, package-qualified identities,
//! dense id tables, mode-scheme derivation, lowering to a Pact
//! [`ProgramDto`](vesper_pact::ProgramDto), the coherence gate and
//! revision-bound evidence sessions. Checked packages enter as
//! [`SourcePackage`]s over a caller-implemented [`SourceWorld`], so the
//! compiler itself can depend on this crate (for source trait elaboration)
//! while `vesper-pact-bridge`, which runs the compiler, stays above it.
//!
//! * definitions get deterministic package-qualified [`DefPath`] identities
//!   and dense, sorted Pact id tables with reverse maps ([`IdTables`]);
//! * constructor mode schemes are derived from [`SourceWorld::mode`] and
//!   verified against it on every well-formed argument-mode combination
//!   (unrepresentable cases are `Unsupported`, never approximated);
//! * trait, impl and model declarations enter through the versioned
//!   [`BindingsDto`]; every type they mention must resolve to a checked
//!   source declaration, a builtin or a parameter;
//! * [`SessionBuilder::build`] validates the program and runs
//!   `check_coherence`; only the resulting opaque [`CoherentSession`] answers
//!   queries, and any incoherence blocks it.
//!
//! Queries preserve all five solver outcomes. A `Proven` answer becomes
//! [`Evidence`] only after its derivation re-checks against the session;
//! [`CoherentSession::consume`] re-checks again and rejects evidence from a
//! different declaration [`Revision`].

pub mod binding;
pub mod error;
pub mod identity;
mod lower;
pub mod modes;
pub mod session;
pub mod source;
pub mod tables;

pub use binding::*;
pub use error::{BridgeDiagnostic, BridgeError, ErrorKind, SourceSpan};
pub use identity::{BUILTIN_PACKAGE, DefKind, DefPath, PackageIdentity};
pub use lower::MAX_TUPLE_ARITY;
pub use session::{
    BridgeOutcome, Certificate, CoherentSession, Consumed, Engine, Evidence, LoweredProgram,
    Revision, Selection, SessionBuilder, TranslatedQuery,
};
pub use source::{SourcePackage, SourceWorld};
pub use tables::{ConformanceEntry, EffectEntry, IdTables, SourceParam, TraitEntry, TypeEntry};
