//! Quantitative `@0` ghost linearity, `0 * Gamma` scaling, `discard(Gamma)`,
//! context splitting (`[Split-Copy]` / `[Split-Sub]`), erasure non-interference,
//! and closure-capture checks (`[T-Abs]`) for manuscript §3.4 and §4.1–§4.2.
//!
//! This module provides standalone context-level rules for the substructural
//! calculus; full source AST/HIR elaboration remains in the compiler facade.

use crate::mode_vec::{Contention, ModeVec, Order, Portability, Usage};
use std::collections::BTreeMap;

/// Quantitative runtime presence `rho in {0, 1}` (§3.1, §3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Presence {
    /// `@0`: ghost proof witness / refinement index, erased before machine-code emission.
    Ghost = 0,
    /// `@1`: runtime value present in generated code.
    Runtime = 1,
}

/// Place binding capability kind in context `Gamma` (§4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlaceKind {
    Own,
    Shr,
    Mut,
    Out,
}

/// Unique binding identity within a typing context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BindingId(pub u32);

/// Entry in the substructural typing context `Gamma` (§4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Binding {
    pub mode: ModeVec,
    pub presence: Presence,
    pub kind: PlaceKind,
}

/// Runtime operation kind checked for strict `@0` non-interference (§3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuntimeUseKind {
    ReturnValue,
    ControlFlowBranch,
    PointerIndex,
    EffectIo,
}

/// Placement of a binding when splitting `Gamma = Gamma_1 (*) Gamma_2` (§4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SplitChoice {
    Left,
    Right,
    Both,
    Omit,
}

/// Typed errors from `@0` non-interference, context splitting, `discard`, or `[T-Abs]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GhostError {
    DuplicateBinding {
        id: BindingId,
    },
    UnknownBinding {
        id: BindingId,
    },
    GhostUsedInRuntime {
        id: BindingId,
        use_kind: RuntimeUseKind,
    },
    NonDiscardableResidual {
        id: BindingId,
        mode: ModeVec,
        kind: PlaceKind,
    },
    NonlinearDuplicateInSplit {
        id: BindingId,
    },
    UnconsumedLinearInSplit {
        id: BindingId,
    },
    NonOwnedSplit {
        id: BindingId,
        kind: PlaceKind,
    },
    BorrowedOrOutCapturedInClosure {
        id: BindingId,
        kind: PlaceKind,
    },
    ClosureModeExceeded {
        id: BindingId,
        captured: ModeVec,
        fn_mode: ModeVec,
    },
}

/// Substructural place context `Gamma` tracking `(rho, mode)` and [`PlaceKind`] (§4.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    bindings: BTreeMap<BindingId, Binding>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bindings(&self) -> &BTreeMap<BindingId, Binding> {
        &self.bindings
    }

    pub fn insert(&mut self, id: BindingId, binding: Binding) -> Result<(), GhostError> {
        if self.bindings.contains_key(&id) {
            return Err(GhostError::DuplicateBinding { id });
        }
        self.bindings.insert(id, binding);
        Ok(())
    }

    /// Enforces `[T-Var]` presence discipline (`rho_ctx <= rho`, §3.4, §4.2):
    /// `@0` ghost bindings may never flow into `@1` runtime control flow,
    /// pointer arithmetic, runtime return values, or I/O.
    pub fn check_use(
        &self,
        id: BindingId,
        site: Presence,
        use_kind: RuntimeUseKind,
    ) -> Result<Binding, GhostError> {
        let Some(&binding) = self.bindings.get(&id) else {
            return Err(GhostError::UnknownBinding { id });
        };
        if site == Presence::Runtime && binding.presence == Presence::Ghost {
            return Err(GhostError::GhostUsedInRuntime { id, use_kind });
        }
        Ok(binding)
    }

    /// Computes `0 * Gamma` for pure refinement predicates (§3.4): demotes `@1`
    /// runtime bindings to `@0` (`Presence::Ghost`) so predicates can inspect
    /// them without consuming runtime linearity, while preserving `@0` ghost
    /// substructural witnesses (`ghost linear`) at their full [`ModeVec`].
    pub fn scale_zero_for_predicate(&self) -> Self {
        let copy_erased = ModeVec::new(
            Usage::Copy,
            Portability::Portable,
            Contention::Uncontended,
            Order::Unordered,
        );
        let bindings = self
            .bindings
            .iter()
            .map(|(&id, &b)| {
                let scaled = match b.presence {
                    Presence::Runtime => Binding {
                        mode: copy_erased,
                        presence: Presence::Ghost,
                        kind: b.kind,
                    },
                    Presence::Ghost => b,
                };
                (id, scaled)
            })
            .collect();
        Self { bindings }
    }

    /// Checks `discard(Gamma)` (§4.1): every residual binding must be an
    /// initialized non-mut-borrow value whose usage mode satisfies `u <= Affine`
    /// (neither `Usage::Linear`, nor `PlaceKind::Mut`, nor `PlaceKind::Out`).
    pub fn is_discardable(&self) -> Result<(), GhostError> {
        for (&id, &b) in &self.bindings {
            let initialized_value = matches!(b.kind, PlaceKind::Own | PlaceKind::Shr);
            if !initialized_value || b.mode.usage() > Usage::Affine {
                return Err(GhostError::NonDiscardableResidual {
                    id,
                    mode: b.mode,
                    kind: b.kind,
                });
            }
        }
        Ok(())
    }

    /// Splits `Gamma = Gamma_1 (*) Gamma_2` according to `[Split-Copy]` and
    /// `[Split-Sub]` (§4.1).
    ///
    /// * `Copy` bindings may go `Left`, `Right`, `Both`, or (if `Own`/`Shr`) `Omit`.
    /// * `Affine` owned bindings must go exclusively `Left`, `Right`, or `Omit`
    ///   (never `Both`).
    /// * `Linear` owned bindings must go strictly `Left` or `Right` (never
    ///   `Both` and never `Omit`).
    pub fn split(
        &self,
        mut choose: impl FnMut(BindingId, &Binding) -> SplitChoice,
    ) -> Result<(Context, Context), GhostError> {
        let mut left = Context::new();
        let mut right = Context::new();

        for (&id, &b) in &self.bindings {
            let choice = choose(id, &b);
            match (b.mode.usage(), b.kind, choice) {
                (Usage::Copy, PlaceKind::Own | PlaceKind::Shr, SplitChoice::Both) => {
                    left.bindings.insert(id, b);
                    right.bindings.insert(id, b);
                }
                (_, PlaceKind::Own | PlaceKind::Shr, SplitChoice::Left) => {
                    left.bindings.insert(id, b);
                }
                (_, PlaceKind::Own | PlaceKind::Shr, SplitChoice::Right) => {
                    right.bindings.insert(id, b);
                }
                (
                    Usage::Copy | Usage::Affine,
                    PlaceKind::Own | PlaceKind::Shr,
                    SplitChoice::Omit,
                ) => {}
                (Usage::Linear, PlaceKind::Own | PlaceKind::Shr, SplitChoice::Omit) => {
                    return Err(GhostError::UnconsumedLinearInSplit { id });
                }
                (
                    Usage::Affine | Usage::Linear,
                    PlaceKind::Own | PlaceKind::Shr,
                    SplitChoice::Both,
                ) => {
                    return Err(GhostError::NonlinearDuplicateInSplit { id });
                }
                (_, PlaceKind::Mut | PlaceKind::Out, SplitChoice::Left) => {
                    left.bindings.insert(id, b);
                }
                (_, PlaceKind::Mut | PlaceKind::Out, SplitChoice::Right) => {
                    right.bindings.insert(id, b);
                }
                (_, PlaceKind::Mut | PlaceKind::Out, SplitChoice::Both | SplitChoice::Omit) => {
                    return Err(GhostError::NonOwnedSplit { id, kind: b.kind });
                }
            }
        }

        Ok((left, right))
    }

    /// Enforces `[T-Abs]` closure-capture discipline (§4.2): closures capture
    /// only owned bindings (no `Shr`, `Mut`, or `Out` places), and each
    /// captured binding must satisfy `m_z <= m_fn`.
    pub fn check_closure_capture(&self, fn_mode: ModeVec) -> Result<(), GhostError> {
        for (&id, &b) in &self.bindings {
            if b.kind != PlaceKind::Own {
                return Err(GhostError::BorrowedOrOutCapturedInClosure { id, kind: b.kind });
            }
            if !b.mode.leq(fn_mode) {
                return Err(GhostError::ClosureModeExceeded {
                    id,
                    captured: b.mode,
                    fn_mode,
                });
            }
        }
        Ok(())
    }

    /// Erases all `@0` ghost bindings prior to machine-code emission (§3.4, Theorem 4).
    pub fn erase_ghosts(&self) -> Self {
        let bindings = self
            .bindings
            .iter()
            .filter(|(_, b)| b.presence == Presence::Runtime)
            .map(|(&id, &b)| (id, b))
            .collect();
        Self { bindings }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(usage: Usage, presence: Presence) -> Binding {
        Binding {
            mode: ModeVec::new(
                usage,
                Portability::Portable,
                Contention::Uncontended,
                Order::Unordered,
            ),
            presence,
            kind: PlaceKind::Own,
        }
    }

    #[test]
    fn ghost_non_interference_scale_zero_and_erasure() {
        let slot_perm = BindingId(1);
        let runtime_tx = BindingId(2);
        let runtime_slice = BindingId(3);

        let mut ctx = Context::new();
        ctx.insert(slot_perm, owned(Usage::Linear, Presence::Ghost))
            .unwrap();
        ctx.insert(runtime_tx, owned(Usage::Linear, Presence::Runtime))
            .unwrap();
        ctx.insert(runtime_slice, owned(Usage::Copy, Presence::Runtime))
            .unwrap();

        // `@0` ghost binding cannot steer `@1` control flow or pointer arithmetic.
        assert_eq!(
            ctx.check_use(
                slot_perm,
                Presence::Runtime,
                RuntimeUseKind::ControlFlowBranch
            ),
            Err(GhostError::GhostUsedInRuntime {
                id: slot_perm,
                use_kind: RuntimeUseKind::ControlFlowBranch,
            })
        );
        // Inside a `@0` context, both `@0` and `0 * @1` scaled runtime bindings are usable.
        let scaled = ctx.scale_zero_for_predicate();
        assert!(
            scaled
                .check_use(slot_perm, Presence::Ghost, RuntimeUseKind::ReturnValue)
                .is_ok()
        );
        assert_eq!(
            scaled.bindings().get(&runtime_tx).unwrap().mode.usage(),
            Usage::Copy
        );
        // Existing `@0` linear witness retains its linear bound inside `@0` proof checking!
        assert_eq!(
            scaled.bindings().get(&slot_perm).unwrap().mode.usage(),
            Usage::Linear
        );

        // Erasure strips `@0` bindings completely and retains `@1` runtime bindings.
        let runtime_only = ctx.erase_ghosts();
        assert!(!runtime_only.bindings().contains_key(&slot_perm));
        assert!(runtime_only.bindings().contains_key(&runtime_tx));
        assert!(runtime_only.bindings().contains_key(&runtime_slice));
    }

    #[test]
    fn context_splitting_discardability_and_closure_capture_rules() {
        let copy_id = BindingId(1);
        let affine_id = BindingId(2);
        let linear_id = BindingId(3);
        let mut_borrow = BindingId(4);

        let mut ctx = Context::new();
        ctx.insert(copy_id, owned(Usage::Copy, Presence::Runtime))
            .unwrap();
        ctx.insert(affine_id, owned(Usage::Affine, Presence::Runtime))
            .unwrap();
        ctx.insert(linear_id, owned(Usage::Linear, Presence::Runtime))
            .unwrap();

        // `Copy` contracts into Both; `Linear` goes Left; `Affine` goes Right.
        let (lhs, rhs) = ctx
            .split(|id, _| match id {
                id if id == copy_id => SplitChoice::Both,
                id if id == linear_id => SplitChoice::Left,
                _ => SplitChoice::Right,
            })
            .unwrap();
        assert!(lhs.bindings().contains_key(&copy_id));
        assert!(rhs.bindings().contains_key(&copy_id));
        assert!(lhs.bindings().contains_key(&linear_id));
        assert!(!rhs.bindings().contains_key(&linear_id));
        assert!(rhs.is_discardable().is_ok());
        assert_eq!(
            lhs.is_discardable(),
            Err(GhostError::NonDiscardableResidual {
                id: linear_id,
                mode: owned(Usage::Linear, Presence::Runtime).mode,
                kind: PlaceKind::Own,
            })
        );

        // Duplicating `Affine`/`Linear` or dropping `Linear` in split fails.
        assert_eq!(
            ctx.split(|_, _| SplitChoice::Both),
            Err(GhostError::NonlinearDuplicateInSplit { id: affine_id })
        );
        assert_eq!(
            ctx.split(|id, _| if id == linear_id {
                SplitChoice::Omit
            } else {
                SplitChoice::Left
            }),
            Err(GhostError::UnconsumedLinearInSplit { id: linear_id })
        );

        // `[T-Abs]`: capturing `Linear` forces `fn` mode >= `Linear`, and rejects borrows.
        assert_eq!(
            ctx.check_closure_capture(ModeVec::NOMINAL_DEFAULT),
            Err(GhostError::ClosureModeExceeded {
                id: linear_id,
                captured: owned(Usage::Linear, Presence::Runtime).mode,
                fn_mode: ModeVec::NOMINAL_DEFAULT,
            })
        );
        let linear_fn = ModeVec::new(
            Usage::Linear,
            Portability::Portable,
            Contention::Uncontended,
            Order::Unordered,
        );
        assert_eq!(ctx.check_closure_capture(linear_fn), Ok(()));

        ctx.insert(
            mut_borrow,
            Binding {
                mode: ModeVec::BOTTOM,
                presence: Presence::Runtime,
                kind: PlaceKind::Mut,
            },
        )
        .unwrap();
        assert_eq!(
            ctx.is_discardable(),
            Err(GhostError::NonDiscardableResidual {
                id: linear_id,
                mode: owned(Usage::Linear, Presence::Runtime).mode,
                kind: PlaceKind::Own,
            })
        );
        assert_eq!(
            ctx.check_closure_capture(linear_fn),
            Err(GhostError::BorrowedOrOutCapturedInClosure {
                id: mut_borrow,
                kind: PlaceKind::Mut,
            })
        );
    }
}
