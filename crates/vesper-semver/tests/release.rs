use vesper_semver::{
    Classification, Declaration, Effects, Fields, Function, Interface, Mode, Parameter, Pass,
    Primitive, Projection, RELEASE_POLICY_VERSION, ReleaseBump, ReleaseIssueCode, ReleaseReport,
    ReleaseVersionSide, Type, check_release,
};

fn added_export() -> Interface {
    let mut interface = Interface::default();
    interface.exports.insert(
        "api::Value".into(),
        Declaration::Struct {
            generics: vec![],
            mode: Mode::Copy,
            fields: Fields::Closed,
        },
    );
    interface
}

fn report(class: Classification, before: &str, after: &str) -> ReleaseReport {
    let empty = Interface::default();
    let added = added_export();
    let (old, new) = match class {
        Classification::Patch => (&empty, &empty),
        Classification::Minor => (&empty, &added),
        Classification::Major => (&added, &empty),
        Classification::Unknown => unreachable!(),
    };
    let report = check_release(old, new, before, after).unwrap();
    assert_eq!(report.required_bump, class);
    assert_eq!(report.comparison.classification, class);
    report
}

#[test]
fn minimum_numeric_bump_matches_actual_interface_comparison() {
    for (class, version, bump, accepted) in [
        (Classification::Patch, "1.0.1", ReleaseBump::Patch, true),
        (Classification::Patch, "1.1.0", ReleaseBump::Minor, true),
        (Classification::Patch, "2.0.0", ReleaseBump::Major, true),
        (Classification::Minor, "1.0.1", ReleaseBump::Patch, false),
        (Classification::Minor, "1.1.0", ReleaseBump::Minor, true),
        (Classification::Minor, "2.0.0", ReleaseBump::Major, true),
        (Classification::Major, "1.0.1", ReleaseBump::Patch, false),
        (Classification::Major, "1.1.0", ReleaseBump::Minor, false),
        (Classification::Major, "2.0.0", ReleaseBump::Major, true),
    ] {
        let report = report(class, "1.0.0", version);
        assert_eq!(report.policy_version, RELEASE_POLICY_VERSION);
        assert_eq!(report.actual_bump, Some(bump));
        assert_eq!(report.accepted, accepted, "{class:?} -> {version}");
        assert_eq!(report.issues.is_empty(), accepted);
        if !accepted {
            assert_eq!(report.issues[0].code, ReleaseIssueCode::InsufficientBump);
        }
    }
}

#[test]
fn component_resets_are_allowed_only_when_precedence_increases() {
    assert!(report(Classification::Minor, "1.2.99", "1.3.0").accepted);
    assert!(report(Classification::Major, "1.99.99", "2.0.0").accepted);
    for (before, after) in [
        ("1.2.3", "1.2.3"),
        ("1.2.3", "1.2.2"),
        ("1.2.3", "1.1.99"),
        ("2.0.0", "1.99.99"),
    ] {
        let report = report(Classification::Patch, before, after);
        assert!(!report.accepted, "{before} -> {after}");
        assert_eq!(report.actual_bump, None);
        assert_eq!(
            report.issues[0].code,
            ReleaseIssueCode::NonIncreasingVersion
        );
    }
}

#[test]
fn metadata_never_counts_as_a_bump_or_changes_precedence() {
    for after in ["1.2.3", "1.2.3+aaa", "1.2.3+zzz"] {
        let report = report(Classification::Patch, "1.2.3+build.10", after);
        assert!(!report.accepted);
        assert_eq!(report.actual_bump, None);
        assert_eq!(
            report.issues[0].code,
            ReleaseIssueCode::NonIncreasingVersion
        );
    }
    let report = report(Classification::Patch, "1.2.3+zzz", "1.2.4+aaa");
    assert!(report.accepted);
    assert_eq!(report.old_version, "1.2.3+zzz");
    assert_eq!(report.new_version, "1.2.4+aaa");
}

#[test]
fn prerelease_transitions_are_explicitly_unsupported() {
    for (before, after) in [
        ("1.0.0-alpha.1", "1.0.0-alpha.2"),
        ("1.0.0-alpha.1", "1.0.0"),
        ("1.0.0", "2.0.0-rc.1"),
        ("1.0.0-alpha.1", "2.0.0"),
    ] {
        let report = report(Classification::Patch, before, after);
        assert!(!report.accepted);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| { issue.code == ReleaseIssueCode::PrereleaseUnsupported })
        );
    }
}

#[test]
fn pre_one_policy_uses_the_same_numeric_bumps_without_a_breaking_exception() {
    let breaking_minor = report(Classification::Major, "0.4.0", "0.5.0");
    assert!(!breaking_minor.accepted);
    assert_eq!(breaking_minor.actual_bump, Some(ReleaseBump::Minor));
    assert_eq!(
        breaking_minor.issues[0].code,
        ReleaseIssueCode::InsufficientBump
    );
    assert!(!report(Classification::Minor, "0.4.0", "0.4.1").accepted);
    assert!(report(Classification::Minor, "0.4.0", "0.5.0").accepted);
    assert!(report(Classification::Major, "0.4.0", "1.0.0").accepted);
    assert!(report(Classification::Patch, "0.0.0", "0.0.1").accepted);
}

#[test]
fn unknown_contracts_never_pass_even_with_a_major_bump() {
    let old = Interface::default();
    let mut unknown = Interface::default();
    unknown.exports.insert(
        "api::Unknown".into(),
        Declaration::Unsupported {
            description: "unmodeled scoped capability".into(),
        },
    );
    let report = check_release(&old, &unknown, "1.0.0", "99.0.0").unwrap();
    assert_eq!(report.comparison.classification, Classification::Unknown);
    assert_eq!(report.required_bump, Classification::Unknown);
    assert_eq!(report.actual_bump, Some(ReleaseBump::Major));
    assert!(!report.accepted);
    assert_eq!(report.issues[0].code, ReleaseIssueCode::UnknownInterface);
    assert!(!report.comparison.issues.is_empty());

    let mut bad_schema = Interface::default();
    bad_schema.schema_version += 1;
    assert!(
        !check_release(&old, &bad_schema, "1.0.0", "2.0.0")
            .unwrap()
            .accepted
    );
}

#[test]
fn major_release_retains_required_adapters_without_claiming_source_compatibility() {
    let signature = Function {
        generics: vec![],
        mode: Mode::Copy,
        parameters: vec![Parameter {
            pass: Pass::Inout,
            ty: Type::Primitive {
                name: Primitive::I64,
            },
            projection: Projection::All,
        }],
        result: Type::Primitive {
            name: Primitive::I64,
        },
        result_mode: Mode::Copy,
        effects: Effects::default(),
    };
    let mut old = Interface::default();
    old.exports.insert(
        "api::read".into(),
        Declaration::Function {
            signature: signature.clone(),
        },
    );
    let mut signature = signature;
    signature.parameters[0].pass = Pass::In;
    let mut new = Interface::default();
    new.exports
        .insert("api::read".into(), Declaration::Function { signature });

    let report = check_release(&old, &new, "1.0.0", "2.0.0").unwrap();
    assert!(report.accepted);
    assert_eq!(report.comparison.classification, Classification::Major);
    assert!(!report.comparison.is_compatible());
    assert_eq!(report.comparison.required_adapters.len(), 1);
    assert_eq!(
        report.comparison.required_adapters[0].function_path,
        "api::read"
    );
    assert!(
        !check_release(&old, &new, "1.0.0", "1.1.0")
            .unwrap()
            .accepted
    );
}

#[test]
fn invalid_and_overflowing_versions_return_typed_errors() {
    let interface = Interface::default();
    for bad in [
        "1",
        "v1.0.0",
        "1.00.0",
        "1.0.0-",
        "18446744073709551616.0.0",
    ] {
        let old_error = check_release(&interface, &interface, bad, "2.0.0").unwrap_err();
        assert_eq!(old_error.side, ReleaseVersionSide::Old);
        assert_eq!(old_error.version, bad);
        assert!(
            old_error
                .to_string()
                .contains("invalid old release version")
        );
        let new_error = check_release(&interface, &interface, "1.0.0", bad).unwrap_err();
        assert_eq!(new_error.side, ReleaseVersionSide::New);
        assert_eq!(new_error.version, bad);
    }
}

#[test]
fn report_serialization_preserves_policy_and_diagnostics() {
    let report = report(Classification::Major, "1.0.0", "1.0.1");
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["policy_version"], 1);
    assert_eq!(json["actual_bump"], "patch");
    assert_eq!(json["required_bump"], "major");
    assert_eq!(json["issues"][0]["code"], "insufficient_bump");
    let round_trip: ReleaseReport = serde_json::from_value(json).unwrap();
    assert_eq!(report, round_trip);
}
