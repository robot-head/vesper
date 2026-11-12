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
//! * trait, impl and model declarations, which Vesper source cannot express
//!   yet, enter through the versioned [`BindingsDto`]; every type they mention
//!   must resolve to a checked source declaration, a builtin or a parameter;
//! * [`SessionBuilder::build`] validates the program and runs
//!   `check_coherence`; only the resulting opaque [`CoherentSession`] answers
//!   queries, and any incoherence blocks it.
//!
//! Queries preserve all five solver outcomes. A `Proven` answer becomes
//! [`Evidence`] only after its derivation re-checks against the session;
//! [`CoherentSession::consume`] re-checks again and rejects evidence from a
//! different declaration [`Revision`].
//!
//! Not implemented here: trait/model surface syntax, dictionary codegen and
//! full source trait checking. Those belong to the source traits/models lane.

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
pub use source::CheckedPackage;
pub use tables::{ConformanceEntry, EffectEntry, IdTables, SourceParam, TraitEntry, TypeEntry};
