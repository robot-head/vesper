//! Stratified coherence (design §16.3).
//!
//! * Tier 1 canonical impls must satisfy the covered-first orphan rule and
//!   must not overlap pairwise (Global Model Uniqueness).
//! * Tier 2 named models are exempt from both checks: they are never found
//!   by canonical search, so they cannot collide.
//! * Every impl and model must be well formed: associated values satisfy
//!   their declared modal and trait bounds under the declaration's
//!   where-clauses.

use crate::diag::Diagnostic;
use crate::program::{ConformanceDecl, Env, Program};

/// Runs orphan, overlap and well-formedness checks. The result is sorted.
pub fn check_coherence(program: &Program) -> Vec<Diagnostic> {
    #[cfg(vesper_port)]
    return crate::port::check_coherence(program);
    #[cfg(not(vesper_port))]
    crate::engine::coherence::check_coherence(program)
}

/// Covered-first orphan rule. A canonical impl declared in package `P` is
/// legal when the trait is defined in `P`, or when, scanning the ordered type
/// arguments `(T_0 = Self, T_1, ..., T_n)`, a type constructor defined in `P`
/// is reached before any uncovered impl parameter. Parameters nested inside a
/// foreign constructor are covered. Non-type arguments are skipped.
pub fn check_orphan(program: &Program, decl: &ConformanceDecl) -> Result<(), String> {
    #[cfg(vesper_port)]
    return crate::port::check_orphan(program, decl);
    #[cfg(not(vesper_port))]
    crate::engine::coherence::check_orphan(program, decl)
}

/// The environment under which an impl/model body is checked: parameters as
/// placeholders, where-clauses as assumptions. `ModeBound(^i, c)` clauses
/// become placeholder mode bounds; other mode clauses are not assumed.
pub fn conformance_env(decl: &ConformanceDecl) -> Env {
    #[cfg(vesper_port)]
    return crate::port::conformance_env(decl);
    #[cfg(not(vesper_port))]
    crate::engine::coherence::conformance_env(decl)
}
