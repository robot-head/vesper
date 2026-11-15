//! Decidable liquid refinement filtering and strong updates (§4.5).
//!
//! Models the three refinement judgments from §4.5:
//! - `[Sub-Refine]`: decidable quantifier-free linear integer (`QF_LIA`) and
//!   bit-vector (`QF_BV`) entailment over conjunctions of refinement clauses.
//! - `[T-StrongUpdate]`: prophecy-free in-place typestate transition
//!   (`inout x: tau_in ~> tau_out`), updating `Gamma[x |-> tau_out]` only when
//!   `x` has exclusive unaliased `own`/`Uncontended` ownership and `tau_old <: tau_in`.
//! - `[M-ContendStrip]`: concurrency-safe refinement filtering when sharing across
//!   a `Pulse::scope` boundary shifts a view to `Contention::Contended`,
//!   stripping non-monotonic `Mutable` interior predicates while retaining
//!   `Immutable` guarantees.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ast::Type;
use crate::mode_vec::{Contention, ModeVec};

/// Whether a refinement predicate remains invariant under shared `contended` access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Stability {
    /// Holds permanently across concurrent shared views (`\phi_imm`).
    Immutable,
    /// Relies on interior mutable state (`\phi_mut`); stripped by `[M-ContendStrip]`.
    Mutable,
}

/// Relational comparison operator in `QF_LIA` refinement atoms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RelOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Quantifier-free liquid refinement atom in `L_Refine` (§4.5).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RefineAtom {
    /// Linear comparison `var <op> constant` (QF_LIA).
    IntCmp { var: String, op: RelOp, value: i128 },
    /// Closed integer interval `lo <= var && var <= hi` (QF_LIA).
    IntRange { var: String, lo: i128, hi: i128 },
    /// Fixed-width bitwise mask `(var & mask) == expected` (QF_BV).
    BitMaskEq {
        var: String,
        mask: u128,
        expected: u128,
    },
    /// Pure total ghost measure relation `measure(var) <op> value`.
    MeasureCmp {
        measure: String,
        op: RelOp,
        value: i128,
    },
    /// Named uninterpreted/Horn predicate atom `pred(subject)`.
    Horn { pred: String, subject: String },
}

/// One conjunct in a refinement type `{ x : tau | /\ clauses }`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RefineClause {
    pub atom: RefineAtom,
    pub stability: Stability,
}

impl RefineClause {
    pub fn immutable(atom: RefineAtom) -> Self {
        Self {
            atom,
            stability: Stability::Immutable,
        }
    }

    pub fn mutable(atom: RefineAtom) -> Self {
        Self {
            atom,
            stability: Stability::Mutable,
        }
    }
}

/// Refined type `{ binder : base | clauses } @ mode`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefineType {
    pub base: Type,
    pub binder: String,
    pub clauses: Vec<RefineClause>,
    pub mode: ModeVec,
}

impl RefineType {
    pub fn new(
        base: Type,
        binder: impl Into<String>,
        clauses: Vec<RefineClause>,
        mode: ModeVec,
    ) -> Self {
        Self {
            base,
            binder: binder.into(),
            clauses,
            mode,
        }
    }

    /// `[M-ContendStrip]` premise: strip non-monotonic `Mutable` interior predicates.
    pub fn strip_mutable_preds(&self) -> Vec<RefineClause> {
        self.clauses
            .iter()
            .filter(|c| c.stability == Stability::Immutable)
            .cloned()
            .collect()
    }

    /// `[M-ContendStrip]`: share across a `Pulse::scope` thread boundary by shifting
    /// contention to `Contention::Contended` and stripping `Mutable` interior clauses.
    pub fn contend(&self) -> Self {
        Self {
            base: self.base.clone(),
            binder: self.binder.clone(),
            clauses: self.strip_mutable_preds(),
            mode: ModeVec::new(
                self.mode.usage(),
                self.mode.portability(),
                Contention::Contended,
                self.mode.order(),
            ),
        }
    }
}

/// Extracted integer interval domain for `[Sub-Refine]` bounded entailment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IntDomain {
    lo: Option<i128>,
    hi: Option<i128>,
    /// Optional non-equality constraint `var != k`.
    neq: Option<i128>,
    /// Bitmask `(mask, expected)` equalities implied by `(var & mask) == expected`.
    mask_known: u128,
    mask_value: u128,
}

impl IntDomain {
    fn unconstrained() -> Self {
        Self {
            lo: None,
            hi: None,
            neq: None,
            mask_known: 0,
            mask_value: 0,
        }
    }

    fn apply_cmp(&mut self, op: RelOp, value: i128) -> bool {
        match op {
            RelOp::Eq => {
                self.apply_lo(value);
                self.apply_hi(value);
            }
            RelOp::Ne => {
                if self.lo == Some(value) && self.hi == Some(value) {
                    return false;
                }
                self.neq = Some(value);
            }
            RelOp::Lt => {
                let Some(hi) = value.checked_sub(1) else {
                    return false;
                };
                self.apply_hi(hi);
            }
            RelOp::Le => self.apply_hi(value),
            RelOp::Gt => {
                let Some(lo) = value.checked_add(1) else {
                    return false;
                };
                self.apply_lo(lo);
            }
            RelOp::Ge => self.apply_lo(value),
        }
        !self.is_unsat()
    }

    fn apply_range(&mut self, lo: i128, hi: i128) -> bool {
        self.apply_lo(lo);
        self.apply_hi(hi);
        !self.is_unsat()
    }

    fn apply_mask(&mut self, mask: u128, expected: u128) -> bool {
        let masked_expected = expected & mask;
        let overlap = self.mask_known & mask;
        if (self.mask_value & overlap) != (masked_expected & overlap) {
            return false;
        }
        self.mask_known |= mask;
        self.mask_value |= masked_expected;
        true
    }

    fn apply_lo(&mut self, lo: i128) {
        self.lo = Some(match self.lo {
            Some(prev) => prev.max(lo),
            None => lo,
        });
    }

    fn apply_hi(&mut self, hi: i128) {
        self.hi = Some(match self.hi {
            Some(prev) => prev.min(hi),
            None => hi,
        });
    }

    fn is_unsat(&self) -> bool {
        match (self.lo, self.hi) {
            (Some(lo), Some(hi)) => {
                if lo > hi {
                    return true;
                }
                if lo == hi && self.neq == Some(lo) {
                    return true;
                }
                if lo == hi && lo >= 0 {
                    let bits = lo as u128;
                    if (bits & self.mask_known) != (self.mask_value & self.mask_known) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    fn entails_cmp(&self, op: RelOp, value: i128) -> bool {
        match op {
            RelOp::Eq => self.lo == Some(value) && self.hi == Some(value),
            RelOp::Ne => {
                self.lo.is_some_and(|lo| lo > value)
                    || self.hi.is_some_and(|hi| hi < value)
                    || self.neq == Some(value)
            }
            RelOp::Lt => self.hi.is_some_and(|hi| hi < value),
            RelOp::Le => self.hi.is_some_and(|hi| hi <= value),
            RelOp::Gt => self.lo.is_some_and(|lo| lo > value),
            RelOp::Ge => self.lo.is_some_and(|lo| lo >= value),
        }
    }

    fn entails_mask(&self, mask: u128, expected: u128) -> bool {
        let target = expected & mask;
        if let (Some(lo), Some(hi)) = (self.lo, self.hi)
            && lo == hi
            && lo >= 0
        {
            return ((lo as u128) & mask) == target;
        }
        (self.mask_known & mask) == mask && (self.mask_value & mask) == target
    }
}

fn normalize_atom(atom: &RefineAtom, binder: &str) -> RefineAtom {
    match atom {
        RefineAtom::IntCmp { var, op, value } if var == binder => RefineAtom::IntCmp {
            var: "__v".into(),
            op: *op,
            value: *value,
        },
        RefineAtom::IntRange { var, lo, hi } if var == binder => RefineAtom::IntRange {
            var: "__v".into(),
            lo: *lo,
            hi: *hi,
        },
        RefineAtom::BitMaskEq {
            var,
            mask,
            expected,
        } if var == binder => RefineAtom::BitMaskEq {
            var: "__v".into(),
            mask: *mask,
            expected: *expected,
        },
        RefineAtom::Horn { pred, subject } if subject == binder => RefineAtom::Horn {
            pred: pred.clone(),
            subject: "__v".into(),
        },
        other => other.clone(),
    }
}

/// `[Sub-Refine]` judgment: `Delta ; Phi |- lhs <: rhs`.
///
/// Checks base type equality, modal subtyping (`lhs.mode <= rhs.mode`),
/// and decidable conjunction entailment under path condition `phi`.
/// Inconsistent assumptions (ex falso) discharge any target clause, and
/// `MeasureCmp` / `IntCmp` / `IntRange` / `BitMaskEq` are solved in interval +
/// bitwise-mask abstract domains.
pub fn is_refine_subtype(phi: &[RefineClause], lhs: &RefineType, rhs: &RefineType) -> bool {
    if lhs.base != rhs.base || !lhs.mode.leq(rhs.mode) {
        return false;
    }

    let mut domains: BTreeMap<String, IntDomain> = BTreeMap::new();
    let mut measures: BTreeMap<String, IntDomain> = BTreeMap::new();
    let mut assumed_atoms: Vec<RefineAtom> = Vec::new();
    let mut unsat = false;

    for clause in phi.iter().chain(lhs.clauses.iter()) {
        let norm = normalize_atom(&clause.atom, &lhs.binder);
        match &norm {
            RefineAtom::IntCmp { var, op, value } => {
                let d = domains
                    .entry(var.clone())
                    .or_insert_with(IntDomain::unconstrained);
                if !d.apply_cmp(*op, *value) {
                    unsat = true;
                }
            }
            RefineAtom::IntRange { var, lo, hi } => {
                let d = domains
                    .entry(var.clone())
                    .or_insert_with(IntDomain::unconstrained);
                if !d.apply_range(*lo, *hi) {
                    unsat = true;
                }
            }
            RefineAtom::BitMaskEq {
                var,
                mask,
                expected,
            } => {
                let d = domains
                    .entry(var.clone())
                    .or_insert_with(IntDomain::unconstrained);
                if !d.apply_mask(*mask, *expected) {
                    unsat = true;
                }
            }
            RefineAtom::MeasureCmp { measure, op, value } => {
                let d = measures
                    .entry(measure.clone())
                    .or_insert_with(IntDomain::unconstrained);
                if !d.apply_cmp(*op, *value) {
                    unsat = true;
                }
            }
            RefineAtom::Horn { .. } => {}
        }
        assumed_atoms.push(norm);
    }

    if unsat {
        return true;
    }

    for target in &rhs.clauses {
        let norm = normalize_atom(&target.atom, &rhs.binder);
        if assumed_atoms.contains(&norm) {
            continue;
        }
        let entailed = match &norm {
            RefineAtom::IntCmp { var, op, value } => {
                domains.get(var).is_some_and(|d| d.entails_cmp(*op, *value))
            }
            RefineAtom::IntRange { var, lo, hi } => domains
                .get(var)
                .is_some_and(|d| d.entails_cmp(RelOp::Ge, *lo) && d.entails_cmp(RelOp::Le, *hi)),
            RefineAtom::BitMaskEq {
                var,
                mask,
                expected,
            } => domains
                .get(var)
                .is_some_and(|d| d.entails_mask(*mask, *expected)),
            RefineAtom::MeasureCmp { measure, op, value } => measures
                .get(measure)
                .is_some_and(|d| d.entails_cmp(*op, *value)),
            RefineAtom::Horn { .. } => false,
        };
        if !entailed {
            return false;
        }
    }

    true
}

/// Variable binding in the local refinement context `Gamma`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRefineBinding {
    pub ty: RefineType,
    /// True if an active borrow/projection alias exists on this place.
    pub aliased: bool,
    pub initialized: bool,
}

/// Typing context `Gamma` mapping local variable names to owned/aliased refined places.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalRefineEnv {
    bindings: BTreeMap<String, LocalRefineBinding>,
}

impl LocalRefineEnv {
    pub fn insert(&mut self, name: impl Into<String>, binding: LocalRefineBinding) {
        self.bindings.insert(name.into(), binding);
    }

    pub fn get(&self, name: &str) -> Option<&LocalRefineBinding> {
        self.bindings.get(name)
    }

    /// Share `name` across a `Pulse::scope` thread boundary, returning its
    /// `[M-ContendStrip]`-filtered view.
    pub fn contend_view(&self, name: &str) -> Option<RefineType> {
        self.bindings
            .get(name)
            .filter(|b| b.initialized)
            .map(|b| b.ty.contend())
    }
}

/// Function signature with an `inout y: tau_in ~> tau_out` strong update (§4.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrongUpdateSig {
    pub tau_in: RefineType,
    pub tau_out: RefineType,
    pub ret: RefineType,
}

/// Errors raised when `[T-StrongUpdate]` premises fail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrongUpdateError {
    UnknownOrUninitializedBinding { var: String },
    AliasedOrContendedTarget { var: String },
    PreconditionNotSatisfied { var: String },
}

/// `[T-StrongUpdate]`: validate exclusive unaliased `own`/`Uncontended` ownership
/// and `Phi |- tau_old <: tau_in`, then update `Gamma[var |-> tau_out]` in place
/// and return `sig.ret`.
pub fn apply_strong_update(
    env: &mut LocalRefineEnv,
    phi: &[RefineClause],
    var: &str,
    sig: &StrongUpdateSig,
) -> Result<RefineType, StrongUpdateError> {
    let binding = env
        .bindings
        .get(var)
        .filter(|b| b.initialized)
        .ok_or_else(|| StrongUpdateError::UnknownOrUninitializedBinding {
            var: var.to_string(),
        })?;

    if binding.aliased || binding.ty.mode.contention() == Contention::Contended {
        return Err(StrongUpdateError::AliasedOrContendedTarget {
            var: var.to_string(),
        });
    }

    if !is_refine_subtype(phi, &binding.ty, &sig.tau_in) {
        return Err(StrongUpdateError::PreconditionNotSatisfied {
            var: var.to_string(),
        });
    }

    if let Some(slot) = env.bindings.get_mut(var) {
        slot.ty = sig.tau_out.clone();
    }

    Ok(sig.ret.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode_vec::{Order, Portability, Usage};

    fn affine_uncontended() -> ModeVec {
        ModeVec::new(
            Usage::Affine,
            Portability::Portable,
            Contention::Uncontended,
            Order::Unordered,
        )
    }

    #[test]
    fn contend_strip_removes_mutable_clauses_and_marks_contended() {
        let rvec = RefineType::new(
            Type::named("RegionVec"),
            "v",
            vec![
                RefineClause::immutable(RefineAtom::IntCmp {
                    var: "cap".into(),
                    op: RelOp::Eq,
                    value: 64,
                }),
                RefineClause::mutable(RefineAtom::MeasureCmp {
                    measure: "len".into(),
                    op: RelOp::Eq,
                    value: 3,
                }),
            ],
            affine_uncontended(),
        );

        let shared = rvec.contend();
        assert_eq!(shared.mode.contention(), Contention::Contended);
        assert_eq!(shared.clauses.len(), 1);
        assert_eq!(shared.clauses[0].stability, Stability::Immutable);
        assert_eq!(
            shared.clauses[0].atom,
            RefineAtom::IntCmp {
                var: "cap".into(),
                op: RelOp::Eq,
                value: 64,
            }
        );
    }

    #[test]
    fn sub_refine_checks_intervals_and_bitmasks() {
        let aligned = RefineType::new(
            Type::named("u64"),
            "n",
            vec![
                RefineClause::immutable(RefineAtom::IntRange {
                    var: "n".into(),
                    lo: 64,
                    hi: 4096,
                }),
                RefineClause::immutable(RefineAtom::BitMaskEq {
                    var: "n".into(),
                    mask: 63,
                    expected: 0,
                }),
            ],
            affine_uncontended(),
        );
        let nonzero_aligned = RefineType::new(
            Type::named("u64"),
            "x",
            vec![
                RefineClause::immutable(RefineAtom::IntCmp {
                    var: "x".into(),
                    op: RelOp::Ne,
                    value: 0,
                }),
                RefineClause::immutable(RefineAtom::IntCmp {
                    var: "x".into(),
                    op: RelOp::Le,
                    value: 4096,
                }),
                RefineClause::immutable(RefineAtom::BitMaskEq {
                    var: "x".into(),
                    mask: 7,
                    expected: 0,
                }),
            ],
            affine_uncontended(),
        );

        assert!(is_refine_subtype(&[], &aligned, &nonzero_aligned));
        assert!(!is_refine_subtype(&[], &nonzero_aligned, &aligned));
    }

    #[test]
    fn strong_update_transitions_typestate_and_rejects_aliased() {
        let s0 = RefineType::new(
            Type::named("ShardStats"),
            "s",
            vec![RefineClause::mutable(RefineAtom::MeasureCmp {
                measure: "pkts".into(),
                op: RelOp::Eq,
                value: 0,
            })],
            affine_uncontended(),
        );
        let s1 = RefineType::new(
            Type::named("ShardStats"),
            "s",
            vec![RefineClause::mutable(RefineAtom::MeasureCmp {
                measure: "pkts".into(),
                op: RelOp::Eq,
                value: 1,
            })],
            affine_uncontended(),
        );
        let ret_unit = RefineType::new(Type::unit(), "_", vec![], ModeVec::BOTTOM);

        let mut env = LocalRefineEnv::default();
        env.insert(
            "stats",
            LocalRefineBinding {
                ty: s0.clone(),
                aliased: false,
                initialized: true,
            },
        );
        env.insert(
            "borrowed_stats",
            LocalRefineBinding {
                ty: s0.clone(),
                aliased: true,
                initialized: true,
            },
        );

        let sig = StrongUpdateSig {
            tau_in: s0,
            tau_out: s1.clone(),
            ret: ret_unit,
        };

        assert_eq!(
            apply_strong_update(&mut env, &[], "borrowed_stats", &sig),
            Err(StrongUpdateError::AliasedOrContendedTarget {
                var: "borrowed_stats".into()
            })
        );
        assert!(apply_strong_update(&mut env, &[], "stats", &sig).is_ok());
        assert_eq!(env.get("stats").unwrap().ty, s1);
    }
}
