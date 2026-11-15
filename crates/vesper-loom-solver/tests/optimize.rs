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
