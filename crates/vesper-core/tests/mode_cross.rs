use vesper_core::mode_cross::{
    AdtFieldKind, KindEnv, KindType, ModeJudgmentError, PlaceOwnership, Presence, PrimKind,
    VarContextEntry, check_var_use, infer_modal_kind, mode_cross, sub_mode, usage_mode,
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
            override_mode: ModeVec::TOP,
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
        Ok(ModeVec::new(
            Usage::Linear,
            Portability::Pinned,
            Contention::Uncontended,
            Order::Lifo,
        ))
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
        mode_cross(&delta, &opt_linear_lifo, usage_mode(Usage::Affine)),
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
                override_mode: ModeVec::TOP,
            },
            AdtFieldKind {
                name: "pending".into(),
                ty: make_option(KindType::Var("Txn".into())),
                override_mode: ModeVec::TOP,
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
                override_mode: ModeVec::TOP,
            },
            AdtFieldKind {
                name: "inner".into(),
                ty: KindType::Var("T".into()),
                override_mode: ModeVec::TOP,
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
            override_mode: ModeVec::TOP,
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
