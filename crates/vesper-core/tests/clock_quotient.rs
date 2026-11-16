use vesper_core::clock_quotient::*;
use vesper_core::mode_cross::{Presence, usage_mode};
use vesper_core::mode_vec::{ModeVec, Usage};
use vesper_core::pcg::{EffectId, EffectInst, EffectRow, ResumptionMode};

#[test]
fn linear_payload_cannot_be_hidden_in_a_later_tick() {
    assert_eq!(
        LaterType::delay(ClockId(0), usage_mode(Usage::Linear)),
        Err(ClockQuotientError::LinearDelayedPayload)
    );
    assert!(LaterType::delay(ClockId(0), usage_mode(Usage::Affine)).is_ok());
}

#[test]
fn same_clock_is_insufficient_for_ghost_elimination() {
    let delayed = LaterType::delay(ClockId(7), ModeVec::BOTTOM).unwrap();
    assert_eq!(
        delayed.force_tick(ClockId(7), Presence::Ghost),
        Err(ClockQuotientError::GhostForceRequiresClockQuantification)
    );
    assert_eq!(
        delayed.force_tick(ClockId(7), Presence::Runtime),
        Ok(ModeVec::BOTTOM)
    );
}

#[test]
fn acyclic_graph_does_not_excuse_noncopy_or_effectful_recursive_steps() {
    for usage in [Usage::Affine, Usage::Linear] {
        assert_eq!(
            verify_guarded_recursion(
                ClockId(0),
                usage_mode(usage),
                &EffectRow::empty(),
                &[],
                &[],
                Limits::default()
            ),
            Err(ClockQuotientError::NoncopyGuardedCapture)
        );
    }
    let effectful = EffectRow::from_instances(vec![EffectInst {
        cap_label: None,
        effect_id: EffectId(0),
        resumption: ResumptionMode::Linear,
    }]);
    let open = EffectRow {
        instances: vec![],
        tail_var: Some(0),
    };
    for effects in [effectful, open] {
        assert_eq!(
            verify_guarded_recursion(
                ClockId(0),
                ModeVec::BOTTOM,
                &effects,
                &[],
                &[],
                Limits::default()
            ),
            Err(ClockQuotientError::EffectfulGuardedStep)
        );
    }
}

fn declaration() -> QuotientDecl {
    QuotientDecl {
        quotient_mode: ModeVec::BOTTOM,
        repr_mode: ModeVec::BOTTOM,
        equiv: EquivId(9),
        public_ops: vec![OpId(1), OpId(2)],
        op_proofs: vec![
            RespectsProof {
                op: OpId(1),
                equiv: EquivId(9),
                proved_respects: true,
            },
            RespectsProof {
                op: OpId(2),
                equiv: EquivId(9),
                proved_respects: true,
            },
        ],
    }
}

#[test]
fn every_export_requires_its_own_congruence_status() {
    let mut decl = declaration();
    assert_eq!(verify_quotient_decl(&decl, Limits::default()), Ok(()));
    decl.op_proofs.pop();
    assert_eq!(
        verify_quotient_decl(&decl, Limits::default()),
        Err(ClockQuotientError::UnverifiedQuotientOperation { op: OpId(2) })
    );
    decl.op_proofs.clear();
    assert_eq!(
        verify_quotient_decl(&decl, Limits::default()),
        Err(ClockQuotientError::UnverifiedQuotientOperation { op: OpId(1) })
    );
}

#[test]
fn duplicate_and_unrelated_proofs_cannot_satisfy_export_coverage() {
    let mut decl = declaration();
    decl.op_proofs[1] = decl.op_proofs[0];
    assert_eq!(
        verify_quotient_decl(&decl, Limits::default()),
        Err(ClockQuotientError::DuplicateRespectsProof { op: OpId(1) })
    );
    decl.op_proofs[1].op = OpId(3);
    assert_eq!(
        verify_quotient_decl(&decl, Limits::default()),
        Err(ClockQuotientError::UnknownQuotientOperation { op: OpId(3) })
    );
    let mut decl = declaration();
    decl.public_ops[1] = OpId(1);
    assert_eq!(
        verify_quotient_decl(&decl, Limits::default()),
        Err(ClockQuotientError::DuplicatePublicOperation { op: OpId(1) })
    );
}

#[test]
fn export_inventory_is_bounded_even_when_no_proofs_are_supplied() {
    let mut decl = declaration();
    decl.op_proofs.clear();
    assert_eq!(
        verify_quotient_decl(
            &decl,
            Limits {
                max_ops: 1,
                ..Limits::default()
            }
        ),
        Err(ClockQuotientError::OpLimit { max: 1 })
    );
}
