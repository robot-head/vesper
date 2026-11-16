//! Quantitative `@0` ghost linearity, `0 * Gamma` scaling, `discard(Gamma)`,
//! context splitting (`[Split-Copy]` / `[Split-Sub]`), erasure non-interference,
//! and closure-capture checks (`[T-Abs]`) for manuscript §3.4 and §4.1–§4.2.
//!
//! This module provides standalone context-level rules for the substructural
//! calculus; full source AST/HIR elaboration remains in the compiler facade.

use crate::mode_vec::{ModeVec, Usage};
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

impl Binding {
    fn discardable(self) -> bool {
        self.kind == PlaceKind::Shr
            || (self.kind == PlaceKind::Own
                && self.mode.usage() <= Usage::Affine
                && !self.mode.is_lifo())
    }
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
    EffectfulGhostUse {
        id: BindingId,
    },
    OrderedOwnerRequiresStack {
        id: BindingId,
    },
    UninitializedObservation {
        id: BindingId,
    },
    InvalidOwnedUse {
        id: BindingId,
    },
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
        if binding.kind == PlaceKind::Own && binding.mode.is_lifo() {
            return Err(GhostError::OrderedOwnerRequiresStack { id });
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
        if binding.kind == PlaceKind::Out {
            return Err(GhostError::UninitializedObservation { id });
        }
        if site == Presence::Runtime && binding.presence == Presence::Ghost {
            return Err(GhostError::GhostUsedInRuntime { id, use_kind });
        }
        if site == Presence::Ghost && use_kind == RuntimeUseKind::EffectIo {
            return Err(GhostError::EffectfulGhostUse { id });
        }
        if site == Presence::Ghost && binding.presence == Presence::Runtime {
            return Ok(Binding {
                presence: Presence::Ghost,
                kind: PlaceKind::Shr,
                ..binding
            });
        }
        Ok(binding)
    }

    /// Moves one owned binding at its own presence. Ghost inspection cannot
    /// discharge a runtime owner. This is context accounting, not an evaluator.
    pub fn take_owned(&mut self, id: BindingId, site: Presence) -> Result<Binding, GhostError> {
        let binding = *self
            .bindings
            .get(&id)
            .ok_or(GhostError::UnknownBinding { id })?;
        if binding.kind != PlaceKind::Own || binding.presence != site {
            return Err(GhostError::InvalidOwnedUse { id });
        }
        self.bindings.remove(&id);
        Ok(binding)
    }

    /// Borrows initialized runtime bindings as ghost shared observations for
    /// pure predicates. Modes stay intact; existing ghost owners retain their
    /// obligations. Uninitialized `out` slots are unavailable for observation.
    /// The caller must separately check total purity and the observation scope.
    pub fn scale_zero_for_predicate(&self) -> Self {
        let bindings = self
            .bindings
            .iter()
            .filter_map(|(&id, &b)| match b.presence {
                Presence::Runtime if b.kind == PlaceKind::Out => None,
                Presence::Runtime => Some((
                    id,
                    Binding {
                        presence: Presence::Ghost,
                        kind: PlaceKind::Shr,
                        ..b
                    },
                )),
                Presence::Ghost => Some((id, b)),
            })
            .collect();
        Self { bindings }
    }

    /// Checks `discard(Gamma)` (§4.1): every residual binding must be an
    /// shared view or unordered owned value of usage at most affine.
    /// Shared access does not own the viewed value's linear obligation.
    pub fn is_discardable(&self) -> Result<(), GhostError> {
        for (&id, &b) in &self.bindings {
            if !b.discardable() {
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
    /// Shared views may contract or be omitted regardless of the viewed mode.
    /// Only copy owners may contract; only copy/affine owners may be omitted.
    /// Exclusive loans and output slots must go to exactly one side.
    pub fn split(
        &self,
        mut choose: impl FnMut(BindingId, &Binding) -> SplitChoice,
    ) -> Result<(Context, Context), GhostError> {
        let mut left = Context::new();
        let mut right = Context::new();

        for (&id, &b) in &self.bindings {
            let choice = choose(id, &b);
            match choice {
                SplitChoice::Left => {
                    left.bindings.insert(id, b);
                }
                SplitChoice::Right => {
                    right.bindings.insert(id, b);
                }
                SplitChoice::Both
                    if b.kind == PlaceKind::Shr
                        || (b.kind == PlaceKind::Own && b.mode.usage() == Usage::Copy) =>
                {
                    left.bindings.insert(id, b);
                    right.bindings.insert(id, b);
                }
                SplitChoice::Omit if b.discardable() => {}
                SplitChoice::Both | SplitChoice::Omit
                    if matches!(b.kind, PlaceKind::Mut | PlaceKind::Out) =>
                {
                    return Err(GhostError::NonOwnedSplit { id, kind: b.kind });
                }
                SplitChoice::Both => return Err(GhostError::NonlinearDuplicateInSplit { id }),
                SplitChoice::Omit => return Err(GhostError::UnconsumedLinearInSplit { id }),
            }
        }

        Ok((left, right))
    }

    /// Enforces `[T-Abs]` closure-capture discipline (§4.2): closures capture
    /// only owned bindings (no `Shr`, `Mut`, or `Out` places), and each
    /// captured binding must satisfy `m_z <= m_fn`.
    pub fn check_closure_capture(
        &self,
        fn_mode: ModeVec,
        presence: Presence,
    ) -> Result<(), GhostError> {
        for (&id, &b) in &self.bindings {
            if b.kind != PlaceKind::Own {
                return Err(GhostError::BorrowedOrOutCapturedInClosure { id, kind: b.kind });
            }
            if b.presence != presence {
                return Err(GhostError::InvalidOwnedUse { id });
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

    /// Projects runtime context only after ghost obligations are discharged.
    /// This local check is not a proof of program erasure or ghost termination.
    pub fn erase_ghosts(&self) -> Result<Self, GhostError> {
        for (&id, &b) in &self.bindings {
            if b.presence == Presence::Ghost && !b.discardable() {
                return Err(GhostError::NonDiscardableResidual {
                    id,
                    mode: b.mode,
                    kind: b.kind,
                });
            }
        }
        let bindings = self
            .bindings
            .iter()
            .filter(|(_, b)| b.presence == Presence::Runtime)
            .map(|(&id, &b)| (id, b))
            .collect();
        Ok(Self { bindings })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode_vec::{Contention, Order, Portability};

    #[test]
    fn permissions_and_order_apply_at_both_presences() {
        let id = BindingId(0);
        for mode in (0..32).filter_map(ModeVec::from_bits) {
            for presence in [Presence::Ghost, Presence::Runtime] {
                for kind in [
                    PlaceKind::Own,
                    PlaceKind::Shr,
                    PlaceKind::Mut,
                    PlaceKind::Out,
                ] {
                    let mut ctx = Context::new();
                    let inserted = ctx.insert(
                        id,
                        Binding {
                            mode,
                            presence,
                            kind,
                        },
                    );
                    if kind == PlaceKind::Own && mode.is_lifo() {
                        assert_eq!(inserted, Err(GhostError::OrderedOwnerRequiresStack { id }));
                        assert!(ctx.bindings().is_empty());
                        continue;
                    }
                    inserted.unwrap();
                    if kind == PlaceKind::Out {
                        for site in [Presence::Ghost, Presence::Runtime] {
                            assert_eq!(
                                ctx.check_use(id, site, RuntimeUseKind::ReturnValue),
                                Err(GhostError::UninitializedObservation { id })
                            );
                        }
                    }
                    let discardable = kind == PlaceKind::Shr
                        || (kind == PlaceKind::Own && mode.usage() <= Usage::Affine);
                    let contractible = kind == PlaceKind::Shr
                        || (kind == PlaceKind::Own && mode.usage() == Usage::Copy);
                    assert_eq!(ctx.is_discardable().is_ok(), discardable);
                    assert_eq!(ctx.split(|_, _| SplitChoice::Omit).is_ok(), discardable);
                    assert_eq!(ctx.split(|_, _| SplitChoice::Both).is_ok(), contractible);
                    assert!(ctx.split(|_, _| SplitChoice::Left).is_ok());
                    assert!(ctx.split(|_, _| SplitChoice::Right).is_ok());
                    assert_eq!(
                        ctx.erase_ghosts().is_ok(),
                        presence == Presence::Runtime || discardable
                    );
                }
            }
        }
    }

    #[test]
    fn predicate_views_cannot_become_owners_or_observe_uninitialized_slots() {
        let id = BindingId(0);
        let out = BindingId(1);
        let mut ctx = Context::new();
        ctx.insert(id, owned(Usage::Linear, Presence::Runtime))
            .unwrap();
        ctx.insert(
            out,
            Binding {
                kind: PlaceKind::Out,
                ..owned(Usage::Copy, Presence::Runtime)
            },
        )
        .unwrap();
        let mut observations = ctx.scale_zero_for_predicate();
        let view = ctx
            .check_use(id, Presence::Ghost, RuntimeUseKind::ReturnValue)
            .unwrap();
        assert_eq!(view.kind, PlaceKind::Shr);
        assert_eq!(view.mode.usage(), Usage::Linear);
        assert_eq!(observations.bindings()[&id], view);
        assert!(observations.split(|_, _| SplitChoice::Both).is_ok());
        assert!(observations.is_discardable().is_ok());
        assert_eq!(
            observations.take_owned(id, Presence::Ghost),
            Err(GhostError::InvalidOwnedUse { id })
        );
        assert!(
            observations
                .check_closure_capture(ModeVec::TOP, Presence::Ghost)
                .is_err()
        );
        assert!(!observations.bindings().contains_key(&out));
        assert_eq!(
            ctx.check_use(out, Presence::Ghost, RuntimeUseKind::ReturnValue),
            Err(GhostError::UninitializedObservation { id: out })
        );
        assert_eq!(
            observations.check_use(id, Presence::Ghost, RuntimeUseKind::EffectIo),
            Err(GhostError::EffectfulGhostUse { id })
        );
        assert!(
            ctx.check_closure_capture(ModeVec::TOP, Presence::Ghost)
                .is_err()
        );
        assert_eq!(ctx.bindings()[&id].presence, Presence::Runtime);
    }

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
            Usage::Linear
        );
        // Existing `@0` linear witness retains its linear bound inside `@0` proof checking!
        assert_eq!(
            scaled.bindings().get(&slot_perm).unwrap().mode.usage(),
            Usage::Linear
        );

        // Erasure strips `@0` bindings completely and retains `@1` runtime bindings.
        assert!(ctx.erase_ghosts().is_err());
        assert_eq!(
            ctx.take_owned(runtime_tx, Presence::Ghost),
            Err(GhostError::InvalidOwnedUse { id: runtime_tx })
        );
        assert!(ctx.take_owned(slot_perm, Presence::Ghost).is_ok());
        assert!(ctx.take_owned(slot_perm, Presence::Ghost).is_err());
        let runtime_only = ctx.erase_ghosts().unwrap();
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
            ctx.check_closure_capture(ModeVec::NOMINAL_DEFAULT, Presence::Runtime),
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
        assert_eq!(
            ctx.check_closure_capture(linear_fn, Presence::Runtime),
            Ok(())
        );

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
            ctx.check_closure_capture(linear_fn, Presence::Runtime),
            Err(GhostError::BorrowedOrOutCapturedInClosure {
                id: mut_borrow,
                kind: PlaceKind::Mut,
            })
        );
    }
}
