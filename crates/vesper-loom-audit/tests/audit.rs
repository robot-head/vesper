use semver::{Version, VersionReq};
use serde_json::json;
use std::collections::BTreeSet;
use vesper_loom_audit::*;
use vesper_loom_calculus::{
    Dependency, Feature, PackageName, RootRequirement, Selection, Solution, SystemSelection,
    parse_manifest,
};

fn name(text: &str) -> PackageName {
    PackageName::parse(text).unwrap()
}
fn id(text: &str) -> PackageIdentity {
    PackageIdentity::Vesper { name: name(text) }
}
fn native(text: &str) -> PackageIdentity {
    PackageIdentity::Native { name: text.into() }
}
fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}
fn package(text: &str) -> Selection {
    Selection { manifest: parse_manifest(&format!("[package]\nname = \"{text}\"\nversion = \"1.0.0\"\nedition = \"2026\"\nlicense = \"MIT\"\n")).unwrap(), features: BTreeSet::new() }
}
fn dependency(package: &mut Selection, target: &str, optional: bool) {
    package.manifest.dependencies.insert(
        name(target),
        Dependency {
            version: VersionReq::parse("^1").unwrap(),
            effects: BTreeSet::new(),
            features: BTreeSet::new(),
            optional,
        },
    );
}
fn root(text: &str) -> RootRequirement {
    RootRequirement {
        name: name(text),
        version: VersionReq::parse("^1").unwrap(),
        features: BTreeSet::new(),
    }
}
fn solution() -> Solution {
    Solution {
        schema_version: 1,
        roots: vec![root("@org/root")],
        packages: vec![package("@org/root")],
        system_packages: vec![],
    }
}
fn catalog() -> Catalog {
    Catalog {
        schema_version: 1,
        catalog_id: "local-1".into(),
        advisories: vec![],
        native_licenses: vec![],
    }
}
fn policy() -> Policy {
    Policy {
        schema_version: 1,
        minimum_severity: Severity::High,
        allowed_licenses: vec!["MIT".into(), "Apache-2.0".into()],
    }
}
fn advisory(id: &str, package: PackageIdentity) -> Advisory {
    Advisory {
        id: id.into(),
        package,
        severity: Severity::High,
        summary: "local test declaration".into(),
        affected: vec![AffectedRange {
            introduced: version("1.0.0"),
            fixed: Some(version("2.0.0")),
            include_prerelease: true,
        }],
    }
}
fn run(s: &Solution, c: &Catalog, p: &Policy) -> AuditReport {
    audit(s, c, p, &Limits::default()).unwrap()
}

#[test]
fn reachable_transitive_vesper_and_native_findings_have_versioned_paths() {
    let mut s = solution();
    dependency(&mut s.packages[0], "@org/middle", false);
    let mut middle = package("@org/middle");
    dependency(&mut middle, "@org/leaf", false);
    let mut leaf = package("@org/leaf");
    leaf.features.insert("native".into());
    leaf.manifest.features.insert(
        "native".into(),
        Feature {
            sys_deps: ["leaf ^1".into()].into(),
            ..Feature::default()
        },
    );
    s.packages.extend([middle, leaf]);
    s.system_packages.push(SystemSelection {
        name: "leaf".into(),
        version: version("1.0.0"),
    });
    let mut c = catalog();
    c.advisories = vec![
        advisory("V-1", id("@org/leaf")),
        advisory("N-1", native("leaf")),
    ];
    c.native_licenses.push(NativeLicense {
        name: "leaf".into(),
        version: version("1.0.0"),
        expression: "MIT".into(),
    });
    let report = run(&s, &c, &policy());
    assert_eq!(report.outcome, Outcome::Findings);
    assert!(report.has_policy_violations);
    assert_eq!(
        (
            report.reachable_vesper_packages,
            report.reachable_native_packages
        ),
        (3, 1)
    );
    assert_eq!(report.findings.len(), 2);
    let vesper = &report.findings[0];
    assert_eq!(vesper.package.identity, id("@org/leaf"));
    assert_eq!(
        vesper.paths[0]
            .iter()
            .map(|p| p.identity.clone())
            .collect::<Vec<_>>(),
        [id("@org/root"), id("@org/middle"), id("@org/leaf")]
    );
    assert!(
        vesper.paths[0]
            .iter()
            .all(|p| p.version == version("1.0.0"))
    );
    let native = &report.findings[1];
    assert_eq!(native.package.identity, crate::native("leaf"));
    assert_eq!(native.paths[0].len(), 4);
    assert_eq!(native.paths[0][2].identity, id("@org/leaf"));
}

#[test]
fn inactive_optional_and_unused_native_selections_do_not_claim_reachability() {
    let mut s = solution();
    dependency(&mut s.packages[0], "@org/unused", true);
    let mut unused = package("@org/unused");
    unused.manifest.package.license = None;
    s.packages.push(unused);
    s.system_packages.push(SystemSelection {
        name: "unused".into(),
        version: version("1.0.0"),
    });
    let mut c = catalog();
    c.advisories = vec![
        advisory("A", id("@org/unused")),
        advisory("B", native("unused")),
    ];
    let report = run(&s, &c, &policy());
    assert_eq!(report.outcome, Outcome::Clean);
    assert_eq!(
        (
            report.reachable_vesper_packages,
            report.reachable_native_packages
        ),
        (1, 0)
    );
    assert!(report.findings.is_empty());
    s.packages[0].manifest.features.insert(
        "enable".into(),
        Feature {
            deps: [name("@org/unused")].into(),
            ..Feature::default()
        },
    );
    s.roots[0].features.insert("enable".into());
    let report = run(&s, &c, &policy());
    assert_eq!(report.outcome, Outcome::Unknown);
    assert!(report.has_policy_violations);
    assert_eq!(report.findings.len(), 2);
}

#[test]
fn cycles_diamonds_and_multiple_roots_choose_shortest_stable_paths() {
    let mut s = solution();
    dependency(&mut s.packages[0], "@org/b", false);
    dependency(&mut s.packages[0], "@org/a", false);
    let mut a = package("@org/a");
    dependency(&mut a, "@org/leaf", false);
    let mut b = package("@org/b");
    dependency(&mut b, "@org/leaf", false);
    let mut leaf = package("@org/leaf");
    dependency(&mut leaf, "@org/root", false);
    let mut other = package("@org/other");
    dependency(&mut other, "@org/leaf", false);
    s.packages.extend([a, b, leaf, other]);
    s.roots.push(root("@org/other"));
    let mut c = catalog();
    c.advisories.push(advisory("A", id("@org/leaf")));
    let report = run(&s, &c, &policy());
    let paths = &report.findings[0].paths;
    assert_eq!(paths.len(), 2);
    assert_eq!(
        paths[0]
            .iter()
            .map(|p| p.identity.clone())
            .collect::<Vec<_>>(),
        [id("@org/other"), id("@org/leaf")]
    );
    assert_eq!(
        paths[1]
            .iter()
            .map(|p| p.identity.clone())
            .collect::<Vec<_>>(),
        [id("@org/root"), id("@org/a"), id("@org/leaf")]
    );
    s.packages.reverse();
    s.roots.reverse();
    c.advisories.reverse();
    let shuffled = run(&s, &c, &policy());
    assert_eq!(report.findings, shuffled.findings);
    assert_ne!(
        report.context.solution_digest,
        shuffled.context.solution_digest
    );
}

#[test]
fn native_shared_dependency_chooses_shortest_path_per_root() {
    let mut s = solution();
    dependency(&mut s.packages[0], "@org/a", false);
    dependency(&mut s.packages[0], "@org/z", false);
    let mut a = package("@org/a");
    dependency(&mut a, "@org/leaf", false);
    let mut z = package("@org/z");
    let mut leaf = package("@org/leaf");
    for p in [&mut z, &mut leaf] {
        p.features.insert("native".into());
        p.manifest.features.insert(
            "native".into(),
            Feature {
                sys_deps: ["libsame ^1".into()].into(),
                ..Feature::default()
            },
        );
    }
    s.packages.extend([a, z, leaf]);
    s.system_packages.push(SystemSelection {
        name: "libsame".into(),
        version: version("1.0.0"),
    });
    let report = run(&s, &catalog(), &policy());
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].package.identity, native("libsame"));
    assert_eq!(
        report.findings[0].paths[0]
            .iter()
            .map(|p| p.identity.clone())
            .collect::<Vec<_>>(),
        [id("@org/root"), id("@org/z"), native("libsame")]
    );
}

#[test]
fn semver_intervals_include_lower_exclude_fixed_and_ignore_build_metadata() {
    let range = AffectedRange {
        introduced: version("1.2.3+intro"),
        fixed: Some(version("2.0.0+fixed")),
        include_prerelease: true,
    };
    for (v, matches) in [
        ("1.2.2", false),
        ("1.2.3-alpha", false),
        ("1.2.3+other", true),
        ("1.9.9", true),
        ("2.0.0-alpha.2", true),
        ("2.0.0", false),
        ("2.0.0+other", false),
    ] {
        assert_eq!(range.contains(&version(v)), matches, "{v}");
    }
    let no_pre = AffectedRange {
        include_prerelease: false,
        ..range
    };
    assert!(!no_pre.contains(&version("1.9.0-alpha")));
    assert!(no_pre.contains(&version("1.9.0")));
    let pre_start = AffectedRange {
        introduced: version("1.0.0-alpha.2"),
        fixed: None,
        include_prerelease: true,
    };
    assert!(!pre_start.contains(&version("1.0.0-alpha.1")));
    assert!(pre_start.contains(&version("1.0.0-alpha.10")));
    assert!(pre_start.contains(&version("500.0.0+build")));
}

#[test]
fn malformed_or_ambiguous_catalogs_never_produce_clean_results() {
    let limits = Limits::default();
    let mut c = catalog();
    c.advisories.push(advisory("A", id("@org/root")));
    let bytes = serde_json::to_vec(&c).unwrap();
    assert!(parse_catalog(&bytes, &limits).is_ok());
    for mutation in [
        json!({"schema_version":2,"catalog_id":"x","advisories":[],"native_licenses":[]}),
        json!({"schema_version":1,"catalog_id":"x","advisories":[],"native_licenses":[],"osv":[]}),
        json!({"schema_version":1,"catalog_id":"x","advisories":[{"id":"A","package":{"ecosystem":"npm","name":"x"},"affected":[],"severity":"high","summary":"x"}],"native_licenses":[]}),
    ] {
        assert!(parse_catalog(&serde_json::to_vec(&mutation).unwrap(), &limits).is_err());
    }
    let mut duplicated = c.clone();
    duplicated.advisories.push(c.advisories[0].clone());
    assert!(audit(&solution(), &duplicated, &policy(), &limits).is_err());
    let mut empty = c.clone();
    empty.advisories[0].affected[0].fixed = Some(version("1.0.0+same-precedence"));
    assert!(parse_catalog(&serde_json::to_vec(&empty).unwrap(), &limits).is_err());
    let mut overlap = c.clone();
    overlap.advisories[0].affected.push(AffectedRange {
        introduced: version("1.5.0"),
        fixed: None,
        include_prerelease: false,
    });
    assert!(audit(&solution(), &overlap, &policy(), &limits).is_err());
    let mut missing_pre = serde_json::to_value(&c).unwrap();
    missing_pre["advisories"][0]["affected"][0]
        .as_object_mut()
        .unwrap()
        .remove("include_prerelease");
    assert!(parse_catalog(&serde_json::to_vec(&missing_pre).unwrap(), &limits).is_err());
    let mut unsupported = serde_json::to_value(&c).unwrap();
    unsupported["advisories"][0]["affected"][0]["introduced"] = json!(">=1.0.0");
    assert!(parse_catalog(&serde_json::to_vec(&unsupported).unwrap(), &limits).is_err());
    let native_license = NativeLicense {
        name: "libdemo".into(),
        version: version("1.0.0"),
        expression: "MIT".into(),
    };
    c.native_licenses = vec![native_license.clone(), native_license];
    assert!(audit(&solution(), &c, &policy(), &limits).is_err());
}

#[test]
fn spdx_boolean_precedence_and_explicit_exception_grants() {
    let mut s = solution();
    let p = policy();
    for (license, outcome) in [
        ("MIT", Outcome::Clean),
        ("Apache-2.0 OR GPL-3.0-only", Outcome::Clean),
        ("GPL-3.0-only OR MIT AND Apache-2.0", Outcome::Clean),
        ("MIT AND GPL-3.0-only OR BSD-3-Clause", Outcome::Findings),
        ("(MIT OR BSD-3-Clause) AND Apache-2.0", Outcome::Clean),
        ("MIT AND (GPL-3.0-only OR BSD-3-Clause)", Outcome::Findings),
        (
            "GPL-2.0-only WITH Classpath-exception-2.0",
            Outcome::Findings,
        ),
    ] {
        s.packages[0].manifest.package.license = Some(license.into());
        assert_eq!(run(&s, &catalog(), &p).outcome, outcome, "{license}");
    }
    s.packages[0].manifest.package.license =
        Some("GPL-2.0-only WITH Classpath-exception-2.0".into());
    let mut with = p.clone();
    with.allowed_licenses
        .push("GPL-2.0-only WITH Classpath-exception-2.0".into());
    assert_eq!(run(&s, &catalog(), &with).outcome, Outcome::Clean);
    s.packages[0].manifest.package.license = Some("GPL-2.0-only".into());
    assert_eq!(run(&s, &catalog(), &with).outcome, Outcome::Findings);
}

#[test]
fn unknown_missing_and_malformed_license_declarations_remain_unknown() {
    let mut s = solution();
    for license in [
        None,
        Some("proprietary terms"),
        Some("LicenseRef-Custom"),
        Some("MIT OR Unknown-SPDX"),
        Some("MIT AND"),
        Some("(MIT"),
        Some("(MIT) WITH LLVM-exception"),
        Some("GPL-2.0+"),
        Some("mit"),
        Some("MIT WITH NewException"),
        Some("MIT/Apache-2.0"),
    ] {
        s.packages[0].manifest.package.license = license.map(str::to_string);
        let report = run(&s, &catalog(), &policy());
        assert_eq!(report.outcome, Outcome::Unknown, "{license:?}");
        assert!(!report.has_policy_violations);
        assert!(matches!(
            report.findings[0].issue,
            FindingKind::LicenseUnknown { .. }
        ));
    }
}

#[test]
fn invalid_policy_and_invalid_solutions_are_errors() {
    let limits = Limits::default();
    for grants in [
        vec!["Unknown-ID"],
        vec!["MIT OR Apache-2.0"],
        vec!["MIT", "MIT"],
        vec!["MIT WITH UnknownException"],
    ] {
        let mut p = policy();
        p.allowed_licenses = grants.into_iter().map(str::to_string).collect();
        assert!(parse_policy(&serde_json::to_vec(&p).unwrap(), &limits).is_err());
        assert!(audit(&solution(), &catalog(), &p, &limits).is_err());
    }
    let mut s = solution();
    dependency(&mut s.packages[0], "@org/missing", false);
    assert!(matches!(
        audit(&s, &catalog(), &policy(), &limits),
        Err(Error::InvalidSolution(_))
    ));
    let mut s = solution();
    s.packages.push(s.packages[0].clone());
    assert!(matches!(
        audit(&s, &catalog(), &policy(), &limits),
        Err(Error::InvalidSolution(_))
    ));
}

#[test]
fn bounded_json_rejects_duplicate_fields_nested_keys_and_trailing_data() {
    let limits = Limits::default();
    assert!(parse_catalog(br#"{"schema_version":1,"schema_version":1,"catalog_id":"x","advisories":[],"native_licenses":[]}"#, &limits).is_err());
    let s = serde_json::to_string(&solution()).unwrap();
    let duplicated_map = s.replace(
        "\"dependencies\":{}",
        "\"dependencies\":{\"@org/a\":{},\"@org/a\":{}}",
    );
    let error = parse_solution(duplicated_map.as_bytes(), &limits)
        .unwrap_err()
        .to_string();
    assert!(error.contains("duplicate JSON key"), "{error}");
    assert!(parse_solution(format!("{s} null").as_bytes(), &limits).is_err());
    let tiny = Limits {
        max_input_bytes: 10,
        ..limits.clone()
    };
    assert!(parse_solution(s.as_bytes(), &tiny).is_err());
    let depth = Limits {
        max_json_depth: 1,
        ..limits.clone()
    };
    assert!(parse_solution(s.as_bytes(), &depth).is_err());
    let nodes = Limits {
        max_json_nodes: 1,
        ..limits.clone()
    };
    assert!(parse_solution(s.as_bytes(), &nodes).is_err());
    let string = Limits {
        max_string_bytes: 2,
        ..limits
    };
    assert!(parse_solution(s.as_bytes(), &string).is_err());
}

#[test]
fn graph_catalog_path_and_work_budgets_fail_closed() {
    let limits = Limits::default();
    let s = solution();
    for small in [
        Limits {
            max_work: 0,
            ..limits.clone()
        },
        Limits {
            max_packages: 0,
            ..limits.clone()
        },
        Limits {
            max_roots: 0,
            ..limits.clone()
        },
        Limits {
            max_path_depth: 0,
            ..limits.clone()
        },
        Limits {
            max_path_nodes: 0,
            ..limits.clone()
        },
        Limits {
            max_input_bytes: 10,
            ..limits.clone()
        },
    ] {
        assert!(audit(&s, &catalog(), &policy(), &small).is_err());
    }
    let mut s = solution();
    dependency(&mut s.packages[0], "@org/leaf", false);
    s.packages.push(package("@org/leaf"));
    assert!(
        audit(
            &s,
            &catalog(),
            &policy(),
            &Limits {
                max_edges: 0,
                ..limits.clone()
            }
        )
        .is_err()
    );
    let mut c = catalog();
    c.advisories.push(advisory("A", id("@org/root")));
    assert!(
        audit(
            &s,
            &c,
            &policy(),
            &Limits {
                max_entries: 0,
                ..limits.clone()
            }
        )
        .is_err()
    );
    s.packages[0].manifest.package.license = Some("((((MIT))))".into());
    assert!(matches!(
        audit(
            &s,
            &catalog(),
            &policy(),
            &Limits {
                max_license_depth: 1,
                ..limits.clone()
            }
        ),
        Err(Error::Limit(_))
    ));
    assert!(
        audit(
            &s,
            &catalog(),
            &policy(),
            &Limits {
                max_license_depth: 65,
                ..limits
            }
        )
        .is_err()
    );
}

#[test]
fn evaluated_context_binds_full_inputs_and_limits_not_catalog_label() {
    let s = solution();
    let c = catalog();
    let p = policy();
    let report = run(&s, &c, &p);
    assert_eq!(report.outcome, Outcome::Clean);
    assert_eq!(report.context.contract, "vesper.loom.offline-audit/1");
    assert_eq!(report, run(&s, &c, &p));
    let mut c2 = c.clone();
    c2.advisories.push(advisory("A", id("@org/root")));
    assert_eq!(c2.catalog_id, c.catalog_id);
    assert_ne!(
        report.context.catalog_digest,
        run(&s, &c2, &p).context.catalog_digest
    );
    let mut p2 = p.clone();
    p2.minimum_severity = Severity::Critical;
    assert_ne!(
        report.context.policy_digest,
        run(&s, &c, &p2).context.policy_digest
    );
    let mut s2 = s.clone();
    s2.packages[0].manifest.package.license = Some("Apache-2.0".into());
    assert_ne!(
        report.context.solution_digest,
        run(&s2, &c, &p).context.solution_digest
    );
    let limits = Limits {
        max_work: 9_999_999,
        ..Limits::default()
    };
    assert_ne!(
        report.context.limits_digest,
        audit(&s, &c, &p, &limits).unwrap().context.limits_digest
    );
    assert_eq!(
        serde_json::from_slice::<AuditReport>(&serde_json::to_vec(&report).unwrap()).unwrap(),
        report
    );
}

#[test]
fn severity_threshold_and_mixed_unknown_violations_are_explicit() {
    let mut c = catalog();
    c.advisories.push(advisory("A", id("@org/root")));
    let mut p = policy();
    p.minimum_severity = Severity::Critical;
    assert_eq!(run(&solution(), &c, &p).outcome, Outcome::Clean);
    p.minimum_severity = Severity::High;
    let mut s = solution();
    s.packages[0].manifest.package.license = None;
    let report = run(&s, &c, &p);
    assert_eq!(report.outcome, Outcome::Unknown);
    assert!(report.has_policy_violations);
    assert_eq!(report.findings.len(), 2);
}

#[test]
fn prerelease_decisions_and_required_fixed_bound_survive_full_audit_path() {
    let mut s = solution();
    s.packages[0].manifest.package.version = version("1.0.0-alpha.10+selected");
    s.roots[0].version = VersionReq::parse("=1.0.0-alpha.10").unwrap();
    let mut c = catalog();
    let mut a = advisory("A", id("@org/root"));
    a.affected[0].introduced = version("1.0.0-alpha.2");
    a.affected[0].fixed = Some(version("1.0.0"));
    c.advisories.push(a);
    assert_eq!(run(&s, &c, &policy()).outcome, Outcome::Findings);
    c.advisories[0].affected[0].include_prerelease = false;
    assert_eq!(run(&s, &c, &policy()).outcome, Outcome::Clean);
    let mut missing_fixed = serde_json::to_value(&c).unwrap();
    missing_fixed["advisories"][0]["affected"][0]
        .as_object_mut()
        .unwrap()
        .remove("fixed");
    assert!(
        parse_catalog(
            &serde_json::to_vec(&missing_fixed).unwrap(),
            &Limits::default()
        )
        .is_err()
    );
}

#[test]
fn retained_report_paths_and_license_token_work_are_bounded() {
    let s = solution();
    let mut c = catalog();
    c.advisories = vec![
        advisory("A", id("@org/root")),
        advisory("B", id("@org/root")),
    ];
    let limits = Limits {
        max_path_nodes: 1,
        ..Limits::default()
    };
    assert!(matches!(
        audit(&s, &c, &policy(), &limits),
        Err(Error::Limit(_))
    ));
    let mut s = s;
    s.packages[0].manifest.package.license = Some("MIT AND MIT".into());
    let limits = Limits {
        max_license_tokens: 1,
        ..Limits::default()
    };
    assert!(matches!(
        audit(&s, &catalog(), &policy(), &limits),
        Err(Error::Limit(_))
    ));
}

#[test]
fn empty_root_scope_cannot_certify_an_empty_or_unreachable_graph_clean() {
    let limits = Limits::default();
    let mut no_roots = solution();
    no_roots.roots.clear();
    // A selected vulnerable package is not reachable without a root. It must
    // not disappear into an empty graph and produce a misleading clean audit.
    let mut c = catalog();
    c.advisories.push(advisory("A", id("@org/root")));
    assert!(matches!(
        audit(&no_roots, &c, &policy(), &limits),
        Err(Error::InvalidInput(reason)) if reason.contains("at least one declared root")
    ));
    no_roots.packages.clear();
    let parsed = parse_solution(&serde_json::to_vec(&no_roots).unwrap(), &limits).unwrap();
    assert!(matches!(
        audit(&parsed, &catalog(), &policy(), &limits),
        Err(Error::InvalidInput(reason)) if reason.contains("at least one declared root")
    ));
}
