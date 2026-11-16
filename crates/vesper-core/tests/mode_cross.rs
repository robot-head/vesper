use vesper_core::mode_cross::{
    check_var_use, infer_modal_kind, mode_cross, sub_mode, usage_mode, AdtFieldKind, KindEnv,
    KindType, ModeJudgmentError, PlaceOwnership, Presence, PrimKind, VarContextEntry,
};
use vesper_core::mode_vec::{Contention, ModeVec, Order, Portability, Usage};

fn make_option(payload: KindType) -> KindType {
    KindType::Adt {
        name: "Option".into(),
        decl_mode: ModeVec::BOTTOM,
        params: vec!["T".into()],
        fields: vec![AdtFieldKind {
            name: "value".into(),
            ty: KindType::Var("T".into()),
            override_mode: None,
        }],
        type_args: vec![payload],
    }
}

#[test]
fn generic_option_and_mode_crossing_follow_sec_3_2_and_4_2() {
    let mut delta = KindEnv::default();
    delta.bind_var("PoolSlice", usage_mode(Usage::Affine));
    delta.bind_var(
        "IrqGuard",
        ModeVec::new(
            Usage::Linear,
            Portability::Pinned,
            Contention::Uncontended,
            Order::Lifo,
        ),
    );

    let opt_u64 = make_option(KindType::Prim(PrimKind::U64));
    let opt_affine = make_option(KindType::Var("PoolSlice".into()));
    let opt_linear_lifo = make_option(KindType::Var("IrqGuard".into()));

    assert_eq!(
        infer_modal_kind(&delta, &opt_u64),
        Ok(usage_mode(Usage::Copy))
    );
    assert_eq!(
        infer_modal_kind(&delta, &opt_affine),
        Ok(usage_mode(Usage::Affine))
    );
    assert_eq!(
        infer_modal_kind(&delta, &opt_linear_lifo),
        Err(ModeJudgmentError::OrderedField {
            adt: "Option".into(),
            field: "value".into()
        })
    );

    // [T-Cross]: generic result at TOP crosses back to copy on Option[u64], affine on Option[PoolSlice].
    assert_eq!(
        mode_cross(&delta, &opt_u64, ModeVec::TOP),
        Ok(usage_mode(Usage::Copy))
    );
    assert_eq!(
        mode_cross(&delta, &opt_affine, ModeVec::TOP),
        Ok(usage_mode(Usage::Affine))
    );
    // Crossing cannot weaken below the intrinsic kind (pinned linear cannot cross to portable affine).
    assert_eq!(
        mode_cross(
            &delta,
            &KindType::Var("IrqGuard".into()),
            usage_mode(Usage::Affine)
        ),
        Err(ModeJudgmentError::ModeOrderViolation {
            actual: ModeVec::new(
                Usage::Linear,
                Portability::Pinned,
                Contention::Uncontended,
                Order::Lifo,
            ),
            required: usage_mode(Usage::Affine),
        })
    );
}

#[test]
fn fn_kinding_and_sub_mode_weakening() {
    let delta = KindEnv::default();
    let copy_fn = KindType::Fn {
        fn_mode: usage_mode(Usage::Copy),
    };
    let linear_fn = KindType::Fn {
        fn_mode: usage_mode(Usage::Linear),
    };
    assert_eq!(
        infer_modal_kind(&delta, &copy_fn),
        Ok(usage_mode(Usage::Copy))
    );
    assert_eq!(
        infer_modal_kind(&delta, &linear_fn),
        Ok(usage_mode(Usage::Linear))
    );
    assert_eq!(
        sub_mode(usage_mode(Usage::Copy), usage_mode(Usage::Linear)),
        Ok(usage_mode(Usage::Linear))
    );
    assert!(sub_mode(usage_mode(Usage::Linear), usage_mode(Usage::Affine)).is_err());
}

#[test]
fn t_var_rejects_ghost_in_runtime_and_unfilled_out_slot() {
    let gamma_ghost = vec![VarContextEntry {
        name: "perm".into(),
        presence: Presence::Ghost,
        mode: usage_mode(Usage::Linear),
        ownership: PlaceOwnership::Own,
        initialized: true,
    }];
    assert_eq!(
        check_var_use(&gamma_ghost, Presence::Runtime, "perm"),
        Err(ModeJudgmentError::PresenceViolation {
            name: "perm".into(),
            ctx_presence: Presence::Runtime,
            binding_presence: Presence::Ghost,
        })
    );
    assert_eq!(
        check_var_use(&gamma_ghost, Presence::Ghost, "perm"),
        Ok((Presence::Ghost, usage_mode(Usage::Linear)))
    );

    let gamma_out = vec![
        VarContextEntry {
            name: "x".into(),
            presence: Presence::Runtime,
            mode: usage_mode(Usage::Copy),
            ownership: PlaceOwnership::Own,
            initialized: true,
        },
        VarContextEntry {
            name: "dst".into(),
            presence: Presence::Runtime,
            mode: usage_mode(Usage::Affine),
            ownership: PlaceOwnership::OutSlot,
            initialized: false,
        },
    ];
    assert_eq!(
        check_var_use(&gamma_out, Presence::Runtime, "x"),
        Err(ModeJudgmentError::ResidualNotDiscardable { name: "dst".into() })
    );
}

#[test]
fn nested_adt_substitution_respects_parameter_scopes_and_preserves_linearity() {
    let mut delta = KindEnv::default();
    delta.bind_var("Txn", usage_mode(Usage::Linear));

    // Wrapper[T] { value: T, pending: Option[Txn] }, where Option also declares param T.
    let wrapper_u64 = KindType::Adt {
        name: "Wrapper".into(),
        decl_mode: ModeVec::BOTTOM,
        params: vec!["T".into()],
        fields: vec![
            AdtFieldKind {
                name: "value".into(),
                ty: KindType::Var("T".into()),
                override_mode: None,
            },
            AdtFieldKind {
                name: "pending".into(),
                ty: make_option(KindType::Var("Txn".into())),
                override_mode: None,
            },
        ],
        type_args: vec![KindType::Prim(PrimKind::U64)],
    };

    assert_eq!(
        infer_modal_kind(&delta, &wrapper_u64),
        Ok(usage_mode(Usage::Linear))
    );
    assert_eq!(
        mode_cross(&delta, &wrapper_u64, usage_mode(Usage::Copy)),
        Err(ModeJudgmentError::ModeOrderViolation {
            actual: usage_mode(Usage::Linear),
            required: usage_mode(Usage::Copy),
        })
    );

    // Outer[U] { inner: PairBox[T] { outer: U, inner: T } } instantiated with U = T
    // (where T in Delta is linear) and PairBox instantiated with u64 must avoid capturing U -> T.
    delta.bind_var("T", usage_mode(Usage::Linear));
    let pair_box = KindType::Adt {
        name: "PairBox".into(),
        decl_mode: ModeVec::BOTTOM,
        params: vec!["T".into()],
        fields: vec![
            AdtFieldKind {
                name: "outer".into(),
                ty: KindType::Var("U".into()),
                override_mode: None,
            },
            AdtFieldKind {
                name: "inner".into(),
                ty: KindType::Var("T".into()),
                override_mode: None,
            },
        ],
        type_args: vec![KindType::Prim(PrimKind::U64)],
    };
    let outer_of_t = KindType::Adt {
        name: "Outer".into(),
        decl_mode: ModeVec::BOTTOM,
        params: vec!["U".into()],
        fields: vec![AdtFieldKind {
            name: "inner".into(),
            ty: pair_box,
            override_mode: None,
        }],
        type_args: vec![KindType::Var("T".into())],
    };
    assert_eq!(
        infer_modal_kind(&delta, &outer_of_t),
        Ok(usage_mode(Usage::Linear))
    );
}

#[test]
fn t_var_rejects_uninitialized_out_slot_as_target() {
    let gamma_only_dst = vec![VarContextEntry {
        name: "dst".into(),
        presence: Presence::Runtime,
        mode: usage_mode(Usage::Copy),
        ownership: PlaceOwnership::OutSlot,
        initialized: false,
    }];
    assert_eq!(
        check_var_use(&gamma_only_dst, Presence::Runtime, "dst"),
        Err(ModeJudgmentError::UninitializedVar { name: "dst".into() })
    );
}

#[test]
fn explicit_field_floors_survive_generic_instantiation_and_crossing() {
    let delta = KindEnv::default();
    for floor in [
        usage_mode(Usage::Affine),
        usage_mode(Usage::Linear),
        ModeVec::new(
            Usage::Linear,
            Portability::Pinned,
            Contention::Contended,
            Order::Unordered,
        ),
    ] {
        let wrapper = KindType::Adt {
            name: "OwnedScalar".into(),
            decl_mode: ModeVec::BOTTOM,
            params: vec!["T".into()],
            fields: vec![AdtFieldKind {
                name: "raw".into(),
                ty: KindType::Var("T".into()),
                override_mode: Some(floor),
            }],
            type_args: vec![KindType::Prim(PrimKind::I64)],
        };
        assert_eq!(infer_modal_kind(&delta, &wrapper), Ok(floor));
        assert_eq!(mode_cross(&delta, &wrapper, ModeVec::TOP), Ok(floor));
        assert!(mode_cross(&delta, &wrapper, ModeVec::BOTTOM).is_err());
    }
}

#[test]
fn permission_contraction_never_copies_exclusive_or_ordered_owners() {
    for usage in [Usage::Copy, Usage::Affine, Usage::Linear] {
        for order in [Order::Unordered, Order::Lifo] {
            for ownership in [
                PlaceOwnership::Own,
                PlaceOwnership::SharedBorrow,
                PlaceOwnership::MutBorrow,
                PlaceOwnership::OutSlot,
            ] {
                let entry = VarContextEntry {
                    name: "p".into(),
                    presence: Presence::Runtime,
                    mode: ModeVec::new(
                        usage,
                        Portability::Portable,
                        Contention::Uncontended,
                        order,
                    ),
                    ownership,
                    initialized: true,
                };
                let contracts = ownership == PlaceOwnership::SharedBorrow
                    || (ownership == PlaceOwnership::Own
                        && usage == Usage::Copy
                        && order == Order::Unordered);
                assert_eq!(entry.is_contractible(), contracts);
                let discards = ownership == PlaceOwnership::SharedBorrow
                    || (ownership == PlaceOwnership::Own
                        && usage <= Usage::Affine
                        && order == Order::Unordered);
                assert_eq!(entry.is_discardable(), discards);
                let uninitialized = VarContextEntry {
                    initialized: false,
                    ..entry
                };
                assert!(!uninitialized.is_contractible());
                assert!(!uninitialized.is_discardable());
            }
        }
    }
}

#[test]
fn ordered_owner_cannot_bypass_stack_by_becoming_an_ordinary_context_entry() {
    let guard = VarContextEntry {
        name: "guard".into(),
        presence: Presence::Runtime,
        mode: ModeVec::new(
            Usage::Copy,
            Portability::Portable,
            Contention::Uncontended,
            Order::Lifo,
        ),
        ownership: PlaceOwnership::Own,
        initialized: true,
    };
    let ordinary = VarContextEntry {
        name: "x".into(),
        mode: ModeVec::BOTTOM,
        ..guard.clone()
    };
    for target in ["guard", "x"] {
        assert_eq!(
            check_var_use(
                &[ordinary.clone(), guard.clone()],
                Presence::Runtime,
                target
            ),
            Err(ModeJudgmentError::OrderedBindingRequiresStack {
                name: "guard".into()
            })
        );
    }
    let view = VarContextEntry {
        ownership: PlaceOwnership::SharedBorrow,
        mode: ModeVec::new(
            Usage::Linear,
            Portability::Pinned,
            Contention::Uncontended,
            Order::Lifo,
        ),
        ..guard
    };
    assert_eq!(
        check_var_use(&[ordinary, view], Presence::Runtime, "x"),
        Ok((Presence::Runtime, ModeVec::BOTTOM))
    );
}

#[test]
fn explicit_ordered_scalar_field_is_rejected_even_without_a_resource_payload() {
    let wrapper = KindType::Adt {
        name: "HiddenGuard".into(),
        decl_mode: ModeVec::BOTTOM,
        params: vec![],
        type_args: vec![],
        fields: vec![AdtFieldKind {
            name: "raw".into(),
            ty: KindType::Prim(PrimKind::U64),
            override_mode: Some(ModeVec::TOP),
        }],
    };
    assert_eq!(
        infer_modal_kind(&KindEnv::default(), &wrapper),
        Err(ModeJudgmentError::OrderedField {
            adt: "HiddenGuard".into(),
            field: "raw".into()
        })
    );
}
