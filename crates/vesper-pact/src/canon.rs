//! Canonical goals: inference variables renumbered `?0, ?1, ...` in order of
//! first occurrence so that alpha-equivalent queries share one table entry.

use crate::program::Env;
use crate::term::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanonicalGoal {
    /// Kinds of the canonical existential variables `?0..?n`.
    pub binders: Vec<ParamKind>,
    pub goal: Goal,
}

/// The table key a top-level query maps to: its placeholder-canonical
/// environment and canonical goal. Alpha-equivalent queries (renamed
/// inference variables or placeholders, unused placeholders) share a key.
pub fn canonicalize_query(
    query: &crate::program::QueryDto,
) -> Result<(Env, CanonicalGoal), DuplicateLabel> {
    #[cfg(vesper_port)]
    return crate::port::canonicalize_query(query);
    #[cfg(not(vesper_port))]
    crate::engine::canon::canonicalize_query(query)
}
