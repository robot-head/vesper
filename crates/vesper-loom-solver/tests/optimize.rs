use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet};
use vesper_loom_calculus::{
    Dependency, Manifest, Package, PackageName, Resolver, ResolverPolicy, SCHEMA_VERSION,
};
use vesper_loom_solver::{
    Catalog, ErrorKind, ResolveRequest,
    optimize::{CandidateMetadata, OptimizeObjective, OptimizeRequest, resolve_optimized},
};

fn pkg(name: &str) -> PackageName {
    PackageName::parse(name).unwrap()
}

fn manifest(
    name: &str,
    version: &str,
    caps: &[&str],
    deps: &[(&str, &str, &[&str])],
    policy: Option<ResolverPolicy>,
) -> Manifest {
    let mut dep_map = BTreeMap::new();
    for (dep_name, req, effects) in deps {
        dep_map.insert(
            pkg(dep_name),
            Dependency {
                version: VersionReq::parse(req).unwrap(),
                effects: effects.iter().map(|s| (*s).to_owned()).collect(),
                optional: false,
                features: BTreeSet::new(),
            },
        );
    }
    Manifest {
        package: Package {
            name: pkg(name),
            version: Version::parse(version).unwrap(),
            edition: "2026".into(),
            license: Some("Apache-2.0".into()),
            capability_ceiling: caps.iter().map(|s| (*s).to_owned()).collect(),
            forbid_unsafe_ffi: true,
        },
        features: BTreeMap::new(),
        dependencies: dep_map,
        build_sandbox: None,
        resolver: policy.map(|policy| Resolver { policy }),
        verification: None,
    }
}

#[test]
fn optimizes_zero_cve_and_minimal_capability_surface_before_staleness() {
    let root = manifest(
        "@app/service",
        "0.1.0",
        &["Net", "Fail"],
        &[("@vendor/codec", "^1.0.0", &["Net", "Fail"])],
        Some(ResolverPolicy {
            optimize: vec![
                "zero_cve".into(),
                "min_capability_surface".into(),
                "min_staleness".into(),
            ],
            min_repro_attestations: Some(2),
            block_unvetted_namespaces: true,
        }),
    );
    // 1.3.0 newest but has 1 CVE; 1.2.0 clean but uses Net+Fail; 1.1.0 clean and pure!
    let c130 = manifest("@vendor/codec", "1.3.0", &["Net", "Fail"], &[], None);
    let c120 = manifest("@vendor/codec", "1.2.0", &["Net", "Fail"], &[], None);
    let c110 = manifest("@vendor/codec", "1.1.0", &[], &[], None);

    let mut metadata = BTreeMap::new();
    metadata.insert(
        (pkg("@vendor/codec"), Version::parse("1.3.0").unwrap()),
        CandidateMetadata {
            cve_count: 1,
            staleness_secs: 10,
            trust_cost: 5,
            repro_attestations: 2,
            vetted_namespace: true,
        },
    );
    metadata.insert(
        (pkg("@vendor/codec"), Version::parse("1.2.0").unwrap()),
        CandidateMetadata {
            cve_count: 0,
            staleness_secs: 100,
            trust_cost: 5,
            repro_attestations: 2,
            vetted_namespace: true,
        },
    );
    metadata.insert(
        (pkg("@vendor/codec"), Version::parse("1.1.0").unwrap()),
        CandidateMetadata {
            cve_count: 0,
            staleness_secs: 500,
            trust_cost: 5,
            repro_attestations: 2,
            vetted_namespace: true,
        },
    );

    let result = resolve_optimized(&OptimizeRequest {
        resolve: ResolveRequest {
            root,
            features: BTreeSet::new(),
            catalog: Catalog {
                schema_version: SCHEMA_VERSION,
                packages: vec![c130, c120, c110],
                system_packages: vec![],
            },
            max_decisions: 1024,
        },
        objectives: vec![],
        metadata,
        max_solutions: 64,
    })
    .unwrap();

    let chosen = result
        .solution
        .packages
        .iter()
        .find(|s| s.manifest.package.name == pkg("@vendor/codec"))
        .unwrap();
    assert_eq!(
        chosen.manifest.package.version,
        Version::parse("1.1.0").unwrap()
    );
    assert_eq!(result.metrics.total_cves, 0);
    assert_eq!(result.metrics.capability_surface, 0);
    assert_eq!(result.evaluated_solutions, 3);
    assert_eq!(
        result.objective_order,
        vec![
            OptimizeObjective::ZeroCve,
            OptimizeObjective::MinCapabilitySurface,
            OptimizeObjective::MinStaleness
        ]
    );
}

#[test]
fn enforces_repro_quorum_and_namespace_vetting_gates() {
    let root = manifest(
        "@app/service",
        "0.1.0",
        &[],
        &[("@vendor/codec", "^1.0.0", &[])],
        Some(ResolverPolicy {
            optimize: vec!["zero_cve".into()],
            min_repro_attestations: Some(2),
            block_unvetted_namespaces: true,
        }),
    );
    let only_one_sig = manifest("@vendor/codec", "1.0.0", &[], &[], None);
    let mut metadata = BTreeMap::new();
    metadata.insert(
        (pkg("@vendor/codec"), Version::parse("1.0.0").unwrap()),
        CandidateMetadata {
            cve_count: 0,
            staleness_secs: 0,
            trust_cost: 1,
            repro_attestations: 1,
            vetted_namespace: true,
        },
    );

    let err = resolve_optimized(&OptimizeRequest {
        resolve: ResolveRequest {
            root,
            features: BTreeSet::new(),
            catalog: Catalog {
                schema_version: SCHEMA_VERSION,
                packages: vec![only_one_sig],
                system_packages: vec![],
            },
            max_decisions: 128,
        },
        objectives: vec![],
        metadata,
        max_solutions: 32,
    })
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsatisfiable);
    assert!(
        err.diagnostics
            .iter()
            .any(|d| d.code == "insufficient_repro_attestations")
    );
}

#[test]
fn enforces_policies_declared_by_selected_dependencies() {
    let root = manifest(
        "@app/service",
        "0.1.0",
        &[],
        &[
            ("@a_leaf/helper", "^1.0.0", &[]),
            ("@z_mid/wrapper", "^1.0.0", &[]),
        ],
        None,
    );
    let mid = manifest(
        "@z_mid/wrapper",
        "1.0.0",
        &[],
        &[("@a_leaf/helper", "^1.0.0", &[])],
        Some(ResolverPolicy {
            optimize: vec![],
            min_repro_attestations: Some(2),
            block_unvetted_namespaces: true,
        }),
    );
    let leaf_bad = manifest("@a_leaf/helper", "1.1.0", &[], &[], None);

    let err = resolve_optimized(&OptimizeRequest {
        resolve: ResolveRequest {
            root: root.clone(),
            features: BTreeSet::new(),
            catalog: Catalog {
                schema_version: SCHEMA_VERSION,
                packages: vec![mid.clone(), leaf_bad.clone()],
                system_packages: vec![],
            },
            max_decisions: 128,
        },
        objectives: vec![OptimizeObjective::MinCapabilitySurface],
        metadata: BTreeMap::new(),
        max_solutions: 32,
    })
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsatisfiable);
    assert!(
        err.diagnostics
            .iter()
            .any(|d| d.code == "insufficient_repro_attestations")
    );

    let leaf_good = manifest("@a_leaf/helper", "1.0.0", &[], &[], None);
    let mut metadata = BTreeMap::new();
    metadata.insert(
        (pkg("@a_leaf/helper"), Version::parse("1.1.0").unwrap()),
        CandidateMetadata {
            cve_count: 0,
            staleness_secs: 0,
            trust_cost: 0,
            repro_attestations: 1,
            vetted_namespace: true,
        },
    );
    metadata.insert(
        (pkg("@a_leaf/helper"), Version::parse("1.0.0").unwrap()),
        CandidateMetadata {
            cve_count: 0,
            staleness_secs: 10,
            trust_cost: 0,
            repro_attestations: 2,
            vetted_namespace: true,
        },
    );
    let ok = resolve_optimized(&OptimizeRequest {
        resolve: ResolveRequest {
            root,
            features: BTreeSet::new(),
            catalog: Catalog {
                schema_version: SCHEMA_VERSION,
                packages: vec![mid, leaf_bad, leaf_good],
                system_packages: vec![],
            },
            max_decisions: 128,
        },
        objectives: vec![OptimizeObjective::MinCapabilitySurface],
        metadata,
        max_solutions: 32,
    })
    .unwrap();
    let chosen_leaf = ok
        .solution
        .packages
        .iter()
        .find(|s| s.manifest.package.name == pkg("@a_leaf/helper"))
        .unwrap();
    assert_eq!(
        chosen_leaf.manifest.package.version,
        Version::parse("1.0.0").unwrap()
    );

    let mid_opt = manifest(
        "@z_mid/wrapper",
        "1.0.0",
        &[],
        &[],
        Some(ResolverPolicy {
            optimize: vec!["min_staleness".into()],
            min_repro_attestations: None,
            block_unvetted_namespaces: false,
        }),
    );
    let root_only_mid = manifest(
        "@app/service",
        "0.1.0",
        &[],
        &[("@z_mid/wrapper", "^1.0.0", &[])],
        None,
    );
    let err_opt = resolve_optimized(&OptimizeRequest {
        resolve: ResolveRequest {
            root: root_only_mid,
            features: BTreeSet::new(),
            catalog: Catalog {
                schema_version: SCHEMA_VERSION,
                packages: vec![mid_opt],
                system_packages: vec![],
            },
            max_decisions: 128,
        },
        objectives: vec![OptimizeObjective::MinCapabilitySurface],
        metadata: BTreeMap::new(),
        max_solutions: 32,
    })
    .unwrap_err();
    assert_eq!(err_opt.kind, ErrorKind::UnsupportedPolicy);
}

#[test]
fn preserves_package_namespaces_when_counting_duplicate_majors() {
    let root = manifest(
        "@app/service",
        "0.1.0",
        &[],
        &[
            ("@alice/codec", "^1.0.0", &[]),
            ("@bob/codec", ">=1.0.0, <3.0.0", &[]),
        ],
        None,
    );
    let alice_1 = manifest("@alice/codec", "1.0.0", &[], &[], None);
    let bob_1 = manifest("@bob/codec", "1.0.0", &[], &[], None);
    let bob_2 = manifest("@bob/codec", "2.0.0", &[], &[], None);

    let mut metadata = BTreeMap::new();
    metadata.insert(
        (pkg("@alice/codec"), Version::parse("1.0.0").unwrap()),
        CandidateMetadata {
            cve_count: 0,
            ..Default::default()
        },
    );
    metadata.insert(
        (pkg("@bob/codec"), Version::parse("1.0.0").unwrap()),
        CandidateMetadata {
            cve_count: 1,
            ..Default::default()
        },
    );
    metadata.insert(
        (pkg("@bob/codec"), Version::parse("2.0.0").unwrap()),
        CandidateMetadata {
            cve_count: 0,
            ..Default::default()
        },
    );

    let res = resolve_optimized(&OptimizeRequest {
        resolve: ResolveRequest {
            root,
            features: BTreeSet::new(),
            catalog: Catalog {
                schema_version: SCHEMA_VERSION,
                packages: vec![alice_1, bob_1, bob_2],
                system_packages: vec![],
            },
            max_decisions: 128,
        },
        objectives: vec![OptimizeObjective::MinDupMajors, OptimizeObjective::ZeroCve],
        metadata,
        max_solutions: 32,
    })
    .unwrap();

    let chosen_bob = res
        .solution
        .packages
        .iter()
        .find(|s| s.manifest.package.name == pkg("@bob/codec"))
        .unwrap();
    assert_eq!(
        chosen_bob.manifest.package.version,
        Version::parse("2.0.0").unwrap()
    );
    assert_eq!(res.metrics.duplicate_majors, 0);
    assert_eq!(res.metrics.total_cves, 0);
}

#[test]
fn excludes_unused_edge_allowances_from_capability_surface() {
    let root = manifest(
        "@app/service",
        "0.1.0",
        &["Net", "Fs", "Fail"],
        &[("@vendor/facade", "^1.0.0", &["Net", "Fs", "Fail"])],
        None,
    );
    let facade_pure = manifest(
        "@vendor/facade",
        "1.1.0",
        &[],
        &[("@vendor/pure_dep", "^1.0.0", &["Net", "Fs"])],
        None,
    );
    let pure_dep = manifest("@vendor/pure_dep", "1.0.0", &[], &[], None);

    let facade_fail = manifest(
        "@vendor/facade",
        "1.0.0",
        &["Fail"],
        &[("@vendor/fail_dep", "^1.0.0", &["Fail"])],
        None,
    );
    let fail_dep = manifest("@vendor/fail_dep", "1.0.0", &["Fail"], &[], None);

    let res = resolve_optimized(&OptimizeRequest {
        resolve: ResolveRequest {
            root,
            features: BTreeSet::new(),
            catalog: Catalog {
                schema_version: SCHEMA_VERSION,
                packages: vec![facade_pure, pure_dep, facade_fail, fail_dep],
                system_packages: vec![],
            },
            max_decisions: 128,
        },
        objectives: vec![OptimizeObjective::MinCapabilitySurface],
        metadata: BTreeMap::new(),
        max_solutions: 32,
    })
    .unwrap();

    let chosen = res
        .solution
        .packages
        .iter()
        .find(|s| s.manifest.package.name == pkg("@vendor/facade"))
        .unwrap();
    assert_eq!(
        chosen.manifest.package.version,
        Version::parse("1.1.0").unwrap()
    );
    assert_eq!(res.metrics.capability_surface, 0);
}

#[test]
fn distinguishes_missing_candidate_metadata_from_zero_cost() {
    let root = manifest(
        "@app/service",
        "0.1.0",
        &[],
        &[("@vendor/codec", "^1.0.0", &[])],
        None,
    );
    let v110_assessed = manifest("@vendor/codec", "1.1.0", &[], &[], None);
    let v120_missing = manifest("@vendor/codec", "1.2.0", &[], &[], None);

    let mut metadata = BTreeMap::new();
    metadata.insert(
        (pkg("@vendor/codec"), Version::parse("1.1.0").unwrap()),
        CandidateMetadata {
            cve_count: 1,
            staleness_secs: 50,
            trust_cost: 5,
            repro_attestations: 0,
            vetted_namespace: false,
        },
    );

    let res = resolve_optimized(&OptimizeRequest {
        resolve: ResolveRequest {
            root: root.clone(),
            features: BTreeSet::new(),
            catalog: Catalog {
                schema_version: SCHEMA_VERSION,
                packages: vec![v110_assessed, v120_missing.clone()],
                system_packages: vec![],
            },
            max_decisions: 128,
        },
        objectives: vec![OptimizeObjective::ZeroCve],
        metadata,
        max_solutions: 32,
    })
    .unwrap();
    let chosen = res
        .solution
        .packages
        .iter()
        .find(|s| s.manifest.package.name == pkg("@vendor/codec"))
        .unwrap();
    assert_eq!(
        chosen.manifest.package.version,
        Version::parse("1.1.0").unwrap()
    );
    assert_eq!(res.metrics.total_cves, 1);
    assert_eq!(res.metrics.unknown_metadata_candidates, 0);

    let err = resolve_optimized(&OptimizeRequest {
        resolve: ResolveRequest {
            root: root.clone(),
            features: BTreeSet::new(),
            catalog: Catalog {
                schema_version: SCHEMA_VERSION,
                packages: vec![v120_missing.clone()],
                system_packages: vec![],
            },
            max_decisions: 128,
        },
        objectives: vec![OptimizeObjective::ZeroCve],
        metadata: BTreeMap::new(),
        max_solutions: 32,
    })
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsatisfiable);
    assert!(
        err.diagnostics
            .iter()
            .any(|d| d.code == "missing_candidate_metadata")
    );

    let cap_res = resolve_optimized(&OptimizeRequest {
        resolve: ResolveRequest {
            root,
            features: BTreeSet::new(),
            catalog: Catalog {
                schema_version: SCHEMA_VERSION,
                packages: vec![v120_missing],
                system_packages: vec![],
            },
            max_decisions: 128,
        },
        objectives: vec![OptimizeObjective::MinCapabilitySurface],
        metadata: BTreeMap::new(),
        max_solutions: 32,
    })
    .unwrap();
    assert_eq!(cap_res.metrics.unknown_metadata_candidates, 1);
}
