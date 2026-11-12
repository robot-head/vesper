use semver::{Version, VersionReq};
use std::collections::BTreeSet;
use vesper_loom_calculus::{
    Dependency, Feature, Manifest, Package, PackageName, Resolver, ResolverPolicy, SCHEMA_VERSION,
    Selection, Solution, SystemSelection, validate_solution,
};
use vesper_loom_solver::{Catalog, ErrorKind, ResolveRequest, resolve, validate_locked};

fn name(s: &str) -> PackageName {
    PackageName::parse(format!("@test/{s}")).unwrap()
}
fn set(strings: &[&str]) -> BTreeSet<String> {
    strings.iter().map(|s| (*s).into()).collect()
}
fn package(s: &str, version: &str) -> Manifest {
    Manifest {
        package: Package {
            name: name(s),
            version: Version::parse(version).unwrap(),
            edition: "2026".into(),
            license: None,
            capability_ceiling: BTreeSet::new(),
            forbid_unsafe_ffi: true,
        },
        features: Default::default(),
        dependencies: Default::default(),
        build_sandbox: None,
        resolver: None,
        verification: None,
    }
}
fn dep(manifest: &mut Manifest, s: &str, version: &str) {
    manifest.dependencies.insert(
        name(s),
        Dependency {
            version: VersionReq::parse(version).unwrap(),
            effects: BTreeSet::new(),
            optional: false,
            features: BTreeSet::new(),
        },
    );
}
fn request() -> ResolveRequest {
    let mut root = package("root", "1.0.0+root");
    dep(&mut root, "a", "^1");
    ResolveRequest {
        root,
        features: BTreeSet::new(),
        catalog: Catalog {
            schema_version: SCHEMA_VERSION,
            packages: vec![package("a", "1.0.0+one")],
            system_packages: vec![],
        },
        max_decisions: 100,
    }
}
fn selection<'a>(locked: &'a mut Solution, s: &str) -> &'a mut Selection {
    locked
        .packages
        .iter_mut()
        .find(|p| p.manifest.package.name == name(s))
        .unwrap()
}
fn mismatch(request: &ResolveRequest, locked: &Solution, code: &str) {
    let error = validate_locked(request, locked).unwrap_err();
    assert_eq!(error.kind, ErrorKind::LockedMismatch, "{error}");
    assert_eq!(error.decisions, 0);
    assert!(error.diagnostics.iter().any(|d| d.code == code), "{error}");
}
fn policy() -> Resolver {
    Resolver {
        policy: ResolverPolicy {
            optimize: vec!["min_versions".into()],
            min_repro_attestations: None,
            block_unvetted_namespaces: false,
        },
    }
}

#[test]
fn valid_older_selection_is_preserved_without_a_search_budget() {
    let mut req = request();
    let locked = resolve(&req).unwrap();
    req.catalog.packages.push(package("a", "1.9.0"));
    assert_ne!(resolve(&req).unwrap(), locked);
    req.max_decisions = 0;
    assert_eq!(validate_locked(&req, &locked), Ok(()));
    assert_eq!(resolve(&req).unwrap_err().kind, ErrorKind::BudgetExhausted);
}

#[test]
fn manifest_metadata_and_build_metadata_must_match() {
    let req = request();
    let original = resolve(&req).unwrap();
    for s in ["root", "a"] {
        let mut locked = original.clone();
        selection(&mut locked, s).manifest.package.license = Some("MIT".into());
        assert!(validate_solution(&locked).diagnostics.is_empty());
        mismatch(&req, &locked, "locked_manifest_mismatch");
    }
    let mut changed_catalog = req.clone();
    changed_catalog.catalog.packages[0].package.license = Some("MIT".into());
    mismatch(&changed_catalog, &original, "locked_manifest_mismatch");
    let mut changed_root = req.clone();
    changed_root.root.package.version = Version::parse("1.0.0+other").unwrap();
    mismatch(&changed_root, &original, "locked_manifest_mismatch");
    let mut changed_catalog = req.clone();
    changed_catalog.catalog.packages[0].package.version = Version::parse("1.0.0+other").unwrap();
    mismatch(&changed_catalog, &original, "locked_candidate_missing");
}

#[test]
fn root_requirement_is_single_exact_and_feature_sensitive() {
    let mut req = request();
    req.root.features.insert("extra".into(), Feature::default());
    let locked = resolve(&req).unwrap();
    let mut duplicate = locked.clone();
    duplicate.roots.push(duplicate.roots[0].clone());
    mismatch(&req, &duplicate, "locked_root_mismatch");
    let mut wildcard = locked.clone();
    wildcard.roots[0].version = VersionReq::STAR;
    mismatch(&req, &wildcard, "locked_root_mismatch");
    let mut absent = locked.clone();
    absent.roots.clear();
    mismatch(&req, &absent, "locked_root_mismatch");
    req.features.insert("extra".into());
    mismatch(&req, &locked, "locked_root_mismatch");
}

#[test]
fn selected_features_cannot_enable_unrequested_optional_work() {
    let mut req = request();
    req.catalog.packages[0]
        .features
        .insert("spare".into(), Feature::default());
    let mut locked = resolve(&req).unwrap();
    selection(&mut locked, "a").features.insert("spare".into());
    assert!(validate_solution(&locked).diagnostics.is_empty());
    mismatch(&req, &locked, "locked_features_mismatch");
}

fn cyclic_native_request() -> ResolveRequest {
    let mut req = request();
    req.root.dependencies.get_mut(&name("a")).unwrap().features = set(&["base"]);
    let a = &mut req.catalog.packages[0];
    dep(a, "b", "^1");
    a.dependencies.get_mut(&name("b")).unwrap().optional = true;
    a.features.insert(
        "base".into(),
        Feature {
            deps: BTreeSet::from([name("b")]),
            ..Feature::default()
        },
    );
    a.features.insert(
        "native".into(),
        Feature {
            sys_deps: set(&["native >=1, <2"]),
            ..Feature::default()
        },
    );
    let mut b = package("b", "1.0.0");
    dep(&mut b, "a", "^1");
    b.dependencies.get_mut(&name("a")).unwrap().features = set(&["native"]);
    req.catalog.packages.push(b);
    req.catalog.system_packages = vec![SystemSelection {
        name: "native".into(),
        version: Version::new(1, 0, 0),
    }];
    req
}

#[test]
fn transitive_cyclic_features_and_native_requirements_are_recomputed() {
    let req = cyclic_native_request();
    let locked = resolve(&req).unwrap();
    assert_eq!(validate_locked(&req, &locked), Ok(()));
    assert_eq!(
        locked
            .packages
            .iter()
            .find(|s| s.manifest.package.name == name("a"))
            .unwrap()
            .features,
        set(&["base", "native"])
    );
    let mut missing_feature = locked.clone();
    selection(&mut missing_feature, "a")
        .features
        .remove("native");
    // The permissive calculus expands features requested by dependency edges;
    // locked checking additionally requires that the stored closure be exact.
    assert!(validate_solution(&missing_feature).diagnostics.is_empty());
    mismatch(&req, &missing_feature, "locked_features_mismatch");
    let mut missing_native = locked.clone();
    missing_native.system_packages.clear();
    mismatch(&req, &missing_native, "locked_system_missing");
    let mut missing_package = locked.clone();
    missing_package
        .packages
        .retain(|s| s.manifest.package.name != name("b"));
    mismatch(&req, &missing_package, "locked_package_missing");
}

#[test]
fn unused_packages_and_systems_are_rejected_even_when_in_the_catalog() {
    let mut req = request();
    req.catalog.packages.push(package("unused", "1.0.0"));
    req.catalog.system_packages.push(SystemSelection {
        name: "unused".into(),
        version: Version::new(1, 0, 0),
    });
    let mut locked = resolve(&req).unwrap();
    locked.packages.push(Selection {
        manifest: req.catalog.packages[1].clone(),
        features: BTreeSet::new(),
    });
    locked.system_packages = req.catalog.system_packages.clone();
    assert!(validate_solution(&locked).diagnostics.is_empty());
    mismatch(&req, &locked, "locked_unused_package");
    mismatch(&req, &locked, "locked_unused_system");
}

#[test]
fn removed_and_incompatible_native_versions_are_rejected() {
    let mut req = cyclic_native_request();
    let locked = resolve(&req).unwrap();
    req.catalog.system_packages.clear();
    mismatch(&req, &locked, "locked_system_candidate_missing");
    req.catalog.system_packages.push(SystemSelection {
        name: "native".into(),
        version: Version::new(2, 0, 0),
    });
    let mut incompatible = locked.clone();
    incompatible.system_packages = req.catalog.system_packages.clone();
    mismatch(&req, &incompatible, "VersionMismatch");
    req.catalog
        .system_packages
        .push(locked.system_packages[0].clone());
    req.max_decisions = 0;
    assert_eq!(validate_locked(&req, &locked), Ok(()));
}

#[test]
fn malformed_snapshot_and_duplicate_selections_are_mismatches() {
    let req = cyclic_native_request();
    let locked = resolve(&req).unwrap();
    let mut duplicate_package = locked.clone();
    duplicate_package.packages.push(locked.packages[0].clone());
    mismatch(&req, &duplicate_package, "DuplicatePackage");
    let mut duplicate_native = locked.clone();
    duplicate_native
        .system_packages
        .push(locked.system_packages[0].clone());
    mismatch(&req, &duplicate_native, "DuplicateSystemPackage");
    let mut wrong_schema = locked.clone();
    wrong_schema.schema_version += 1;
    mismatch(&req, &wrong_schema, "ManifestSyntax");
    let mut malformed = locked;
    selection(&mut malformed, "a")
        .manifest
        .features
        .get_mut("base")
        .unwrap()
        .deps
        .insert(name("undeclared"));
    mismatch(&req, &malformed, "UnknownDependency");
}

#[test]
fn missing_catalog_candidate_does_not_trigger_fresh_resolution() {
    let mut req = request();
    let locked = resolve(&req).unwrap();
    req.catalog.packages = vec![package("a", "1.9.0")];
    assert!(resolve(&req).is_ok());
    mismatch(&req, &locked, "locked_candidate_missing");
}

#[test]
fn invalid_input_and_unsupported_policy_remain_distinct() {
    let req = request();
    let locked = resolve(&req).unwrap();
    let mut invalid = req.clone();
    invalid
        .catalog
        .packages
        .push(invalid.catalog.packages[0].clone());
    let e = validate_locked(&invalid, &locked).unwrap_err();
    assert_eq!((e.kind, e.decisions), (ErrorKind::InvalidInput, 0));
    let mut unsupported = req.clone();
    unsupported.root.resolver = Some(policy());
    let e = validate_locked(&unsupported, &locked).unwrap_err();
    assert_eq!((e.kind, e.decisions), (ErrorKind::UnsupportedPolicy, 0));
    let mut unsupported = req.clone();
    unsupported.catalog.packages[0].resolver = Some(policy());
    let mut unsupported_lock = locked.clone();
    selection(&mut unsupported_lock, "a").manifest.resolver = Some(policy());
    let e = validate_locked(&unsupported, &unsupported_lock).unwrap_err();
    assert_eq!((e.kind, e.decisions), (ErrorKind::UnsupportedPolicy, 0));
    let mut unused_policy = package("unused", "1.0.0");
    unused_policy.resolver = Some(policy());
    let mut allowed = req;
    allowed.catalog.packages.push(unused_policy);
    assert_eq!(validate_locked(&allowed, &locked), Ok(()));
}

#[test]
fn collection_order_does_not_change_results_or_diagnostics() {
    let mut req = cyclic_native_request();
    let mut locked = resolve(&req).unwrap();
    req.catalog.packages.reverse();
    locked.packages.reverse();
    assert_eq!(validate_locked(&req, &locked), Ok(()));
    locked.packages[0].manifest.package.license = Some("changed".into());
    let error = validate_locked(&req, &locked).unwrap_err();
    req.catalog.packages.reverse();
    locked.packages.reverse();
    assert_eq!(validate_locked(&req, &locked), Err(error.clone()));
    assert_eq!(
        serde_json::to_value(&error).unwrap()["kind"],
        "locked_mismatch"
    );
}
