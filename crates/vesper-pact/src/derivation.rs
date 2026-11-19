//! Proof trees produced by the solver and an independent structural checker.
//!
//! A derivation is the elaboration evidence the compiler needs (which impl or
//! model was selected, with which arguments) and a certificate that can be
//! re-checked without search. [`check_derivation`] re-validates each step
//! against the declarations and environment; it does not trust the solver.

use crate::ids::*;
use crate::program::{Env, Program};
use crate::term::*;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ConformanceSource {
    Impl(ImplId),
    Model(ModelId),
}

impl fmt::Display for ConformanceSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConformanceSource::Impl(i) => write!(f, "{i}"),
            ConformanceSource::Model(m) => write!(f, "{m}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Step {
    /// `Implements` by an impl/model; children prove its where-clauses.
    Conformance {
        source: ConformanceSource,
        args: Vec<GenericArg>,
    },
    /// `Implements` by environment assumption `index`.
    Assumption { index: u32 },
    /// `Implements` for a rigid projection self type, by bound `bound` of its
    /// associated type; the single child proves the projection's trait ref.
    AliasBound { bound: u32 },
    /// `ProjectionEq` by an impl/model's associated value. Children prove the
    /// where-clauses, followed by `value_normalizations` `ProjectionEq`
    /// proofs normalizing projections inside the associated value.
    ProjectionConformance {
        source: ConformanceSource,
        args: Vec<GenericArg>,
        value_normalizations: u32,
    },
    /// `ProjectionEq` by environment assumption `index`.
    ProjectionAssumption { index: u32 },
    /// `ProjectionEq(p, p)` for a rigid projection; the child proves p's trait ref.
    RigidProjection,
    /// `ModeBound` computed structurally.
    Mode,
    /// A goal containing projections: children are the `ProjectionEq`
    /// proofs (innermost-first, left-to-right), then the normalized goal.
    Normalize,
    /// Leaf closing a cycle whose goals are all coinductive.
    CoinductiveHypothesis,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Derivation {
    pub goal: Goal,
    pub step: Step,
    pub children: Vec<Derivation>,
}

impl Derivation {
    pub fn leaf(goal: Goal, step: Step) -> Self {
        Derivation {
            goal,
            step,
            children: vec![],
        }
    }

    /// Whether any leaf closes a coinductive cycle.
    pub fn uses_coinduction(&self) -> bool {
        self.step == Step::CoinductiveHypothesis
            || self.children.iter().any(|c| c.uses_coinduction())
    }

    /// Number of nodes.
    pub fn size(&self) -> usize {
        1 + self.children.iter().map(|c| c.size()).sum::<usize>()
    }

    /// Deterministic indented rendering.
    pub fn render(&self) -> String {
        let mut s = String::new();
        self.render_into(0, &mut s);
        s
    }

    fn render_into(&self, depth: usize, s: &mut String) {
        use std::fmt::Write;
        let step = match &self.step {
            Step::Conformance { source, .. } => format!("by {source}"),
            Step::Assumption { index } => format!("by assumption {index}"),
            Step::AliasBound { bound } => format!("by alias bound {bound}"),
            Step::ProjectionConformance { source, .. } => format!("by value of {source}"),
            Step::ProjectionAssumption { index } => format!("by assumption {index}"),
            Step::RigidProjection => "rigid".into(),
            Step::Mode => "by modal kinding".into(),
            Step::Normalize => "after normalization".into(),
            Step::CoinductiveHypothesis => "by coinductive hypothesis".into(),
        };
        let _ = writeln!(s, "{}{}  [{}]", "  ".repeat(depth), self.goal, step);
        for c in &self.children {
            c.render_into(depth + 1, s);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivationError {
    /// Child indices from the root to the offending node.
    pub path: Vec<usize>,
    pub message: String,
}

impl fmt::Display for DerivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at {:?}: {}", self.path, self.message)
    }
}

/// Re-checks a derivation against the program and environment.
///
/// Checked per node: impl/model heads and where-clauses instantiate exactly
/// to the recorded goals (with recorded argument kinds), assumptions exist
/// in the environment, associated values match after replaying the recorded
/// normalizations, mode bounds recompute, rigid projections have a
/// placeholder or projection self type, and every coinductive hypothesis
/// closes a cycle (up to renaming of inference variables) whose goals are
/// all `Implements` goals of coinductive traits.
pub fn check_derivation(
    program: &Program,
    env: &Env,
    d: &Derivation,
) -> Result<(), DerivationError> {
    #[cfg(vesper_port)]
    return crate::port::check_derivation(program, env, d);
    #[cfg(not(vesper_port))]
    crate::engine::derivation::check_derivation(program, env, d)
}
