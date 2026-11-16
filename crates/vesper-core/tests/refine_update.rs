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

#[test]
fn bitmask_entailment_preserves_full_expected_value() {
    let x_zero = RefineType::new(
        Type::named("u64"),
        "x",
        vec![RefineClause::immutable(RefineAtom::IntCmp {
            var: "x".into(),
            op: RelOp::Eq,
            value: 0,
        })],
        affine_uncontended(),
    );
    let impossible_target = RefineType::new(
        Type::named("u64"),
        "x",
        vec![RefineClause::immutable(RefineAtom::BitMaskEq {
            var: "x".into(),
            mask: 1,
            expected: 2,
        })],
        affine_uncontended(),
    );
    assert!(!is_refine_subtype(&[], &x_zero, &impossible_target));

    let unknown = RefineType::new(Type::named("u64"), "x", vec![], affine_uncontended());
    let masked_known = RefineType::new(
        Type::named("u64"),
        "x",
        vec![RefineClause::immutable(RefineAtom::BitMaskEq {
            var: "x".into(),
            mask: 1,
            expected: 0,
        })],
        affine_uncontended(),
    );
    assert!(!is_refine_subtype(&[], &masked_known, &impossible_target));
    assert!(!is_refine_subtype(&[], &unknown, &impossible_target));

    // An assumption `(x & 1) == 2` is inconsistent and discharges via ex falso.
    assert!(is_refine_subtype(&[], &impossible_target, &x_zero));

    // Strong update must reject impossible bitmask preconditions.
    let mut env = LocalRefineEnv::default();
    env.insert(
        "x",
        LocalRefineBinding {
            ty: x_zero.clone(),
            aliased: false,
            initialized: true,
        },
    );
    let sig = StrongUpdateSig {
        tau_in: impossible_target,
        tau_out: x_zero,
        ret: RefineType::new(Type::unit(), "_", vec![], ModeVec::BOTTOM),
    };
    assert_eq!(
        apply_strong_update(&mut env, &[], "x", &sig),
        Err(StrongUpdateError::PreconditionNotSatisfied { var: "x".into() })
    );
}

#[test]
fn sub_refine_preserves_predicate_stability() {
    let mut_len_zero = RefineType::new(
        Type::named("RegionVec"),
        "v",
        vec![RefineClause::mutable(RefineAtom::MeasureCmp {
            measure: "len".into(),
            op: RelOp::Eq,
            value: 0,
        })],
        affine_uncontended(),
    );
    let imm_len_zero = RefineType::new(
        Type::named("RegionVec"),
        "v",
        vec![RefineClause::immutable(RefineAtom::MeasureCmp {
            measure: "len".into(),
            op: RelOp::Eq,
            value: 0,
        })],
        affine_uncontended(),
    );
    assert!(!is_refine_subtype(&[], &mut_len_zero, &imm_len_zero));
    assert!(is_refine_subtype(&[], &imm_len_zero, &mut_len_zero));

    // Derived interval facts from mutable clauses cannot prove immutable clauses.
    let mut_x_10 = RefineType::new(
        Type::named("i64"),
        "x",
        vec![RefineClause::mutable(RefineAtom::IntCmp {
            var: "x".into(),
            op: RelOp::Eq,
            value: 10,
        })],
        affine_uncontended(),
    );
    let imm_x_ge_5 = RefineType::new(
        Type::named("i64"),
        "x",
        vec![RefineClause::immutable(RefineAtom::IntCmp {
            var: "x".into(),
            op: RelOp::Ge,
            value: 5,
        })],
        affine_uncontended(),
    );
    assert!(!is_refine_subtype(&[], &mut_x_10, &imm_x_ge_5));
}

#[test]
fn binder_normalization_is_capture_free() {
    let unconstrained_x = RefineType::new(Type::named("i64"), "x", vec![], affine_uncontended());
    let target_x_zero = RefineType::new(
        Type::named("i64"),
        "x",
        vec![RefineClause::immutable(RefineAtom::IntCmp {
            var: "x".into(),
            op: RelOp::Eq,
            value: 0,
        })],
        affine_uncontended(),
    );
    let phi_dunder_v = [RefineClause::immutable(RefineAtom::IntCmp {
        var: "__v".into(),
        op: RelOp::Eq,
        value: 0,
    })];
    assert!(!is_refine_subtype(
        &phi_dunder_v,
        &unconstrained_x,
        &target_x_zero
    ));

    // Path condition mentioning free `x` must not be captured by lhs binder `x`.
    let phi_x = [RefineClause::immutable(RefineAtom::IntCmp {
        var: "x".into(),
        op: RelOp::Eq,
        value: 0,
    })];
    let target_y_zero = RefineType::new(
        Type::named("i64"),
        "y",
        vec![RefineClause::immutable(RefineAtom::IntCmp {
            var: "y".into(),
            op: RelOp::Eq,
            value: 0,
        })],
        affine_uncontended(),
    );
    assert!(!is_refine_subtype(&phi_x, &unconstrained_x, &target_y_zero));
}

#[test]
fn int_bound_extremes_do_not_overflow_into_unsat() {
    let lt_min = RefineType::new(
        Type::named("Int"),
        "x",
        vec![RefineClause::immutable(RefineAtom::IntCmp {
            var: "x".into(),
            op: RelOp::Lt,
            value: i128::MIN,
        })],
        affine_uncontended(),
    );
    let gt_max = RefineType::new(
        Type::named("Int"),
        "x",
        vec![RefineClause::immutable(RefineAtom::IntCmp {
            var: "x".into(),
            op: RelOp::Gt,
            value: i128::MAX,
        })],
        affine_uncontended(),
    );
    let eq_zero = RefineType::new(
        Type::named("Int"),
        "x",
        vec![RefineClause::immutable(RefineAtom::IntCmp {
            var: "x".into(),
            op: RelOp::Eq,
            value: 0,
        })],
        affine_uncontended(),
    );
    let lt_zero = RefineType::new(
        Type::named("Int"),
        "x",
        vec![RefineClause::immutable(RefineAtom::IntCmp {
            var: "x".into(),
            op: RelOp::Lt,
            value: 0,
        })],
        affine_uncontended(),
    );

    assert!(!is_refine_subtype(&[], &lt_min, &eq_zero));
    assert!(!is_refine_subtype(&[], &gt_max, &eq_zero));
    assert!(is_refine_subtype(&[], &lt_min, &lt_zero));

    // Combining `x < i128::MIN` with `x >= i128::MIN` is genuinely contradictory.
    let contra = RefineType::new(
        Type::named("Int"),
        "x",
        vec![
            RefineClause::immutable(RefineAtom::IntCmp {
                var: "x".into(),
                op: RelOp::Lt,
                value: i128::MIN,
            }),
            RefineClause::immutable(RefineAtom::IntCmp {
                var: "x".into(),
                op: RelOp::Ge,
                value: i128::MIN,
            }),
        ],
        affine_uncontended(),
    );
    assert!(is_refine_subtype(&[], &contra, &eq_zero));
}
