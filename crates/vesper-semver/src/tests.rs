use super::*;
fn int() -> Type {
    Type::Primitive {
        name: Primitive::I64,
    }
}
fn parameter(pass: Pass) -> Parameter {
    Parameter {
        pass,
        ty: int(),
        projection: Projection::All,
    }
}
fn function() -> Function {
    Function {
        generics: vec![],
        mode: Mode::Copy,
        parameters: vec![parameter(Pass::Sink)],
        result: int(),
        result_mode: Mode::Copy,
        effects: Effects::default(),
    }
}
fn interface(f: Function) -> Interface {
    Interface {
        exports: [("api".into(), Declaration::Function { signature: f })].into(),
        ..Default::default()
    }
}
fn check(a: Function, b: Function) -> Report {
    compare(&interface(a), &interface(b))
}
fn cap(name: &str, q: Resumption) -> Capability {
    Capability {
        identity: format!("cap:{name}"),
        effect: name.into(),
        arguments: vec![],
        resumption: q,
    }
}
fn paths(names: &[&str]) -> Projection {
    Projection::Paths {
        paths: names
            .iter()
            .map(|p| p.split('.').map(String::from).collect())
            .collect(),
    }
}
#[test]
fn identical_interface_is_patch_and_round_trips() {
    let old = interface(function());
    let json = serde_json::to_string(&old).unwrap();
    let new: Interface = serde_json::from_str(&json).unwrap();
    assert_eq!(compare(&old, &new).classification, Classification::Patch);
    assert!(compare(&old, &new).is_compatible());
}
#[test]
fn generic_modal_bounds_are_contravariant() {
    for a in [Mode::Copy, Mode::Affine, Mode::Linear] {
        for b in [Mode::Copy, Mode::Affine, Mode::Linear] {
            let mut old = function();
            old.generics = vec![Generic::Type { bound: a }];
            let mut new = old.clone();
            new.generics = vec![Generic::Type { bound: b }];
            assert_eq!(check(old, new).is_compatible(), a <= b);
        }
    }
}
#[test]
fn function_and_produced_value_modes_are_covariant() {
    for a in [Mode::Copy, Mode::Affine, Mode::Linear] {
        for b in [Mode::Copy, Mode::Affine, Mode::Linear] {
            let mut old = function();
            old.mode = a;
            old.result_mode = a;
            let mut new = old.clone();
            new.mode = b;
            new.result_mode = b;
            assert_eq!(check(old, new).is_compatible(), b <= a);
        }
    }
}
#[test]
fn adding_a_capability_or_weakening_resumption_breaks_callers() {
    let old = function();
    let mut new = old.clone();
    new.effects
        .capabilities
        .push(cap("Storage", Resumption::Linear));
    assert_eq!(
        check(old.clone(), new.clone()).classification,
        Classification::Major
    );
    assert!(check(new.clone(), old).is_compatible());
    let mut degraded = new.clone();
    degraded.effects.capabilities[0].resumption = Resumption::Affine;
    let report = check(new, degraded);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.rule == "SV-EffRow" && i.message.contains("resumption"))
    );
}
#[test]
fn lexical_identity_and_type_arguments_cannot_be_erased() {
    let mut old = function();
    old.effects
        .capabilities
        .push(cap("Storage", Resumption::Linear));
    let mut new = old.clone();
    new.effects.capabilities[0].identity = "another lexical handler".into();
    assert!(!check(old.clone(), new).is_compatible());
    let mut new = old.clone();
    new.effects.capabilities[0].arguments.push(int());
    assert!(!check(old, new).is_compatible());
}
#[test]
fn row_variables_and_their_guarantees_are_checked() {
    let mut old = function();
    old.generics.push(Generic::Row {
        minimum_resumption: Resumption::Linear,
    });
    old.effects.row_variables.insert(0);
    let mut new = old.clone();
    new.generics[0] = Generic::Row {
        minimum_resumption: Resumption::Affine,
    };
    assert!(!check(old.clone(), new).is_compatible());
    let mut removed = old.clone();
    removed.effects.row_variables.clear();
    assert!(check(old.clone(), removed).is_compatible());
    let mut unknown = old.clone();
    unknown.effects.row_variables.insert(9);
    assert_eq!(check(old, unknown).classification, Classification::Unknown);
}
#[test]
fn projections_use_subtree_prefixes_not_string_prefixes() {
    assert!(paths(&["index.keys"]).is_subset_of(&paths(&["index"])));
    assert!(!paths(&["index_extra"]).is_subset_of(&paths(&["index"])));
    assert!(paths(&[]).is_subset_of(&Projection::All));
    assert!(Projection::All.is_subset_of(&paths(&[])));
    let mut old = function();
    old.parameters[0].pass = Pass::Inout;
    old.parameters[0].projection = paths(&["index", "metrics"]);
    let mut new = old.clone();
    new.parameters[0].projection = paths(&["index.keys"]);
    assert!(check(old.clone(), new.clone()).is_compatible());
    assert!(!check(new, old).is_compatible());
}
#[test]
fn passing_mode_changes_never_silently_discharge_linear_obligations() {
    for old_pass in [Pass::In, Pass::Inout, Pass::Sink] {
        for new_pass in [Pass::In, Pass::Inout, Pass::Sink] {
            let mut old = function();
            old.generics = vec![Generic::Type {
                bound: Mode::Linear,
            }];
            old.parameters[0].pass = old_pass;
            old.parameters[0].ty = Type::Parameter { index: 0 };
            let mut new = old.clone();
            new.parameters[0].pass = new_pass;
            assert_eq!(check(old, new).is_compatible(), old_pass == new_pass);
        }
    }
}
#[test]
fn inout_to_in_reports_the_explicit_reborrow_adapter() {
    let mut old = function();
    old.parameters[0].pass = Pass::Inout;
    let mut new = old.clone();
    new.parameters[0].pass = Pass::In;
    let report = check(old, new);
    assert!(!report.is_compatible());
    assert_eq!(
        report.required_adapters,
        vec![ReborrowAdapter {
            function_path: "api".into(),
            parameter: 0
        }]
    );
}
#[test]
fn higher_order_parameters_are_contravariant_and_mutable_types_invariant() {
    let mut broad = function();
    broad
        .effects
        .capabilities
        .push(cap("Fail", Resumption::Affine));
    let narrow = function();
    let mut old = function();
    old.parameters[0].pass = Pass::In;
    old.parameters[0].ty = Type::Function {
        signature: Box::new(narrow.clone()),
    };
    let mut new = old.clone();
    new.parameters[0].ty = Type::Function {
        signature: Box::new(broad.clone()),
    };
    assert!(check(old.clone(), new.clone()).is_compatible());
    assert!(!check(new.clone(), old.clone()).is_compatible());
    old.parameters[0].pass = Pass::Inout;
    new.parameters[0].pass = Pass::Inout;
    assert!(!check(old, new).is_compatible());
}
#[test]
fn returning_a_less_effectful_callback_is_safe() {
    let mut callback = function();
    callback
        .effects
        .capabilities
        .push(cap("Fail", Resumption::Affine));
    let mut old = function();
    old.result = Type::Function {
        signature: Box::new(callback),
    };
    let mut new = old.clone();
    new.result = Type::Function {
        signature: Box::new(function()),
    };
    assert!(check(old, new).is_compatible());
}
fn enum_interface(open: bool, mode: Mode, variants: BTreeMap<String, Vec<Type>>) -> Interface {
    Interface {
        exports: [(
            "Error".into(),
            Declaration::Enum {
                generics: vec![],
                mode,
                open,
                variants,
            },
        )]
        .into(),
        ..Default::default()
    }
}
#[test]
fn closed_enums_cannot_add_variants_but_open_enums_can() {
    for open in [false, true] {
        let old = enum_interface(open, Mode::Affine, [("Old".into(), vec![])].into());
        let mut new = old.clone();
        let Declaration::Enum { variants, .. } = new.exports.get_mut("Error").unwrap() else {
            unreachable!()
        };
        variants.insert("New".into(), vec![int()]);
        assert_eq!(compare(&old, &new).is_compatible(), open);
    }
}
#[test]
fn an_open_enum_cannot_hide_a_new_linear_payload() {
    let mut old = enum_interface(true, Mode::Affine, [("Old".into(), vec![])].into());
    old.external_types.insert(
        "Ticket".into(),
        ExternalType {
            arity: 0,
            mode: Mode::Linear,
        },
    );
    let mut new = old.clone();
    let Declaration::Enum { variants, mode, .. } = new.exports.get_mut("Error").unwrap() else {
        unreachable!()
    };
    *mode = Mode::Linear;
    variants.insert(
        "New".into(),
        vec![Type::Named {
            name: "Ticket".into(),
            arguments: vec![],
        }],
    );
    let report = compare(&old, &new);
    assert_eq!(report.classification, Classification::Major);
    assert!(report.issues.iter().any(|i| i.message.contains("payload")));
}
#[test]
fn open_and_sealed_effect_resumption_rules_differ() {
    for sealed in [false, true] {
        let old = Interface {
            exports: [(
                "E".into(),
                Declaration::Effect {
                    generics: vec![],
                    sealed,
                    resumption: Resumption::Affine,
                    operations: [("op".into(), function())].into(),
                },
            )]
            .into(),
            ..Default::default()
        };
        let mut new = old.clone();
        let Declaration::Effect { resumption, .. } = new.exports.get_mut("E").unwrap() else {
            unreachable!()
        };
        *resumption = Resumption::Linear;
        assert_eq!(compare(&old, &new).is_compatible(), sealed);
        assert!(!compare(&new, &old).is_compatible());
    }
}
#[test]
fn closed_fields_may_change_body_but_public_fields_are_stable_contracts() {
    let old = Interface {
        exports: [(
            "S".into(),
            Declaration::Struct {
                generics: vec![],
                mode: Mode::Linear,
                fields: Fields::Closed,
            },
        )]
        .into(),
        ..Default::default()
    };
    let mut new = old.clone();
    let Declaration::Struct { mode, .. } = new.exports.get_mut("S").unwrap() else {
        unreachable!()
    };
    *mode = Mode::Copy;
    assert!(compare(&old, &new).is_compatible());
    let mut new = old.clone();
    let Declaration::Struct { fields, .. } = new.exports.get_mut("S").unwrap() else {
        unreachable!()
    };
    *fields = Fields::Public {
        fields: [("x".into(), int())].into(),
    };
    assert!(!compare(&old, &new).is_compatible());
}
#[test]
fn unknown_inputs_never_certify_even_when_identical() {
    for value in [
        Type::Unsupported {
            description: "higher rank lifetime".into(),
        },
        Type::Named {
            name: "Missing".into(),
            arguments: vec![],
        },
        Type::Parameter { index: 8 },
    ] {
        let mut f = function();
        f.result = value;
        let interface = interface(f);
        let report = compare(&interface, &interface);
        assert_eq!(report.classification, Classification::Unknown);
        assert!(!report.is_compatible());
    }
    let mut unknown = interface(function());
    unknown.schema_version = 2;
    assert_eq!(
        compare(&unknown, &unknown).classification,
        Classification::Unknown
    );
}
#[test]
fn removal_and_declaration_kind_changes_break_importing_callers() {
    let old = interface(function());
    assert_eq!(
        compare(&old, &Interface::default()).classification,
        Classification::Major
    );
    let new = Interface {
        exports: [(
            "api".into(),
            Declaration::Struct {
                generics: vec![],
                mode: Mode::Copy,
                fields: Fields::Closed,
            },
        )]
        .into(),
        ..Default::default()
    };
    assert_eq!(compare(&old, &new).classification, Classification::Major);
}
#[test]
fn malformed_projection_and_binder_kinds_fail_closed() {
    let mut f = function();
    f.parameters[0].pass = Pass::In;
    f.parameters[0].projection = Projection::Paths {
        paths: [vec![]].into(),
    };
    assert_eq!(check(f.clone(), f).classification, Classification::Unknown);
    let mut f = function();
    f.generics.push(Generic::Row {
        minimum_resumption: Resumption::Affine,
    });
    f.result = Type::Parameter { index: 0 };
    assert_eq!(check(f.clone(), f).classification, Classification::Unknown);
}

#[test]
fn changed_operation_signatures_are_rejected_even_with_the_same_names() {
    let old = Interface {
        exports: [(
            "E".into(),
            Declaration::Effect {
                generics: vec![],
                sealed: false,
                resumption: Resumption::Affine,
                operations: [("op".into(), function())].into(),
            },
        )]
        .into(),
        ..Default::default()
    };
    let mut new = old.clone();
    let Declaration::Effect { operations, .. } = new.exports.get_mut("E").unwrap() else {
        unreachable!()
    };
    operations.get_mut("op").unwrap().parameters[0].pass = Pass::In;
    assert_eq!(compare(&old, &new).classification, Classification::Major);
}
#[test]
fn empty_projection_sets_are_not_a_no_access_permission() {
    let mut f = function();
    f.parameters[0].pass = Pass::In;
    f.parameters[0].projection = paths(&[]);
    assert_eq!(check(f.clone(), f).classification, Classification::Unknown);
}
#[test]
fn stricter_referenced_external_kinds_break_clients() {
    let mut old = interface(function());
    old.external_types.insert(
        "Other".into(),
        ExternalType {
            arity: 0,
            mode: Mode::Copy,
        },
    );
    let mut new = old.clone();
    new.external_types.get_mut("Other").unwrap().mode = Mode::Linear;
    assert_eq!(compare(&old, &new).classification, Classification::Major);
}
#[test]
fn json_rejects_unknown_fields_and_duplicate_export_identities() {
    let json = serde_json::to_string(&Interface::default()).unwrap();
    assert!(
        serde_json::from_str::<Interface>(
            &json.replace("\"exports\":{}", "\"exports\":{},\"extra\":true")
        )
        .is_err()
    );
    let f = serde_json::to_string(&Declaration::Function {
        signature: function(),
    })
    .unwrap();
    let json = format!(
        "{{\"schema_version\":1,\"exports\":{{\"api\":{f},\"api\":{f}}},\"external_types\":{{}}}}"
    );
    assert!(serde_json::from_str::<Interface>(&json).is_err());
}

#[test]
fn metadata_cannot_claim_copy_for_a_linear_result() {
    let mut interface = interface(function());
    interface.external_types.insert(
        "Ticket".into(),
        ExternalType {
            arity: 0,
            mode: Mode::Linear,
        },
    );
    let Declaration::Function { signature } = interface.exports.get_mut("api").unwrap() else {
        unreachable!()
    };
    signature.result = Type::Named {
        name: "Ticket".into(),
        arguments: vec![],
    };
    assert_eq!(
        compare(&interface, &interface).classification,
        Classification::Unknown
    );
}
#[test]
fn model_versions_are_part_of_the_certification_boundary() {
    let mut interface = interface(function());
    interface.semantics_version = 42;
    assert_eq!(
        compare(&interface, &interface).classification,
        Classification::Unknown
    );
}

#[test]
fn wrapping_arithmetic_exports_cannot_be_certified_under_checked_semantics() {
    let checked = interface(function());
    assert_eq!(checked.semantics_version, 2);
    let mut wrapping = checked.clone();
    wrapping.semantics_version = 1;
    for (old, new) in [
        (&wrapping, &checked),
        (&checked, &wrapping),
        (&wrapping, &wrapping),
    ] {
        let report = compare(old, new);
        assert_eq!(report.classification, Classification::Unknown);
        assert!(!report.is_compatible());
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.message.contains("language-semantics version"))
        );
    }
    assert!(compare(&checked, &checked).is_compatible());
}

#[test]
fn an_already_affine_row_does_not_mask_a_changed_capability_contract() {
    let mut old = function();
    old.effects.capabilities = vec![cap("A", Resumption::Affine), cap("B", Resumption::Linear)];
    let mut new = old.clone();
    new.effects.capabilities[1].resumption = Resumption::Affine;
    assert_eq!(check(old, new).classification, Classification::Major);
}
