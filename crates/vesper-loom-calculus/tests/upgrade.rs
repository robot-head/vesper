use semver::{Version, VersionReq};
use std::collections::BTreeSet;
use vesper_loom_calculus::{
    AuthorityScope, Dependency, Feature, Manifest, Package, PackageName, RootRequirement,
    SCHEMA_VERSION, Selection, Solution, SystemSelection, UpgradeSnapshot, check_upgrade,
    validate_solution,
};

fn name(s: &str) -> PackageName {
    PackageName::parse(format!("@test/{s}")).unwrap()
}
fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|v| (*v).into()).collect()
}
fn package(s: &str, capabilities: &[&str]) -> Selection {
    Selection {
        manifest: Manifest {
            package: Package {
                name: name(s),
                version: Version::new(1, 0, 0),
                edition: "2026".into(),
                license: None,
                capability_ceiling: set(capabilities),
                forbid_unsafe_ffi: true,
            },
            features: Default::default(),
            dependencies: Default::default(),
            build_sandbox: None,
            resolver: None,
            verification: None,
        },
        features: Default::default(),
    }
}
fn dependency(package: &mut Selection, target: &str, capabilities: &[&str]) {
    package.manifest.dependencies.insert(
        name(target),
        Dependency {
            version: VersionReq::parse("^1").unwrap(),
            effects: set(capabilities),
            optional: false,
            features: Default::default(),
        },
    );
}
fn solution(packages: Vec<Selection>) -> Solution {
    Solution {
        schema_version: SCHEMA_VERSION,
        roots: vec![RootRequirement {
            name: name("app"),
            version: VersionReq::parse("^1").unwrap(),
            features: Default::default(),
        }],
        packages,
        system_packages: vec![],
    }
}
fn selected<'a>(solution: &'a mut Solution, s: &str) -> &'a mut Selection {
    solution
        .packages
        .iter_mut()
        .find(|p| p.manifest.package.name == name(s))
        .unwrap()
}
fn valid(solution: &Solution) {
    assert!(validate_solution(solution).diagnostics.is_empty());
}

#[test]
fn unchanged_authority_allows_versions_and_metadata_to_change() {
    let before = solution(vec![package("app", &[])]);
    let mut after = before.clone();
    selected(&mut after, "app").manifest.package.version = Version::new(2, 0, 0);
    selected(&mut after, "app").manifest.package.license = Some("MIT".into());
    after.roots[0].version = VersionReq::parse("^2").unwrap();
    let report = check_upgrade(&before, &after);
    assert!(report.allowed);
    assert_eq!(report.schema_version, 1);
    assert!(report.changes.is_empty());
}

#[test]
fn pure_new_packages_are_allowed_but_privileged_ones_are_not() {
    let before = solution(vec![package("app", &["Net"])]);
    let mut after = before.clone();
    dependency(selected(&mut after, "app"), "new", &[]);
    after.packages.push(package("new", &[]));
    assert!(check_upgrade(&before, &after).allowed);
    selected(&mut after, "new")
        .manifest
        .package
        .capability_ceiling = set(&["Net"]);
    selected(&mut after, "app")
        .manifest
        .dependencies
        .get_mut(&name("new"))
        .unwrap()
        .effects = set(&["Net"]);
    valid(&after);
    let report = check_upgrade(&before, &after);
    assert!(!report.allowed);
    assert!(
        report
            .changes
            .iter()
            .any(|c| c.package == name("new") && c.added == set(&["Net"]))
    );
}

#[test]
fn moving_authority_between_packages_is_blocked_even_with_the_same_root_union() {
    let mut app = package("app", &["Net"]);
    dependency(&mut app, "a", &["Net"]);
    dependency(&mut app, "b", &["Net"]);
    let before = solution(vec![app, package("a", &["Net"]), package("b", &[])]);
    let mut after = before.clone();
    selected(&mut after, "a")
        .manifest
        .package
        .capability_ceiling
        .clear();
    selected(&mut after, "b")
        .manifest
        .package
        .capability_ceiling = set(&["Net"]);
    let old = validate_solution(&before).graph.unwrap();
    let new = validate_solution(&after).graph.unwrap();
    assert_eq!(
        old.packages[&name("app")].required_capabilities,
        new.packages[&name("app")].required_capabilities
    );
    let report = check_upgrade(&before, &after);
    assert!(!report.allowed);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.package == Some(name("b")))
    );
}

#[test]
fn unused_edge_budget_increases_are_still_permission_increases() {
    let mut app = package("app", &[]);
    dependency(&mut app, "a", &[]);
    let before = solution(vec![app, package("a", &[])]);
    let mut after = before.clone();
    selected(&mut after, "app")
        .manifest
        .dependencies
        .get_mut(&name("a"))
        .unwrap()
        .effects = set(&["Net"]);
    valid(&after);
    let report = check_upgrade(&before, &after);
    assert!(!report.allowed);
    assert_eq!(report.changes.len(), 1);
    assert_eq!(report.changes[0].scope, AuthorityScope::DependencyCeiling);
    assert_eq!(report.changes[0].dependency, Some(name("a")));
    assert_eq!(report.changes[0].added, set(&["Net"]));
}

#[test]
fn reductions_and_removed_edges_are_allowed_and_remain_reviewable() {
    let mut app = package("app", &["Net"]);
    dependency(&mut app, "a", &["Net"]);
    let before = solution(vec![app, package("a", &["Net"])]);
    let after = solution(vec![package("app", &[])]);
    let report = check_upgrade(&before, &after);
    assert!(report.allowed);
    assert!(
        report
            .changes
            .iter()
            .any(|c| c.scope == AuthorityScope::DependencyCeiling && c.removed == set(&["Net"]))
    );
    assert!(
        report
            .changes
            .iter()
            .any(|c| c.package == name("a") && c.scope == AuthorityScope::RequiredCapabilities)
    );
    assert!(report.changes.iter().all(|c| c.added.is_empty()));
}

#[test]
fn typed_effect_arguments_are_distinct() {
    let mut app = package("app", &["Ffi[LibA]", "Ffi[LibB]"]);
    app.manifest.package.forbid_unsafe_ffi = false;
    dependency(&mut app, "a", &["Ffi[LibA]", "Ffi[LibB]"]);
    let mut a = package("a", &["Ffi[LibA]"]);
    a.manifest.package.forbid_unsafe_ffi = false;
    let before = solution(vec![app, a]);
    let mut after = before.clone();
    selected(&mut after, "a")
        .manifest
        .package
        .capability_ceiling = set(&["Ffi[LibB]"]);
    valid(&before);
    valid(&after);
    let report = check_upgrade(&before, &after);
    assert!(!report.allowed);
    assert!(report.changes.iter().any(|c| c.package == name("a")
        && c.added == set(&["Ffi[LibB]"])
        && c.removed == set(&["Ffi[LibA]"])));
}

#[test]
fn unsafe_ffi_policy_relaxation_is_rejected_for_existing_and_new_packages() {
    let before = solution(vec![package("app", &[])]);
    let mut after = before.clone();
    selected(&mut after, "app")
        .manifest
        .package
        .forbid_unsafe_ffi = false;
    let report = check_upgrade(&before, &after);
    assert!(!report.allowed);
    assert_eq!(report.changes[0].scope, AuthorityScope::UnsafeFfiPolicy);
    assert!(report.changes[0].unsafe_ffi_relaxed);
    assert_eq!(report.changes[0].before_forbid_unsafe_ffi, Some(true));
    assert_eq!(report.changes[0].after_forbid_unsafe_ffi, Some(false));
    assert!(check_upgrade(&after, &before).allowed);
    let mut added = before.clone();
    dependency(selected(&mut added, "app"), "new", &[]);
    let mut extra = package("new", &[]);
    extra.manifest.package.forbid_unsafe_ffi = false;
    added.packages.push(extra);
    assert!(!check_upgrade(&before, &added).allowed);
}

#[test]
fn both_snapshots_validate_before_any_comparison_and_identify_their_side() {
    let valid = solution(vec![package("app", &[])]);
    let mut before = valid.clone();
    before.schema_version = 999;
    let mut after = valid;
    after.packages.push(after.packages[0].clone());
    let report = check_upgrade(&before, &after);
    assert!(!report.allowed);
    assert!(report.changes.is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.snapshot == Some(UpgradeSnapshot::Baseline))
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.snapshot == Some(UpgradeSnapshot::Candidate))
    );
}

#[test]
fn unrelated_or_empty_root_sets_do_not_form_an_upgrade() {
    let before = solution(vec![package("app", &[])]);
    let mut after = before.clone();
    after.packages[0].manifest.package.name = name("other");
    after.roots[0].name = name("other");
    let report = check_upgrade(&before, &after);
    assert!(!report.allowed);
    assert_eq!(report.diagnostics[0].code, "root_mismatch");
    let mut empty = before;
    empty.roots.clear();
    assert!(!check_upgrade(&empty, &empty).allowed);
}

#[test]
fn valid_unreachable_metadata_does_not_contribute_authority_but_is_validated() {
    let before = solution(vec![package("app", &[])]);
    let mut after = before.clone();
    after.packages.push(package("unused", &["Net"]));
    assert!(check_upgrade(&before, &after).allowed);
    selected(&mut after, "unused").manifest.features.insert(
        "invalid".into(),
        Feature {
            features: set(&["unknown"]),
            ..Default::default()
        },
    );
    let report = check_upgrade(&before, &after);
    assert!(!report.allowed);
    assert!(report.changes.is_empty());
    assert_eq!(
        report.diagnostics[0].snapshot,
        Some(UpgradeSnapshot::Candidate)
    );
}

#[test]
fn cycles_terminate_and_input_order_does_not_change_diagnostics_or_changes() {
    let mut app = package("app", &[]);
    dependency(&mut app, "a", &[]);
    let mut a = package("a", &[]);
    dependency(&mut a, "b", &[]);
    let mut b = package("b", &[]);
    dependency(&mut b, "a", &[]);
    let before = solution(vec![app, a, b]);
    let mut after = before.clone();
    selected(&mut after, "b")
        .manifest
        .dependencies
        .get_mut(&name("a"))
        .unwrap()
        .effects = set(&["Net"]);
    let report = check_upgrade(&before, &after);
    assert!(!report.allowed);
    let mut old_reordered = before;
    old_reordered.packages.reverse();
    after.packages.reverse();
    assert_eq!(report, check_upgrade(&old_reordered, &after));
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(report, serde_json::from_str(&json).unwrap());
}

#[test]
fn native_versions_alone_do_not_imply_an_authority_change() {
    let mut app = package("app", &[]);
    app.manifest.features.insert(
        "native".into(),
        Feature {
            sys_deps: set(&["zlib ^1"]),
            ..Default::default()
        },
    );
    app.features = set(&["native"]);
    let mut before = solution(vec![app]);
    before.system_packages.push(SystemSelection {
        name: "zlib".into(),
        version: Version::new(1, 0, 0),
    });
    let mut after = before.clone();
    after.system_packages[0].version = Version::new(1, 1, 0);
    valid(&before);
    valid(&after);
    let report = check_upgrade(&before, &after);
    assert!(report.allowed);
    assert!(report.changes.is_empty());
}
