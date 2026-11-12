use semver::{Version, VersionReq};
use std::collections::BTreeSet;
use vesper_loom_calculus::*;
fn name(s: &str) -> PackageName {
    PackageName::parse(s).unwrap()
}
fn caps(xs: &[&str]) -> BTreeSet<String> {
    xs.iter().map(|s| (*s).into()).collect()
}
fn manifest(n: &str, ceiling: &[&str]) -> Manifest {
    parse_manifest(&format!("[package]\nname = \"{n}\"\nversion = \"1.0.0\"\nedition = \"2026\"\ncapability_ceiling = {:?}\n", ceiling)).unwrap()
}
fn dep(effects: &[&str]) -> Dependency {
    Dependency {
        version: VersionReq::parse("^1").unwrap(),
        effects: caps(effects),
        optional: false,
        features: BTreeSet::new(),
    }
}
fn selected(manifest: Manifest) -> Selection {
    Selection {
        manifest,
        features: BTreeSet::new(),
    }
}
fn solution(root: &str, packages: Vec<Manifest>) -> Solution {
    Solution {
        schema_version: SCHEMA_VERSION,
        roots: vec![RootRequirement {
            name: name(root),
            version: VersionReq::parse("^1").unwrap(),
            features: BTreeSet::new(),
        }],
        packages: packages.into_iter().map(selected).collect(),
        system_packages: vec![],
    }
}
fn has(v: &Validation, code: Code) -> bool {
    v.diagnostics.iter().any(|e| e.code == code)
}

#[test]
fn documented_manifest_is_declarative_and_secure_by_default() {
    let text = r#"
[package]
name = "@robot-head/krabka-wal"
version = "0.4.0"
edition = "2026"
license = "Apache-2.0"
capability_ceiling = ["Storage", "Net", "Clock", "Fail"]
forbid_unsafe_ffi = true
[features]
io_uring = { enables_effects = ["Storage"], sys_deps = ["liburing ^2.6.0"] }
compression = { deps = ["@vesper-compression/lz4-pure"] }
[dependencies]
"std/bytes" = { version = "^1.3.0", effects = [] }
"@vesper-compression/lz4-pure" = { version = "^2.1.0", effects = [], optional = true }
"@vesper-io/uring-reactor" = { version = "^0.9.2", effects = ["Storage", "Fail"] }
[build-sandbox]
script = "build.vsp"
wasi_read_paths = ["proto/"]
fuel_limit = 50_000_000
allow_network = false
[resolver.policy]
optimize = ["zero_cve", "min_capability_surface", "min_dup_majors", "min_staleness"]
min_repro_attestations = 2
block_unvetted_namespaces = true
[verification]
export_lean4_specs = ["src/wal_txn.vsp"]
"#;
    let parsed = parse_manifest(text).unwrap();
    assert!(parsed.dependencies[&name("@vesper-compression/lz4-pure")].optional);
    assert!(!parsed.build_sandbox.unwrap().allow_network);
    assert!(manifest("@org/pure", &[]).package.forbid_unsafe_ffi);
    for invalid in [
        "http",
        "org/pkg",
        "@org",
        "@org/pkg/extra",
        "@Org/pkg",
        "@org/../pkg",
        "@org/-pkg",
        "@org/pkg name",
    ] {
        assert!(PackageName::parse(invalid).is_err(), "{invalid}");
    }
    assert!(PackageName::parse("std/io").is_ok());
    assert!(parse_manifest(&text.replace("enables_effects", "disables_effects")).is_err());
}

#[test]
fn root_inclusion_dependency_closure_and_version_uniqueness_are_independent() {
    let root = manifest("@org/root", &[]);
    let missing = solution("@org/root", vec![]);
    assert!(has(&validate_solution(&missing), Code::MissingRoot));
    let mut root_with_dep = root.clone();
    root_with_dep
        .dependencies
        .insert(name("@org/dep"), dep(&[]));
    assert!(has(
        &validate_solution(&solution("@org/root", vec![root_with_dep])),
        Code::MissingDependency
    ));
    let duplicate = solution("@org/root", vec![root.clone(), root]);
    let checked = validate_solution(&duplicate);
    assert!(has(&checked, Code::DuplicatePackage));
    assert!(checked.graph.is_none());
}

#[test]
fn exact_selections_must_satisfy_root_and_dependency_requirements() {
    let mut root = manifest("@org/root", &[]);
    root.dependencies.insert(name("@org/dep"), dep(&[]));
    let mut target = manifest("@org/dep", &[]);
    target.package.version = Version::parse("2.0.0").unwrap();
    let mut s = solution("@org/root", vec![root, target]);
    s.roots[0].version = VersionReq::parse("^3").unwrap();
    let checked = validate_solution(&s);
    assert_eq!(
        checked
            .diagnostics
            .iter()
            .filter(|d| d.code == Code::VersionMismatch)
            .count(),
        2
    );
}

#[test]
fn every_edge_and_ancestor_must_cover_transitive_authority() {
    let mut root = manifest("@org/root", &["Storage"]);
    let mut middle = manifest("@org/middle", &["Storage"]);
    let leaf = manifest("@org/leaf", &["Net"]);
    root.dependencies
        .insert(name("@org/middle"), dep(&["Storage"]));
    middle.dependencies.insert(name("@org/leaf"), dep(&["Net"]));
    let checked = validate_solution(&solution("@org/root", vec![root, middle, leaf]));
    assert!(checked.graph.is_none());
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code == Code::EdgeCapabilityEscalation
                && d.package == Some(name("@org/root")))
    );
    assert_eq!(
        checked
            .diagnostics
            .iter()
            .filter(|d| d.code == Code::CapabilityEscalation)
            .count(),
        2
    );
}

#[test]
fn additive_features_activate_optional_dependencies_and_native_edges() {
    let mut root = manifest("@org/root", &["Storage"]);
    let leaf = manifest("@org/leaf", &["Storage"]);
    let mut edge = dep(&["Storage"]);
    edge.optional = true;
    root.dependencies.insert(name("@org/leaf"), edge);
    root.features.insert(
        "storage".into(),
        Feature {
            enables_effects: caps(&["Storage"]),
            deps: [name("@org/leaf")].into(),
            features: caps(&["base"]),
            sys_deps: caps(&["liburing ^2.6"]),
        },
    );
    root.features.insert(
        "base".into(),
        Feature {
            features: caps(&["storage"]),
            ..Default::default()
        },
    );
    let mut s = solution("@org/root", vec![root, leaf]);
    let inactive = validate_solution(&s);
    assert!(inactive.diagnostics.is_empty());
    assert_eq!(inactive.unused_selections, [name("@org/leaf")].into());
    s.roots[0].features.insert("storage".into());
    assert!(has(&validate_solution(&s), Code::MissingSystemPackage));
    s.system_packages.push(SystemSelection {
        name: "liburing".into(),
        version: Version::parse("2.6.1").unwrap(),
    });
    let active = validate_solution(&s);
    assert!(active.diagnostics.is_empty(), "{:?}", active.diagnostics);
    let graph = active.graph.unwrap();
    assert_eq!(
        graph.packages[&name("@org/root")].features,
        caps(&["base", "storage"])
    );
    assert_eq!(graph.system_packages.len(), 1);
    assert_eq!(graph.packages.len(), 2);
}

#[test]
fn dependency_requested_features_propagate_to_a_fixed_point_on_cycles() {
    let mut a = manifest("@org/a", &["Clock"]);
    let mut b = manifest("@org/b", &["Clock"]);
    a.features.insert("a".into(), Feature::default());
    b.features.insert(
        "b".into(),
        Feature {
            enables_effects: caps(&["Clock"]),
            ..Default::default()
        },
    );
    let mut ab = dep(&["Clock"]);
    ab.features.insert("b".into());
    let mut ba = dep(&["Clock"]);
    ba.features.insert("a".into());
    a.dependencies.insert(name("@org/b"), ab);
    b.dependencies.insert(name("@org/a"), ba);
    let checked = validate_solution(&solution("@org/a", vec![a, b]));
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let graph = checked.graph.unwrap();
    assert_eq!(graph.packages[&name("@org/a")].features, caps(&["a"]));
    assert_eq!(graph.packages[&name("@org/b")].features, caps(&["b"]));
    assert_eq!(
        graph.packages[&name("@org/a")].required_capabilities,
        caps(&["Clock"])
    );
}

#[test]
fn active_features_cannot_raise_the_package_budget() {
    let mut root = manifest("@org/root", &[]);
    root.features.insert(
        "network".into(),
        Feature {
            enables_effects: caps(&["Net"]),
            ..Default::default()
        },
    );
    let mut s = solution("@org/root", vec![root]);
    assert!(validate_solution(&s).diagnostics.is_empty());
    s.roots[0].features.insert("network".into());
    assert!(has(&validate_solution(&s), Code::CapabilityEscalation));
}

#[test]
fn unselected_optional_dependencies_do_not_add_authority() {
    let mut root = manifest("@org/root", &[]);
    let mut edge = dep(&[]);
    edge.optional = true;
    root.dependencies.insert(name("@org/network"), edge);
    let checked = validate_solution(&solution(
        "@org/root",
        vec![root, manifest("@org/network", &["Net"])],
    ));
    let graph = checked.graph.unwrap();
    assert_eq!(graph.packages.len(), 1);
    assert!(
        graph.packages[&name("@org/root")]
            .required_capabilities
            .is_empty()
    );
}

#[test]
fn foreign_authority_is_detected_through_dependencies() {
    let mut root = manifest("@org/root", &["Ffi[codec]"]);
    root.dependencies
        .insert(name("@org/codec"), dep(&["Ffi[codec]"]));
    let mut codec = manifest("@org/codec", &["Ffi[codec]"]);
    codec.package.forbid_unsafe_ffi = false;
    let mut s = solution("@org/root", vec![root, codec]);
    assert!(has(&validate_solution(&s), Code::UnsafeFfiForbidden));
    s.packages[0].manifest.package.forbid_unsafe_ffi = false;
    assert!(validate_solution(&s).diagnostics.is_empty());
}

#[test]
fn input_permutations_produce_identical_graph_bytes_and_diagnostics() {
    let mut root = manifest("@org/root", &["Net"]);
    root.dependencies.insert(name("@org/dep"), dep(&["Net"]));
    let mut s = solution("@org/root", vec![root, manifest("@org/dep", &["Net"])]);
    let one = validate_solution(&s);
    s.packages.reverse();
    assert_eq!(one, validate_solution(&s));
    let graph = one.graph.unwrap();
    let json = graph.canonical_json().unwrap();
    assert_eq!(serde_json::from_str::<PackageGraph>(&json).unwrap(), graph);
    s.packages
        .iter_mut()
        .find(|p| p.manifest.package.name == name("@org/root"))
        .unwrap()
        .manifest
        .package
        .capability_ceiling
        .clear();
    let one = validate_solution(&s);
    s.packages.reverse();
    assert_eq!(one, validate_solution(&s));
}

#[test]
fn malformed_or_unknown_manifest_features_fail_closed() {
    let mut m = manifest("@org/root", &[]);
    m.features.insert(
        "a".into(),
        Feature {
            features: caps(&["missing"]),
            deps: [name("@org/missing")].into(),
            sys_deps: caps(&["@org/native ^1"]),
            ..Default::default()
        },
    );
    let errors = validate_manifest(&m);
    assert!(errors.iter().any(|d| d.code == Code::UnknownFeature));
    assert!(errors.iter().any(|d| d.code == Code::UnknownDependency));
    assert!(
        errors
            .iter()
            .any(|d| d.code == Code::InvalidSystemRequirement)
    );
    let mut s = solution("@org/root", vec![manifest("@org/root", &[])]);
    s.roots[0].features.insert("not-defined".into());
    assert!(has(&validate_solution(&s), Code::UnknownFeature));
    s.schema_version += 1;
    assert!(has(&validate_solution(&s), Code::ManifestSyntax));
}

#[test]
fn duplicates_are_rejected_independently_of_input_order() {
    let mut a = manifest("@org/root", &[]);
    let b = manifest("@org/root", &["Net"]);
    a.package.version = Version::parse("2.0.0").unwrap();
    let mut s = solution("@org/root", vec![a, b]);
    s.system_packages = vec![
        SystemSelection {
            name: "libx".into(),
            version: Version::parse("1.0.0").unwrap(),
        },
        SystemSelection {
            name: "libx".into(),
            version: Version::parse("2.0.0").unwrap(),
        },
    ];
    let one = validate_solution(&s);
    s.packages.reverse();
    s.system_packages.reverse();
    assert_eq!(one, validate_solution(&s));
    assert!(has(&one, Code::DuplicatePackage));
    assert!(has(&one, Code::DuplicateSystemPackage));
}

#[test]
fn native_requirements_use_the_native_namespace_and_exact_selection() {
    let mut root = manifest("@org/root", &[]);
    root.features.insert(
        "native".into(),
        Feature {
            sys_deps: caps(&["libx ^2"]),
            ..Default::default()
        },
    );
    let mut s = solution("@org/root", vec![root]);
    s.roots[0].features.insert("native".into());
    s.system_packages.push(SystemSelection {
        name: "libx".into(),
        version: Version::parse("1.0.0").unwrap(),
    });
    assert!(has(&validate_solution(&s), Code::VersionMismatch));
    s.system_packages[0].version = Version::parse("2.1.0").unwrap();
    assert!(validate_solution(&s).diagnostics.is_empty());
}

#[test]
fn sandbox_configuration_cannot_request_ambient_network_or_path_escape() {
    let mut root = manifest("@org/root", &[]);
    root.build_sandbox = Some(BuildSandbox {
        script: Some("../outside.vsp".into()),
        wasi_read_paths: caps(&["C:\\secrets", "/absolute"]),
        fuel_limit: Some(10),
        allow_network: true,
    });
    let checked = validate_solution(&solution("@org/root", vec![root]));
    assert_eq!(
        checked
            .diagnostics
            .iter()
            .filter(|d| d.code == Code::UnsupportedBuildPolicy)
            .count(),
        4
    );
    assert!(checked.graph.is_none());
}

#[test]
fn schema_rejects_unrecognized_solution_fields() {
    let s = solution("@org/root", vec![manifest("@org/root", &[])]);
    let mut json = serde_json::to_value(&s).unwrap();
    json["override_capability_check"] = true.into();
    assert!(serde_json::from_value::<Solution>(json).is_err());
}

#[test]
fn feature_union_never_removes_an_existing_dependency_or_feature() {
    let mut root = manifest("@org/root", &["Clock", "Storage"]);
    let mut optional = dep(&["Storage"]);
    optional.optional = true;
    root.dependencies.insert(name("@org/storage"), optional);
    root.features.insert(
        "clock".into(),
        Feature {
            enables_effects: caps(&["Clock"]),
            ..Default::default()
        },
    );
    root.features.insert(
        "storage".into(),
        Feature {
            deps: [name("@org/storage")].into(),
            ..Default::default()
        },
    );
    let packages = vec![root, manifest("@org/storage", &["Storage"])];
    for old in 0..4 {
        for new in 0..4 {
            if old & new != old {
                continue;
            }
            let graph = |bits| {
                let mut s = solution("@org/root", packages.clone());
                if bits & 1 != 0 {
                    s.roots[0].features.insert("clock".into());
                }
                if bits & 2 != 0 {
                    s.roots[0].features.insert("storage".into());
                }
                validate_solution(&s).graph.unwrap()
            };
            let before = graph(old);
            let after = graph(new);
            let a = &before.packages[&name("@org/root")];
            let b = &after.packages[&name("@org/root")];
            assert!(a.features.is_subset(&b.features));
            assert!(a.required_capabilities.is_subset(&b.required_capabilities));
            assert!(
                a.dependencies
                    .iter()
                    .all(|edge| b.dependencies.contains(edge))
            );
        }
    }
}

#[test]
fn foreign_library_arguments_are_not_erased_or_aliased() {
    let mut root = manifest("@org/root", &["Ffi[LibA]"]);
    root.package.forbid_unsafe_ffi = false;
    root.dependencies
        .insert(name("@org/dep"), dep(&["Ffi[LibA]"]));
    let mut target = manifest("@org/dep", &["Ffi[LibB]"]);
    target.package.forbid_unsafe_ffi = false;
    let result = validate_solution(&solution("@org/root", vec![root, target]));
    assert!(has(&result, Code::CapabilityEscalation));
    assert!(has(&result, Code::EdgeCapabilityEscalation));
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("Ffi[LibB]"))
    );
    let mut unsupported = manifest("@org/root", &[]);
    unsupported
        .package
        .capability_ceiling
        .insert("@org/dep::Net".into());
    assert!(
        validate_manifest(&unsupported)
            .iter()
            .any(|d| d.code == Code::InvalidCapability)
    );
}
