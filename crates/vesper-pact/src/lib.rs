//! Vesper Pact: canonical trait goals and tabled trait resolution for the
//! first Pact fragment (design §16.2–16.3).
//!
//! The crate is standalone. The compiler hands over a versioned
//! [`ProgramDto`] of numerically identified declarations and asks
//! [`QueryDto`]s; no source names or surface AST are involved.
//!
//! Supported fragment:
//!
//! * first-order goals `Implements`, `ProjectionEq` and `ModeBound` over
//!   nominal types, rigid placeholders, inference variables and associated
//!   type projections;
//! * modal arguments and modal bounds on the lattice `copy ⊑ affine ⊑ linear`,
//!   effect-row arguments with open tails and lexical capability labels
//!   (each effect key at most once per row);
//! * lazy (eager-at-registration) projection normalization with rigid
//!   projections under the environment, associated-type equality
//!   assumptions and alias bounds;
//! * canonical impls with orphan/overlap checking, and type-branded named
//!   models selected explicitly or by lexical `use model` scopes;
//! * tabled answers keyed by canonical goals, with coinductive cycles (only
//!   through coinductive traits) distinguished from inductive cycles.
//!
//! Outside the fragment (rejected or reported as
//! [`Outcome::Unsupported`]): higher-ranked goals, supertrait elaboration,
//! negative reasoning, specialization, duplicate effect labels, capability
//! inference variables and projections in impl heads.

// The Rust implementation stays compiled under `vesper-port` (types,
// rendering, folds, and the stand-alone `check_orphan`, `conformance_env`
// and `validate_query`); the engine paths it no longer calls are unused there.
#![cfg_attr(vesper_port, allow(dead_code, unused_imports))]

pub mod canon;
pub mod coherence;
pub mod derivation;
pub mod diag;
pub mod ids;
mod infer;
pub mod mode;
pub mod program;
pub mod solver;
pub mod term;

pub use canon::{CanonicalGoal, canonicalize_query};
pub use coherence::{check_coherence, check_orphan, conformance_env};
pub use derivation::{ConformanceSource, Derivation, DerivationError, Step, check_derivation};
pub use diag::{DiagCode, Diagnostic, Subject};
pub use ids::*;
pub use program::*;
pub use solver::*;
pub use term::*;

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[cfg(vesper_port)]
#[allow(unsafe_code)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;
#[cfg(vesper_port)]
mod port;
#[cfg(vesper_port)]
pub use port::live_allocations;

/// Which build this is: `"vesper"` for the port-backed build of
/// `vesper-pact` (feature `vesper-port`), `"rust"` otherwise and for the
/// seed copy `vesper-pact-seed` that the compiler links.
pub const IMPLEMENTATION: &str = if cfg!(vesper_port) { "vesper" } else { "rust" };
