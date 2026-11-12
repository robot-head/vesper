use semver::{Version, VersionReq};
use std::collections::BTreeSet;
use vesper_loom_calculus::{
    Dependency, Feature, Manifest, Package, PackageName, Resolver, ResolverPolicy, SCHEMA_VERSION,
    SystemSelection, validate_solution,
};
use vesper_loom_solver::{Catalog, ErrorKind, ResolveRequest, resolve};

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
fn request(root: Manifest, packages: Vec<Manifest>) -> ResolveRequest {
    ResolveRequest {
        root,
        features: BTreeSet::new(),
        catalog: Catalog {
            schema_version: SCHEMA_VERSION,
            packages,
            system_packages: vec![],
        },
        max_decisions: 10_000,
    }
}
fn selected(result: &vesper_loom_calculus::Solution, s: &str) -> Version {
    result
        .packages
        .iter()
        .find(|p| p.manifest.package.name == name(s))
        .unwrap()
        .manifest
        .package
        .version
        .clone()
}

#[test]
fn backtracks_latest_parent_after_transitive_conflict() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    dep(&mut root, "b", "=1.0.0");
    let mut newest = package("a", "1.1.0");
    dep(&mut newest, "b", "^2");
    let mut older = package("a", "1.0.0");
    dep(&mut older, "b", "^1");
    let result = resolve(&request(
        root,
        vec![newest, older, package("b", "1.0.0"), package("b", "2.0.0")],
    ))
    .unwrap();
    assert_eq!(selected(&result, "a"), Version::new(1, 0, 0));
    assert!(validate_solution(&result).graph.is_some());
}

#[test]
fn supplied_root_is_fixed_even_with_newer_catalog_root() {
    let req = request(
        package("root", "1.0.0+source"),
        vec![package("root", "2.0.0")],
    );
    let result = resolve(&req).unwrap();
    assert_eq!(result.packages[0].manifest, req.root);
    assert_eq!(selected(&result, "root").to_string(), "1.0.0+source");
}

#[test]
fn inactive_optional_dependency_is_not_selected() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "optional", "^1");
    root.dependencies
        .get_mut(&name("optional"))
        .unwrap()
        .optional = true;
    root.features.insert(
        "extra".into(),
        Feature {
            deps: BTreeSet::from([name("optional")]),
            ..Feature::default()
        },
    );
    let mut req = request(root, vec![]);
    assert_eq!(resolve(&req).unwrap().packages.len(), 1);
    req.features.insert("extra".into());
    assert_eq!(resolve(&req).unwrap_err().kind, ErrorKind::Unsatisfiable);
}

#[test]
fn late_feature_requests_revisit_nodes_through_cycles() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    dep(&mut root, "z", "^1");
    let mut a = package("a", "1.0.0");
    dep(&mut a, "optional", "^1");
    a.dependencies.get_mut(&name("optional")).unwrap().optional = true;
    a.features.insert(
        "outer".into(),
        Feature {
            features: set(&["inner"]),
            ..Feature::default()
        },
    );
    a.features.insert(
        "inner".into(),
        Feature {
            deps: BTreeSet::from([name("optional")]),
            ..Feature::default()
        },
    );
    let mut z = package("z", "1.0.0");
    dep(&mut z, "a", "^1");
    z.dependencies.get_mut(&name("a")).unwrap().features = set(&["outer"]);
    let mut optional = package("optional", "1.0.0");
    dep(&mut optional, "z", "^1");
    let result = resolve(&request(root, vec![a, z, optional])).unwrap();
    assert_eq!(result.packages.len(), 4);
    assert_eq!(
        result
            .packages
            .iter()
            .find(|s| s.manifest.package.name == name("a"))
            .unwrap()
            .features,
        set(&["outer", "inner"])
    );
}

#[test]
fn feature_requested_by_dependency_can_force_older_version() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    root.dependencies.get_mut(&name("a")).unwrap().features = set(&["available"]);
    let mut old = package("a", "1.0.0");
    old.features.insert("available".into(), Feature::default());
    let result = resolve(&request(root, vec![old, package("a", "1.1.0")])).unwrap();
    assert_eq!(selected(&result, "a"), Version::new(1, 0, 0));
}

#[test]
fn feature_capability_escalation_is_rejected() {
    let mut root = package("root", "1.0.0");
    root.features.insert(
        "network".into(),
        Feature {
            enables_effects: set(&["Net"]),
            ..Feature::default()
        },
    );
    let mut req = request(root, vec![]);
    req.features = set(&["network"]);
    let error = resolve(&req).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsatisfiable);
    assert!(
        error
            .diagnostics
            .iter()
            .any(|d| d.code == "CapabilityEscalation")
    );
}

#[test]
fn source_and_edge_ceilings_both_apply() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    root.package.capability_ceiling = set(&["Net"]);
    let mut a = package("a", "1.1.0");
    a.package.capability_ceiling = set(&["Net"]);
    let req = request(root.clone(), vec![a.clone(), package("a", "1.0.0")]);
    assert_eq!(
        selected(&resolve(&req).unwrap(), "a"),
        Version::new(1, 0, 0)
    );
    root.dependencies.get_mut(&name("a")).unwrap().effects = set(&["Net"]);
    assert_eq!(
        selected(
            &resolve(&request(root.clone(), vec![a.clone()])).unwrap(),
            "a"
        ),
        Version::new(1, 1, 0)
    );
    root.package.capability_ceiling.clear();
    assert_eq!(
        resolve(&request(root, vec![a])).unwrap_err().kind,
        ErrorKind::Unsatisfiable
    );
}

#[test]
fn ffi_identity_and_transitive_unsafe_ban_are_preserved() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    root.package.capability_ceiling = set(&["Ffi[LibA]"]);
    root.dependencies.get_mut(&name("a")).unwrap().effects = set(&["Ffi[LibA]"]);
    let mut a = package("a", "1.0.0");
    a.package.forbid_unsafe_ffi = false;
    a.package.capability_ceiling = set(&["Ffi[LibA]"]);
    assert_eq!(
        resolve(&request(root.clone(), vec![a.clone()]))
            .unwrap_err()
            .kind,
        ErrorKind::Unsatisfiable
    );
    root.package.forbid_unsafe_ffi = false;
    assert!(resolve(&request(root.clone(), vec![a.clone()])).is_ok());
    root.dependencies.get_mut(&name("a")).unwrap().effects = set(&["Ffi[LibB]"]);
    assert_eq!(
        resolve(&request(root, vec![a])).unwrap_err().kind,
        ErrorKind::Unsatisfiable
    );
}

#[test]
fn native_constraints_unify_and_pick_latest_satisfying_version() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    root.features.insert(
        "native".into(),
        Feature {
            sys_deps: set(&["liburing >=2.6.0, <3"]),
            ..Feature::default()
        },
    );
    let mut a = package("a", "1.0.0");
    a.features.insert(
        "native".into(),
        Feature {
            sys_deps: set(&["liburing <2.8.0"]),
            ..Feature::default()
        },
    );
    root.dependencies.get_mut(&name("a")).unwrap().features = set(&["native"]);
    let mut req = request(root, vec![a]);
    req.features = set(&["native"]);
    req.catalog.system_packages = ["2.6.0", "2.7.0", "2.9.0"]
        .map(|v| SystemSelection {
            name: "liburing".into(),
            version: Version::parse(v).unwrap(),
        })
        .to_vec();
    let result = resolve(&req).unwrap();
    assert_eq!(result.system_packages[0].version, Version::new(2, 7, 0));
    req.catalog
        .system_packages
        .retain(|s| s.version == Version::new(2, 9, 0));
    assert_eq!(resolve(&req).unwrap_err().kind, ErrorKind::Unsatisfiable);
}

#[test]
fn native_conflict_backtracks_to_different_package_version() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    root.dependencies.get_mut(&name("a")).unwrap().features = set(&["native"]);
    let mut newest = package("a", "1.1.0");
    newest.features.insert(
        "native".into(),
        Feature {
            sys_deps: set(&["liburing ^3"]),
            ..Feature::default()
        },
    );
    let mut old = newest.clone();
    old.package.version = Version::new(1, 0, 0);
    old.features.get_mut("native").unwrap().sys_deps = set(&["liburing ^2"]);
    let mut req = request(root, vec![newest, old]);
    req.catalog.system_packages.push(SystemSelection {
        name: "liburing".into(),
        version: Version::new(2, 6, 0),
    });
    assert_eq!(
        selected(&resolve(&req).unwrap(), "a"),
        Version::new(1, 0, 0)
    );
}

#[test]
fn invalid_candidates_and_duplicate_identities_fail_closed() {
    let root = package("root", "1.0.0");
    let a = package("a", "1.0.0");
    assert_eq!(
        resolve(&request(root.clone(), vec![a.clone(), a.clone()]))
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    let mut bad = a;
    bad.features.insert(
        "native".into(),
        Feature {
            sys_deps: set(&["liburing 2:2.6-ubuntu"]),
            ..Feature::default()
        },
    );
    assert_eq!(
        resolve(&request(root.clone(), vec![bad])).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
    let mut req = request(root, vec![]);
    req.catalog.system_packages.push(SystemSelection {
        name: "/bad".into(),
        version: Version::new(1, 0, 0),
    });
    assert_eq!(resolve(&req).unwrap_err().kind, ErrorKind::InvalidInput);
    req.catalog.system_packages[0].name = "valid".into();
    req.catalog
        .system_packages
        .push(req.catalog.system_packages[0].clone());
    assert_eq!(resolve(&req).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn schema_unknown_fields_and_root_collisions_are_errors() {
    let mut req = request(package("root", "1.0.0"), vec![]);
    req.catalog.schema_version = 99;
    assert_eq!(resolve(&req).unwrap_err().kind, ErrorKind::InvalidInput);
    let value = serde_json::json!({"schema_version":1,"packages":[],"network":true});
    assert!(serde_json::from_value::<Catalog>(value).is_err());
    req.catalog.schema_version = 1;
    let mut changed_root = req.root.clone();
    changed_root.package.edition = "other".into();
    req.catalog.packages.push(changed_root);
    assert_eq!(resolve(&req).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn output_and_conflicts_ignore_catalog_order() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    let mut req = request(
        root,
        vec![
            package("a", "1.0.0"),
            package("a", "1.2.0"),
            package("a", "1.1.0"),
        ],
    );
    let first = resolve(&req).unwrap();
    req.catalog.packages.reverse();
    assert_eq!(first, resolve(&req).unwrap());
    for p in &mut req.catalog.packages {
        dep(p, "missing", "^1");
    }
    let first = resolve(&req).unwrap_err();
    req.catalog.packages.reverse();
    assert_eq!(first, resolve(&req).unwrap_err());
    let json = serde_json::to_string(&first).unwrap();
    assert_eq!(first, serde_json::from_str(&json).unwrap());
}

#[test]
fn distinguishes_exhaustive_failure_from_search_budget() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    let mut bad = package("a", "1.1.0");
    dep(&mut bad, "missing", "^1");
    let mut req = request(root, vec![bad, package("a", "1.0.0")]);
    req.max_decisions = 0;
    let e = resolve(&req).unwrap_err();
    assert_eq!((e.kind, e.decisions), (ErrorKind::BudgetExhausted, 0));
    req.max_decisions = 1;
    assert_eq!(resolve(&req).unwrap_err().kind, ErrorKind::BudgetExhausted);
    req.max_decisions = 2;
    assert!(resolve(&req).is_ok());
    req.catalog.packages.pop();
    req.max_decisions = 1;
    let e = resolve(&req).unwrap_err();
    assert_eq!((e.kind, e.decisions), (ErrorKind::Unsatisfiable, 1));
    let mut root_only = request(package("root", "1.0.0"), vec![]);
    root_only.max_decisions = 0;
    assert!(resolve(&root_only).is_ok());
}

#[test]
fn optimization_and_attestation_requirements_are_not_silently_ignored() {
    let mut req = request(package("root", "1.0.0"), vec![]);
    req.root.resolver = Some(Resolver {
        policy: ResolverPolicy {
            optimize: vec!["zero_cve".into()],
            ..ResolverPolicy::default()
        },
    });
    assert_eq!(
        resolve(&req).unwrap_err().kind,
        ErrorKind::UnsupportedPolicy
    );
    req.root.resolver.as_mut().unwrap().policy.optimize.clear();
    req.root
        .resolver
        .as_mut()
        .unwrap()
        .policy
        .min_repro_attestations = Some(2);
    assert_eq!(
        resolve(&req).unwrap_err().kind,
        ErrorKind::UnsupportedPolicy
    );
}

#[test]
fn root_dependency_cycle_and_feature_cycle_terminate() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    root.features.insert(
        "one".into(),
        Feature {
            features: set(&["two"]),
            ..Feature::default()
        },
    );
    root.features.insert(
        "two".into(),
        Feature {
            features: set(&["one"]),
            ..Feature::default()
        },
    );
    let mut a = package("a", "1.0.0");
    dep(&mut a, "root", "^1");
    a.dependencies.get_mut(&name("root")).unwrap().features = set(&["one"]);
    let result = resolve(&request(root, vec![a])).unwrap();
    assert_eq!(result.packages.len(), 2);
    assert_eq!(
        result
            .packages
            .iter()
            .find(|p| p.manifest.package.name == name("root"))
            .unwrap()
            .features,
        set(&["one", "two"])
    );
}

#[test]
fn unsupported_dependency_policies_can_be_avoided_but_never_accepted() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    let mut newest = package("a", "1.1.0");
    newest.resolver = Some(serde_json::from_value(serde_json::json!({"policy": {}})).unwrap());
    assert!(
        newest
            .resolver
            .as_ref()
            .unwrap()
            .policy
            .block_unvetted_namespaces
    );
    let mut req = request(root, vec![newest, package("a", "1.0.0")]);
    assert_eq!(
        selected(&resolve(&req).unwrap(), "a"),
        Version::new(1, 0, 0)
    );
    req.catalog.packages.pop();
    assert_eq!(
        resolve(&req).unwrap_err().kind,
        ErrorKind::UnsupportedPolicy
    );
    req.root.resolver = req.catalog.packages[0].resolver.clone();
    let error = resolve(&req).unwrap_err();
    assert_eq!(
        (error.kind, error.decisions),
        (ErrorKind::UnsupportedPolicy, 0)
    );
    let mut root_only = request(package("root", "1.0.0"), req.catalog.packages);
    // An unreachable valid candidate's policy does not block resolution.
    root_only.max_decisions = 0;
    assert!(resolve(&root_only).is_ok());
}

#[test]
fn dependency_chain_uses_no_recursive_search_stack() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let count = 256;
            let mut root = package("root", "1.0.0");
            dep(&mut root, "node0", "^1");
            let packages = (0..count)
                .map(|index| {
                    let mut node = package(&format!("node{index}"), "1.0.0");
                    if index + 1 < count {
                        dep(&mut node, &format!("node{}", index + 1), "^1");
                    }
                    node
                })
                .collect();
            let mut req = request(root, packages);
            req.max_decisions = count;
            assert_eq!(resolve(&req).unwrap().packages.len(), count + 1);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn incompatible_root_cycle_cannot_replace_source_root() {
    let mut root = package("root", "1.0.0");
    dep(&mut root, "a", "^1");
    let mut a = package("a", "1.0.0");
    dep(&mut a, "root", "^2");
    assert_eq!(
        resolve(&request(root, vec![a, package("root", "2.0.0")]))
            .unwrap_err()
            .kind,
        ErrorKind::Unsatisfiable
    );
}
