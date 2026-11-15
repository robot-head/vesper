use vesper_core::ast::Type;
use vesper_core::mode_vec::{Contention, ModeVec, Order, Portability, Usage};
use vesper_core::refine_update::{
    LocalRefineBinding, LocalRefineEnv, RefineAtom, RefineClause, RefineType, RelOp, Stability,
    StrongUpdateError, StrongUpdateSig, apply_strong_update, is_refine_subtype,
};

fn affine_uncontended() -> ModeVec {
    ModeVec::new(
        Usage::Affine,
        Portability::Portable,
        Contention::Uncontended,
        Order::Unordered,
    )
}

#[test]
fn m_contend_strip_preserves_immutable_invariants_across_threads() {
    let channel = RefineType::new(
        Type::named("RingChannel"),
        "ch",
        vec![
            RefineClause::immutable(RefineAtom::IntRange {
                var: "capacity".into(),
                lo: 16,
                hi: 4096,
            }),
            RefineClause::immutable(RefineAtom::BitMaskEq {
                var: "align".into(),
                mask: 63,
                expected: 0,
            }),
            RefineClause::mutable(RefineAtom::MeasureCmp {
                measure: "queued_items".into(),
                op: RelOp::Gt,
                value: 0,
            }),
        ],
        affine_uncontended(),
    );

    let mut env = LocalRefineEnv::default();
    env.insert(
        "ch",
        LocalRefineBinding {
            ty: channel,
            aliased: false,
            initialized: true,
        },
    );

    let contended = env.contend_view("ch").unwrap();
    assert_eq!(contended.mode.contention(), Contention::Contended);
    assert_eq!(contended.clauses.len(), 2);
    assert!(
        contended
            .clauses
            .iter()
            .all(|c| c.stability == Stability::Immutable)
    );
}

#[test]
fn strong_update_chains_typestate_steps_and_enforces_precondition() {
    let at_state = |n: i128| {
        RefineType::new(
            Type::named("RegionVec"),
            "xs",
            vec![
                RefineClause::immutable(RefineAtom::IntCmp {
                    var: "cap".into(),
                    op: RelOp::Eq,
                    value: 32,
                }),
                RefineClause::mutable(RefineAtom::MeasureCmp {
                    measure: "len".into(),
                    op: RelOp::Eq,
                    value: n,
                }),
            ],
            affine_uncontended(),
        )
    };

    let ret = RefineType::new(Type::unit(), "_", vec![], ModeVec::BOTTOM);
    let mut env = LocalRefineEnv::default();
    env.insert(
        "xs",
        LocalRefineBinding {
            ty: at_state(0),
            aliased: false,
            initialized: true,
        },
    );

    let push_0_to_1 = StrongUpdateSig {
        tau_in: at_state(0),
        tau_out: at_state(1),
        ret: ret.clone(),
    };
    let push_1_to_2 = StrongUpdateSig {
        tau_in: at_state(1),
        tau_out: at_state(2),
        ret,
    };

    // Wrong state rejected before advancing.
    assert_eq!(
        apply_strong_update(&mut env, &[], "xs", &push_1_to_2),
        Err(StrongUpdateError::PreconditionNotSatisfied { var: "xs".into() })
    );

    // Valid chain: len == 0 ~> len == 1 ~> len == 2.
    assert!(apply_strong_update(&mut env, &[], "xs", &push_0_to_1).is_ok());
    assert!(apply_strong_update(&mut env, &[], "xs", &push_1_to_2).is_ok());
    assert!(is_refine_subtype(
        &[],
        &env.get("xs").unwrap().ty,
        &at_state(2)
    ));
}

#[test]
fn strong_update_rejects_contended_and_missing_bindings() {
    let ty = RefineType::new(Type::named("Counter"), "c", vec![], affine_uncontended());
    let contended_ty = ty.contend();
    let mut env = LocalRefineEnv::default();
    env.insert(
        "shared",
        LocalRefineBinding {
            ty: contended_ty.clone(),
            aliased: false,
            initialized: true,
        },
    );
    env.insert(
        "uninit",
        LocalRefineBinding {
            ty: ty.clone(),
            aliased: false,
            initialized: false,
        },
    );

    let sig = StrongUpdateSig {
        tau_in: ty.clone(),
        tau_out: ty,
        ret: RefineType::new(Type::unit(), "_", vec![], ModeVec::BOTTOM),
    };

    assert_eq!(
        apply_strong_update(&mut env, &[], "shared", &sig),
        Err(StrongUpdateError::AliasedOrContendedTarget {
            var: "shared".into()
        })
    );
    assert_eq!(
        apply_strong_update(&mut env, &[], "uninit", &sig),
        Err(StrongUpdateError::UnknownOrUninitializedBinding {
            var: "uninit".into()
        })
    );
}
