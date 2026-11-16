//! Clock-indexed guarded recursion (`Later[k, T]`, `guarded_fix[k]`) and
//! substructural quotient contract verification (`quotient Q = repr R modulo eq`)
//! for manuscript §5.4 and Appendix A.3.
//!
//! Clocks erase completely (`rho = 0`) prior to codegen and quotients adopt the
//! exact bit layout of `repr R`, while static checks reject unguarded zero-delay
//! algebraic loops and unverified `@respects` operations. This module provides
//! standalone verification primitives; full source elaboration remains in higher
//! compiler stages.

use crate::mode_vec::ModeVec;
use std::collections::{BTreeMap, BTreeSet};

/// Guarded recursion clock binder identifier `kappa : clock` (§5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClockId(pub u32);

/// Node identifier in a synchronous feedback / recursive signal graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// Identifier of an equivalence relation used in a `quotient` declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EquivId(pub u32);

/// Identifier of a public operation exported behind a `quotient` boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId(pub u32);

/// Bounded limits for causal loop analysis and quotient audit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_ops: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_nodes: 4096,
            max_edges: 16384,
            max_ops: 4096,
        }
    }
}

/// Errors from guarded recursion or quotient contract verification (§5.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClockQuotientError {
    ClockMismatch {
        expected: ClockId,
        found: ClockId,
    },
    UnknownNode {
        node: NodeId,
    },
    UnguardedAlgebraicLoop {
        node: NodeId,
    },
    QuotientModeWeakened {
        repr_mode: ModeVec,
        quotient_mode: ModeVec,
    },
    UnverifiedQuotientOperation {
        op: OpId,
    },
    MismatchedEquivalenceRelation {
        op: OpId,
        expected: EquivId,
        found: EquivId,
    },
    NodeLimit {
        max: usize,
    },
    EdgeLimit {
        max: usize,
    },
    OpLimit {
        max: usize,
    },
}

/// Clocked delayed type `Later[clock, T]` (§5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LaterType {
    pub clock: ClockId,
    pub inner_mode: ModeVec,
}

impl LaterType {
    pub const fn delay(clock: ClockId, inner_mode: ModeVec) -> Self {
        Self { clock, inner_mode }
    }

    /// Eliminates `Later[clock, T]` at the next causal tick of `active_clock`.
    /// Rejects cross-clock substitution so independent clocks remain isolated.
    pub const fn force_tick(self, active_clock: ClockId) -> Result<ModeVec, ClockQuotientError> {
        if self.clock.0 != active_clock.0 {
            Err(ClockQuotientError::ClockMismatch {
                expected: self.clock,
                found: active_clock,
            })
        } else {
            Ok(self.inner_mode)
        }
    }

    /// Clocks carry `@0` presence and erase completely at code generation (§5.4).
    pub const fn erases_at_codegen(self) -> bool {
        true
    }
}

/// Directed dataflow edge in a feedback / stream / DSP graph (§5.4).
///
/// * `guard: Some(k)` indicates a unit causal delay through `Later[k, T]`
///   (e.g., `Flux::z_inv` or `Resonance::sample_delay`).
/// * `guard: None` indicates a zero-delay combinational dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignalEdge {
    pub from: NodeId,
    pub to: NodeId,
    pub guard: Option<ClockId>,
}

/// Audits `guarded_fix[clock]` productivity (§5.4): every cycle in the dependency
/// graph must traverse at least one `Later[clock, T]` delay edge (`guard: Some(clock)`).
/// Purely combinational cycles (`guard: None`) and cycles guarded only by a
/// different clock (`guard: Some(j)` with `j != clock`) are rejected as
/// `UnguardedAlgebraicLoop`.
pub fn verify_guarded_recursion(
    clock: ClockId,
    nodes: &[NodeId],
    edges: &[SignalEdge],
    limits: Limits,
) -> Result<(), ClockQuotientError> {
    if nodes.len() > limits.max_nodes {
        return Err(ClockQuotientError::NodeLimit {
            max: limits.max_nodes,
        });
    }
    if edges.len() > limits.max_edges {
        return Err(ClockQuotientError::EdgeLimit {
            max: limits.max_edges,
        });
    }

    let node_set: BTreeSet<NodeId> = nodes.iter().copied().collect();
    let mut adj: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in edges {
        if !node_set.contains(&edge.from) {
            return Err(ClockQuotientError::UnknownNode { node: edge.from });
        }
        if !node_set.contains(&edge.to) {
            return Err(ClockQuotientError::UnknownNode { node: edge.to });
        }
        // Only edges guarded by a delay on the bound recursion clock `clock`
        // break instantaneous algebraic loops; unguarded (`None`) and cross-clock
        // (`Some(j)` where `j != clock`) edges still participate in cycle checks.
        if edge.guard != Some(clock) {
            adj.entry(edge.from).or_default().push(edge.to);
        }
    }

    // Iterative DFS cycle detection over zero-delay edges.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mark {
        Visiting,
        Done,
    }
    let mut state: BTreeMap<NodeId, Mark> = BTreeMap::new();
    for &start in &node_set {
        if state.contains_key(&start) {
            continue;
        }
        let mut stack: Vec<(NodeId, usize)> = vec![(start, 0)];
        state.insert(start, Mark::Visiting);
        while let Some(&(cur, idx)) = stack.last() {
            let next_opt = adj.get(&cur).and_then(|list| list.get(idx)).copied();
            if let Some(next) = next_opt {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
                match state.get(&next).copied() {
                    Some(Mark::Visiting) => {
                        return Err(ClockQuotientError::UnguardedAlgebraicLoop { node: next });
                    }
                    Some(Mark::Done) => {}
                    None => {
                        state.insert(next, Mark::Visiting);
                        stack.push((next, 0));
                    }
                }
            } else {
                state.insert(cur, Mark::Done);
                stack.pop();
            }
        }
    }

    Ok(())
}

/// Proof witness linking one public quotient operation to its `@respects`
/// equivalence-preservation proof (`Quot.sound` in Lean 4 extraction, §5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RespectsProof {
    pub op: OpId,
    pub equiv: EquivId,
    pub proved_respects: bool,
}

/// Declaration contract for `ModeSpec quotient Q = repr R modulo eq` (§5.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotientDecl {
    pub quotient_mode: ModeVec,
    pub repr_mode: ModeVec,
    pub equiv: EquivId,
    pub op_proofs: Vec<RespectsProof>,
}

/// Verifies a `quotient` declaration (§5.4, Appendix A.3):
/// 1. The public quotient modality must satisfy `repr_mode <= quotient_mode`
///    (cannot silently downgrade e.g. `linear` representation to `copy`).
/// 2. Every public operation must target the declared `equiv` relation and
///    discharge its `@respects` congruence obligation (`proved_respects == true`).
pub fn verify_quotient_decl(decl: &QuotientDecl, limits: Limits) -> Result<(), ClockQuotientError> {
    if decl.op_proofs.len() > limits.max_ops {
        return Err(ClockQuotientError::OpLimit {
            max: limits.max_ops,
        });
    }
    if !decl.repr_mode.leq(decl.quotient_mode) {
        return Err(ClockQuotientError::QuotientModeWeakened {
            repr_mode: decl.repr_mode,
            quotient_mode: decl.quotient_mode,
        });
    }
    for proof in &decl.op_proofs {
        if proof.equiv != decl.equiv {
            return Err(ClockQuotientError::MismatchedEquivalenceRelation {
                op: proof.op,
                expected: decl.equiv,
                found: proof.equiv,
            });
        }
        if !proof.proved_respects {
            return Err(ClockQuotientError::UnverifiedQuotientOperation { op: proof.op });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode_vec::{Contention, Order, Portability, Usage};

    #[test]
    fn clock_guarded_feedback_loops_succeed_while_zero_delay_loops_fail() {
        let k0 = ClockId(1);
        let k1 = ClockId(2);
        let delayed = LaterType::delay(k0, ModeVec::BOTTOM);
        assert!(delayed.erases_at_codegen());
        assert_eq!(delayed.force_tick(k0), Ok(ModeVec::BOTTOM));
        assert_eq!(
            delayed.force_tick(k1),
            Err(ClockQuotientError::ClockMismatch {
                expected: k0,
                found: k1,
            })
        );

        let nodes = [NodeId(1), NodeId(2), NodeId(3)];
        // DSP biquad / DBSP feedback loop: 1 -> 2 -> 3 is combinational, 3 -> 1 traverses `Later[k0, _]`.
        let guarded_cycle = [
            SignalEdge {
                from: NodeId(1),
                to: NodeId(2),
                guard: None,
            },
            SignalEdge {
                from: NodeId(2),
                to: NodeId(3),
                guard: None,
            },
            SignalEdge {
                from: NodeId(3),
                to: NodeId(1),
                guard: Some(k0),
            },
        ];
        assert_eq!(
            verify_guarded_recursion(k0, &nodes, &guarded_cycle, Limits::default()),
            Ok(())
        );

        // A cycle delayed only on a different clock `k1` cannot guard `guarded_fix[k0]`.
        let cross_clock_cycle = [
            SignalEdge {
                from: NodeId(1),
                to: NodeId(2),
                guard: None,
            },
            SignalEdge {
                from: NodeId(2),
                to: NodeId(3),
                guard: None,
            },
            SignalEdge {
                from: NodeId(3),
                to: NodeId(1),
                guard: Some(k1),
            },
        ];
        assert_eq!(
            verify_guarded_recursion(k0, &nodes, &cross_clock_cycle, Limits::default()),
            Err(ClockQuotientError::UnguardedAlgebraicLoop { node: NodeId(1) })
        );
        // The same graph is productive under `guarded_fix[k1]`.
        assert_eq!(
            verify_guarded_recursion(k1, &nodes, &cross_clock_cycle, Limits::default()),
            Ok(())
        );

        // Removing the `Later` unit delay creates an unguarded algebraic loop.
        let unguarded_cycle = [
            SignalEdge {
                from: NodeId(1),
                to: NodeId(2),
                guard: None,
            },
            SignalEdge {
                from: NodeId(2),
                to: NodeId(3),
                guard: None,
            },
            SignalEdge {
                from: NodeId(3),
                to: NodeId(1),
                guard: None,
            },
        ];
        assert_eq!(
            verify_guarded_recursion(k0, &nodes, &unguarded_cycle, Limits::default()),
            Err(ClockQuotientError::UnguardedAlgebraicLoop { node: NodeId(1) })
        );
    }

    #[test]
    fn quotient_decl_enforces_modal_soundness_and_respects_congruence() {
        let affine = ModeVec::NOMINAL_DEFAULT;
        let linear = ModeVec::new(
            Usage::Linear,
            Portability::Portable,
            Contention::Uncontended,
            Order::Unordered,
        );
        let eq = EquivId(7);

        let valid = QuotientDecl {
            quotient_mode: affine,
            repr_mode: affine,
            equiv: eq,
            op_proofs: vec![
                RespectsProof {
                    op: OpId(1),
                    equiv: eq,
                    proved_respects: true,
                },
                RespectsProof {
                    op: OpId(2),
                    equiv: eq,
                    proved_respects: true,
                },
            ],
        };
        assert_eq!(verify_quotient_decl(&valid, Limits::default()), Ok(()));

        // Cannot expose a `linear` repr behind an `affine` quotient.
        let weakened = QuotientDecl {
            quotient_mode: affine,
            repr_mode: linear,
            equiv: eq,
            op_proofs: vec![],
        };
        assert_eq!(
            verify_quotient_decl(&weakened, Limits::default()),
            Err(ClockQuotientError::QuotientModeWeakened {
                repr_mode: linear,
                quotient_mode: affine,
            })
        );

        // Missing or wrong-equiv `@respects` proof is rejected.
        let unproved = QuotientDecl {
            quotient_mode: affine,
            repr_mode: affine,
            equiv: eq,
            op_proofs: vec![RespectsProof {
                op: OpId(3),
                equiv: eq,
                proved_respects: false,
            }],
        };
        assert_eq!(
            verify_quotient_decl(&unproved, Limits::default()),
            Err(ClockQuotientError::UnverifiedQuotientOperation { op: OpId(3) })
        );

        let wrong_eq = QuotientDecl {
            quotient_mode: affine,
            repr_mode: affine,
            equiv: eq,
            op_proofs: vec![RespectsProof {
                op: OpId(4),
                equiv: EquivId(99),
                proved_respects: true,
            }],
        };
        assert_eq!(
            verify_quotient_decl(&wrong_eq, Limits::default()),
            Err(ClockQuotientError::MismatchedEquivalenceRelation {
                op: OpId(4),
                expected: eq,
                found: EquivId(99),
            })
        );
    }
}
