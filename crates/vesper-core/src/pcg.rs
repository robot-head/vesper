//! Lexical effect-row resumption guarantees and control-flow Place Capability
//! Graph (PCG) state transitions for manuscript §4.1–§4.2, §5.1–§5.2, §7.3,
//! and §7.5.
//!
//! This module models the value/state transitions (`[T-Let]` Resumption Frame
//! Condition, `[T-Match]` branch join consistency, and `discard(Gamma)` return
//! exhaustion) independently of the compiler's `World` and CFG lowering; full
//! source/HIR integration remains in higher compiler stages.

use crate::ast::Mode;
use std::collections::BTreeMap;

/// Default hard cap on tracked locals per [`PcgState`] to bound memory/scans.
pub const DEFAULT_MAX_LOCALS: usize = 4096;

/// Continuation resumption contract `q in {affine, linear}` on an effect
/// declaration (§5.1, §7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ResumptionMode {
    /// Handler may abort without resuming (e.g., `effect Fail[E]`).
    Affine = 1,
    /// Handler must resume `k` exactly once (e.g., `linear effect Storage`).
    Linear = 2,
}

impl ResumptionMode {
    #[inline]
    pub const fn as_mode(self) -> Mode {
        match self {
            Self::Affine => Mode::Affine,
            Self::Linear => Mode::Linear,
        }
    }
}

/// Stable identifier of an effect declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EffectId(pub u32);

/// One lexical effect instance `l : E^q` inside an effect row (§7.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EffectInst {
    pub cap_label: Option<String>,
    pub effect_id: EffectId,
    pub resumption: ResumptionMode,
}

/// Lexical effect row `eff [l1: E1^q1, ..., ln: En^qn | tail_var]` (§4.1, §7.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectRow {
    pub instances: Vec<EffectInst>,
    pub tail_var: Option<u32>,
}

impl EffectRow {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_instances(instances: Vec<EffectInst>) -> Self {
        Self {
            instances,
            tail_var: None,
        }
    }

    /// Computes `res(epsilon) = meet_i q_i`, folded from `tail_bound`
    /// (`res(empty) = Mode::Linear`, §4.1, §7.3).
    pub fn resumption_guarantee(&self, tail_bound: Mode) -> Mode {
        self.instances
            .iter()
            .fold(tail_bound, |acc, inst| acc.meet(inst.resumption.as_mode()))
    }

    /// Sequentially composes two effect rows (`epsilon_1 o epsilon_2`).
    pub fn compose(&self, other: &Self) -> Self {
        let mut instances = Vec::with_capacity(self.instances.len() + other.instances.len());
        instances.extend(self.instances.iter().cloned());
        instances.extend(other.instances.iter().cloned());
        Self {
            instances,
            tail_var: self.tail_var.or(other.tail_var),
        }
    }
}

/// Compiler-assigned local variable identifier inside a CFG frame (§7.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(pub u32);

/// Basic block identifier for branch join diagnostics (§7.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u32);

/// Capability state of a tracked local in [`PcgState`] (§7.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalCapability {
    /// Initialized owned binding with active usage mode.
    LiveOwned { mode: Mode },
    /// Uninitialized write-only `out` destination slot.
    UninitializedOutSlot,
    /// Explicitly consumed / moved (`sink`).
    Consumed,
}

/// Typed PCG control-flow or resumption-frame error (§5.2, §7.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PcgError {
    DuplicateLocal {
        local: LocalId,
    },
    UnknownLocal {
        local: LocalId,
    },
    AlreadyConsumed {
        local: LocalId,
    },
    OutSlotNotInitialized {
        local: LocalId,
    },
    AlreadyInitializedOutSlot {
        local: LocalId,
    },
    UnconsumedLinearAtScopeExit {
        local: LocalId,
    },
    UninitializedOutSlotAtReturn {
        local: LocalId,
    },
    InconsistentLinearConsumptionAtJoin {
        block: BlockId,
        local: LocalId,
    },
    InconsistentOutSlotAtJoin {
        block: BlockId,
        local: LocalId,
    },
    ResumptionFrameViolation {
        live_linear: LocalId,
        required_max: Mode,
        actual: Mode,
    },
    CapacityExceeded {
        max: usize,
    },
}

/// Returns `true` when `mode` permits implicit synchronous `discard(Gamma)` (§4.1).
#[inline]
pub fn is_discardable(mode: Mode) -> bool {
    mode <= Mode::Affine
}

/// Per-path Place Capability Graph capability table (§7.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcgState {
    locals: BTreeMap<LocalId, LocalCapability>,
    max_locals: usize,
}

impl Default for PcgState {
    fn default() -> Self {
        Self::with_max_locals(DEFAULT_MAX_LOCALS)
    }
}

impl PcgState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_locals(max_locals: usize) -> Self {
        Self {
            locals: BTreeMap::new(),
            max_locals,
        }
    }

    pub fn locals(&self) -> &BTreeMap<LocalId, LocalCapability> {
        &self.locals
    }

    /// Introduces a newly bound initialized owned local. Errors leave state unchanged.
    pub fn assign_owned(&mut self, local: LocalId, mode: Mode) -> Result<(), PcgError> {
        if self.locals.contains_key(&local) {
            return Err(PcgError::DuplicateLocal { local });
        }
        if self.locals.len() >= self.max_locals {
            return Err(PcgError::CapacityExceeded {
                max: self.max_locals,
            });
        }
        self.locals
            .insert(local, LocalCapability::LiveOwned { mode });
        Ok(())
    }

    /// Introduces an uninitialized write-only `out` destination slot (§4.4).
    pub fn declare_out_slot(&mut self, local: LocalId) -> Result<(), PcgError> {
        if self.locals.contains_key(&local) {
            return Err(PcgError::DuplicateLocal { local });
        }
        if self.locals.len() >= self.max_locals {
            return Err(PcgError::CapacityExceeded {
                max: self.max_locals,
            });
        }
        self.locals
            .insert(local, LocalCapability::UninitializedOutSlot);
        Ok(())
    }

    /// Marks an `UninitializedOutSlot` as fully initialized with `mode` (§4.4).
    pub fn complete_out_slot(&mut self, local: LocalId, mode: Mode) -> Result<(), PcgError> {
        match self.locals.get(&local).copied() {
            Some(LocalCapability::UninitializedOutSlot) => {
                self.locals
                    .insert(local, LocalCapability::LiveOwned { mode });
                Ok(())
            }
            Some(LocalCapability::LiveOwned { .. } | LocalCapability::Consumed) => {
                Err(PcgError::AlreadyInitializedOutSlot { local })
            }
            None => Err(PcgError::UnknownLocal { local }),
        }
    }

    /// Consumes a binding passed by `sink` (`[Split-Copy]` leaves `Mode::Copy`
    /// live for further uses; `Affine` and `Linear` transition to `Consumed`).
    pub fn consume_sink(&mut self, local: LocalId) -> Result<(), PcgError> {
        match self.locals.get(&local).copied() {
            Some(LocalCapability::LiveOwned { mode: Mode::Copy }) => Ok(()),
            Some(LocalCapability::LiveOwned { .. }) => {
                self.locals.insert(local, LocalCapability::Consumed);
                Ok(())
            }
            Some(LocalCapability::Consumed) => Err(PcgError::AlreadyConsumed { local }),
            Some(LocalCapability::UninitializedOutSlot) => {
                Err(PcgError::OutSlotNotInitialized { local })
            }
            None => Err(PcgError::UnknownLocal { local }),
        }
    }

    /// Checks the `[T-Let]` Resumption Frame Condition (`forall z in Gamma_2,
    /// u(m_z) <= res(epsilon_1)`, §4.2, §5.2, §7.5).
    ///
    /// Unfilled `UninitializedOutSlot` bindings hold uninitialized storage
    /// rather than live obligations, so they are safely ignored across
    /// abortable effect unwinds (§4.4, §7.5).
    pub fn check_resumption_frame(
        &self,
        effects: &EffectRow,
        tail_bound: Mode,
    ) -> Result<(), PcgError> {
        let res_guarantee = effects.resumption_guarantee(tail_bound);
        for (&local, &cap) in &self.locals {
            if let LocalCapability::LiveOwned { mode } = cap
                && mode > res_guarantee
            {
                return Err(PcgError::ResumptionFrameViolation {
                    live_linear: local,
                    required_max: res_guarantee,
                    actual: mode,
                });
            }
        }
        Ok(())
    }

    /// Enforces uniform linear consumption and `out`-slot consistency across CFG
    /// join points (`[T-Match]` / `if-else`, §4.2, §7.5).
    pub fn join_at_block(&self, other: &Self, block: BlockId) -> Result<Self, PcgError> {
        let max_locals = self.max_locals.min(other.max_locals);
        let mut merged = BTreeMap::new();

        for (&local, &lhs) in &self.locals {
            match other.locals.get(&local).copied() {
                None => match lhs {
                    LocalCapability::LiveOwned {
                        mode: Mode::Linear, ..
                    } => {
                        return Err(PcgError::InconsistentLinearConsumptionAtJoin { block, local });
                    }
                    LocalCapability::UninitializedOutSlot => {
                        return Err(PcgError::InconsistentOutSlotAtJoin { block, local });
                    }
                    LocalCapability::LiveOwned { .. } | LocalCapability::Consumed => {}
                },
                Some(rhs) => {
                    let cap = join_capabilities(lhs, rhs, block, local)?;
                    merged.insert(local, cap);
                }
            }
        }

        for (&local, &rhs) in &other.locals {
            if !self.locals.contains_key(&local) {
                match rhs {
                    LocalCapability::LiveOwned {
                        mode: Mode::Linear, ..
                    } => {
                        return Err(PcgError::InconsistentLinearConsumptionAtJoin { block, local });
                    }
                    LocalCapability::UninitializedOutSlot => {
                        return Err(PcgError::InconsistentOutSlotAtJoin { block, local });
                    }
                    LocalCapability::LiveOwned { .. } | LocalCapability::Consumed => {}
                }
            }
        }

        if merged.len() > max_locals {
            return Err(PcgError::CapacityExceeded { max: max_locals });
        }

        Ok(Self {
            locals: merged,
            max_locals,
        })
    }

    /// Enforces `discard(Gamma)` and complete `OutSlot` initialization at
    /// function `Return` (§4.1, §4.4, §7.5).
    pub fn check_return_exhaustion(&self) -> Result<(), PcgError> {
        for (&local, &cap) in &self.locals {
            match cap {
                LocalCapability::LiveOwned { mode } if !is_discardable(mode) => {
                    return Err(PcgError::UnconsumedLinearAtScopeExit { local });
                }
                LocalCapability::UninitializedOutSlot => {
                    return Err(PcgError::UninitializedOutSlotAtReturn { local });
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn join_capabilities(
    lhs: LocalCapability,
    rhs: LocalCapability,
    block: BlockId,
    local: LocalId,
) -> Result<LocalCapability, PcgError> {
    match (lhs, rhs) {
        (LocalCapability::Consumed, LocalCapability::Consumed) => Ok(LocalCapability::Consumed),
        (LocalCapability::LiveOwned { mode: m1 }, LocalCapability::LiveOwned { mode: m2 }) => {
            Ok(LocalCapability::LiveOwned { mode: m1.join(m2) })
        }
        (LocalCapability::LiveOwned { mode: Mode::Linear }, LocalCapability::Consumed)
        | (LocalCapability::Consumed, LocalCapability::LiveOwned { mode: Mode::Linear }) => {
            Err(PcgError::InconsistentLinearConsumptionAtJoin { block, local })
        }
        (LocalCapability::LiveOwned { mode }, LocalCapability::Consumed)
        | (LocalCapability::Consumed, LocalCapability::LiveOwned { mode })
            if is_discardable(mode) =>
        {
            Ok(LocalCapability::Consumed)
        }
        (LocalCapability::UninitializedOutSlot, LocalCapability::UninitializedOutSlot) => {
            Ok(LocalCapability::UninitializedOutSlot)
        }
        (LocalCapability::UninitializedOutSlot, _) | (_, LocalCapability::UninitializedOutSlot) => {
            Err(PcgError::InconsistentOutSlotAtJoin { block, local })
        }
        // Covered by the arms above (`Linear` + `Consumed` or discardable + `Consumed`).
        (LocalCapability::LiveOwned { .. }, LocalCapability::Consumed)
        | (LocalCapability::Consumed, LocalCapability::LiveOwned { .. }) => {
            unreachable!("all Mode variants are handled above")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage_effect() -> EffectInst {
        EffectInst {
            cap_label: Some("io".to_string()),
            effect_id: EffectId(1),
            resumption: ResumptionMode::Linear,
        }
    }

    fn fail_effect() -> EffectInst {
        EffectInst {
            cap_label: Some("fail".to_string()),
            effect_id: EffectId(2),
            resumption: ResumptionMode::Affine,
        }
    }

    #[test]
    fn effect_row_resumption_guarantee_folds_meet_and_empty_is_linear() {
        assert_eq!(
            EffectRow::empty().resumption_guarantee(Mode::Linear),
            Mode::Linear
        );

        let storage_only = EffectRow::from_instances(vec![storage_effect()]);
        assert_eq!(
            storage_only.resumption_guarantee(Mode::Linear),
            Mode::Linear
        );

        let storage_and_fail =
            storage_only.compose(&EffectRow::from_instances(vec![fail_effect()]));
        assert_eq!(
            storage_and_fail.resumption_guarantee(Mode::Linear),
            Mode::Affine
        );

        let open_affine_tail = EffectRow {
            instances: vec![storage_effect()],
            tail_var: Some(0),
        };
        assert_eq!(
            open_affine_tail.resumption_guarantee(Mode::Affine),
            Mode::Affine
        );
    }

    #[test]
    fn worked_derivation_5_2_rejects_fail_across_live_linear_tx_and_accepts_uniform_branches() {
        let tx = LocalId(1);
        let batch = LocalId(2);
        let config = LocalId(3);

        let mut base = PcgState::new();
        base.assign_owned(tx, Mode::Linear).unwrap();
        base.assign_owned(batch, Mode::Affine).unwrap();
        base.assign_owned(config, Mode::Copy).unwrap();

        // Linear-only Storage suspension permits live `tx: Linear` and `batch: Affine`.
        let storage_row = EffectRow::from_instances(vec![storage_effect()]);
        assert_eq!(
            base.check_resumption_frame(&storage_row, Mode::Linear),
            Ok(())
        );

        // Performing abortable `Fail` (`?`) while `tx: Linear` is still live fails `[T-Let]`.
        let fail_row = EffectRow::from_instances(vec![fail_effect()]);
        assert_eq!(
            base.check_resumption_frame(&fail_row, Mode::Linear),
            Err(PcgError::ResumptionFrameViolation {
                live_linear: tx,
                required_max: Mode::Affine,
                actual: Mode::Linear,
            })
        );

        // Consuming `tx` on Ok arm and forgetting `tx` on Err arm fails `[T-Match]` join.
        let mut ok_branch = base.clone();
        ok_branch.consume_sink(batch).unwrap();
        ok_branch.consume_sink(tx).unwrap();
        // `Copy` config remains reusable after `sink`.
        ok_branch.consume_sink(config).unwrap();

        let err_branch_leaky = base.clone();
        assert_eq!(
            ok_branch.join_at_block(&err_branch_leaky, BlockId(10)),
            Err(PcgError::InconsistentLinearConsumptionAtJoin {
                block: BlockId(10),
                local: tx,
            })
        );

        // Consuming `tx` and `batch` on `Err` branch joins cleanly and discharges `Return`.
        let mut err_branch = base.clone();
        err_branch.consume_sink(tx).unwrap();
        // Leave `batch` unconsumed on Err branch to test `Affine` discard join -> `Consumed`.
        let joined = ok_branch.join_at_block(&err_branch, BlockId(11)).unwrap();
        assert_eq!(joined.locals().get(&tx), Some(&LocalCapability::Consumed));
        assert_eq!(
            joined.locals().get(&batch),
            Some(&LocalCapability::Consumed)
        );
        assert_eq!(
            joined.locals().get(&config),
            Some(&LocalCapability::LiveOwned { mode: Mode::Copy })
        );
        assert_eq!(
            joined.check_resumption_frame(&fail_row, Mode::Linear),
            Ok(())
        );
        assert_eq!(joined.check_return_exhaustion(), Ok(()));
    }

    #[test]
    fn uninit_out_slot_allows_abortable_effects_until_initialized_and_requires_init_at_return() {
        let out_buf = LocalId(1);
        let mut state = PcgState::new();
        state.declare_out_slot(out_buf).unwrap();

        // Unfilled `OutSlot` survives abortable effects safely (§4.4, §7.5).
        let fail_row = EffectRow::from_instances(vec![fail_effect()]);
        assert_eq!(
            state.check_resumption_frame(&fail_row, Mode::Linear),
            Ok(())
        );

        // Normal return with unfilled `OutSlot` is rejected (`UninitializedOutSlotAtReturn`).
        assert_eq!(
            state.check_return_exhaustion(),
            Err(PcgError::UninitializedOutSlotAtReturn { local: out_buf })
        );

        // Cannot `sink`-consume before initialization.
        assert_eq!(
            state.consume_sink(out_buf),
            Err(PcgError::OutSlotNotInitialized { local: out_buf })
        );

        // Joining one initialized arm with one uninitialized arm fails.
        let mut init_arm = state.clone();
        init_arm.complete_out_slot(out_buf, Mode::Linear).unwrap();
        assert_eq!(
            init_arm.join_at_block(&state, BlockId(3)),
            Err(PcgError::InconsistentOutSlotAtJoin {
                block: BlockId(3),
                local: out_buf,
            })
        );

        // Once initialized as Affine/Copy (or Linear then consumed), Return succeeds.
        let mut affine_out = state.clone();
        affine_out.complete_out_slot(out_buf, Mode::Affine).unwrap();
        assert_eq!(affine_out.check_return_exhaustion(), Ok(()));

        let mut linear_state = state;
        linear_state
            .complete_out_slot(out_buf, Mode::Linear)
            .unwrap();
        assert_eq!(
            linear_state.check_return_exhaustion(),
            Err(PcgError::UnconsumedLinearAtScopeExit { local: out_buf })
        );
        linear_state.consume_sink(out_buf).unwrap();
        assert_eq!(linear_state.check_return_exhaustion(), Ok(()));
    }
}
