//! Bounded exhaustive execution of projected endpoints.
//!
//! Runs all projected roles of a choreography together under asynchronous
//! semantics (one FIFO queue per ordered pair of roles; sends never block)
//! and explores every interleaving and every internal choice up to a state
//! budget. It reports a reachable deadlock (some role waits forever), a
//! reception whose queue head carries a label the role does not accept, or
//! messages left in a queue when every role has ended (orphans).
//!
//! This is executable evidence for a particular choreography under the
//! stated bounds, not a proof of the projection theorem.

use crate::chor::Choreography;
use crate::project::{LocalType, ProjectionError};
use std::fmt;

#[derive(Clone, Copy, Debug)]
pub struct ExploreLimits {
    /// Maximum distinct global states visited.
    pub max_states: usize,
    /// Maximum messages queued on one directed link; deeper sends are not
    /// explored and the run is reported as bounded.
    pub max_queue: usize,
}

impl Default for ExploreLimits {
    fn default() -> Self {
        Self {
            max_states: 200_000,
            max_queue: 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExploreReport {
    pub states: usize,
    /// Number of terminal states where every role reached `End`.
    pub completed: usize,
    /// Whether some send was cut off by `max_queue`.
    pub queue_bound_hit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExploreError {
    Projection(ProjectionError),
    /// No role can move, yet not all have ended. `waiting` lists each blocked
    /// role and the first action it is waiting for.
    Deadlock {
        waiting: Vec<(String, String)>,
    },
    /// `role` expected one of `expected` from `peer` but the queue head was
    /// `found`.
    UnexpectedMessage {
        role: String,
        peer: String,
        expected: Vec<String>,
        found: String,
    },
    /// All roles ended with undelivered messages.
    Orphans {
        messages: Vec<(String, String, String)>,
    },
    StateBudgetExceeded {
        states: usize,
    },
}

impl fmt::Display for ExploreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ExploreError {}

/// Explore every reachable state of the projected system.
///
/// Projection and the explorer are the Vesper library's
/// (`vesper/src/explore.ves`, port wave P3b lane 6, #1068).
pub fn explore(chor: &Choreography, limits: ExploreLimits) -> Result<ExploreReport, ExploreError> {
    crate::port::explore(chor, None, limits)
}

/// Explore an explicit set of local types (one per role, in declaration
/// order). Used to show that hand-written, non-projected endpoints can
/// deadlock.
pub fn explore_locals(
    chor: &Choreography,
    locals: Vec<LocalType>,
    limits: ExploreLimits,
) -> Result<ExploreReport, ExploreError> {
    crate::port::explore(chor, Some(&locals), limits)
}
