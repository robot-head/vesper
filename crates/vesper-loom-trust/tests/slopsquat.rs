use vesper_loom_calculus::PackageName;
use vesper_loom_trust::slopsquat::{
    CatalogEntry, QuarantineDecision, QuarantinePolicy, QuarantineReason, canonical_conflation_key,
    damerau_levenshtein, screen_package_admission,
};

fn pkg(s: &str) -> PackageName {
    PackageName::parse(s).unwrap()
}

fn sample_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry {
            package: pkg("@vesper-core/bytes"),
            org_created_at: 1_000,
            trust_cost: 5,
            attested_oidc: true,
        },
        CatalogEntry {
            package: pkg("@vesper-io/uring-reactor"),
            org_created_at: 1_000,
            trust_cost: 12,
            attested_oidc: true,
        },
        CatalogEntry {
            package: pkg("@vesper-compression/lz4-pure"),
            org_created_at: 2_000,
            trust_cost: 8,
            attested_oidc: true,
        },
        CatalogEntry {
            package: pkg("@vesper-core/bytes-fast"),
            org_created_at: 10_000_000,
            trust_cost: 15,
            attested_oidc: true,
        },
        CatalogEntry {
            package: pkg("@vesper_core/bytes"),
            org_created_at: 10_000_000,
            trust_cost: 10,
            attested_oidc: true,
        },
        CatalogEntry {
            package: pkg("@vesper-core/byets"),
            org_created_at: 10_000_000,
            trust_cost: 10,
            attested_oidc: true,
        },
        CatalogEntry {
            package: pkg("@vesper-std/bytes"),
            org_created_at: 1_000,
            trust_cost: 10,
            attested_oidc: true,
        },
        CatalogEntry {
            package: pkg("@acme/risky"),
            org_created_at: 1_000,
            trust_cost: 90,
            attested_oidc: false,
        },
    ]
}

#[test]
fn admits_vetted_mature_package() {
    let catalog = sample_catalog();
    let policy = QuarantinePolicy::default();
    let report = screen_package_admission(
        &pkg("@vesper-io/uring-reactor"),
        &catalog,
        &policy,
        10_000_000,
    );
    assert_eq!(report.decision, QuarantineDecision::Admit);
    assert!(report.findings.is_empty());

    // When the catalog has no lookalike siblings, @vesper-core/bytes is admitted.
    let clean_catalog = vec![catalog[0].clone(), catalog[1].clone(), catalog[2].clone()];
    let clean_report = screen_package_admission(
        &pkg("@vesper-core/bytes"),
        &clean_catalog,
        &policy,
        10_000_000,
    );
    assert_eq!(clean_report.decision, QuarantineDecision::Admit);
    assert!(clean_report.findings.is_empty());
}

#[test]
fn detects_transposition_typosquat_and_hard_blocks_in_agent_mode() {
    assert_eq!(damerau_levenshtein("bytes", "byets"), 1);
    let catalog = sample_catalog();
    let mut policy = QuarantinePolicy::default();
    let interactive =
        screen_package_admission(&pkg("@vesper-core/byets"), &catalog, &policy, 20_000_000);
    assert_eq!(interactive.decision, QuarantineDecision::Quarantined);
    assert!(
        interactive
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::EditDistanceTyposquat
                && f.edit_distance == Some(1))
    );

    policy.agent_mode = true;
    let agent = screen_package_admission(&pkg("@vesper-core/byets"), &catalog, &policy, 20_000_000);
    assert_eq!(agent.decision, QuarantineDecision::Rejected);
}

#[test]
fn detects_delimiter_conflation_and_std_impersonation() {
    assert_eq!(
        canonical_conflation_key("vesper_c0re/byt3s"),
        canonical_conflation_key("vesper-core/bytes")
    );
    let catalog = sample_catalog();
    let policy = QuarantinePolicy::default();

    let conflation =
        screen_package_admission(&pkg("@vesper_core/bytes"), &catalog, &policy, 20_000_000);
    assert_eq!(conflation.decision, QuarantineDecision::Quarantined);
    assert!(
        conflation
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::IdentifierConflation)
    );

    let spoof = screen_package_admission(&pkg("@vesper-std/bytes"), &catalog, &policy, 20_000_000);
    assert_eq!(spoof.decision, QuarantineDecision::Quarantined);
    assert!(
        spoof
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::StdNamespaceImpersonation)
    );
}

#[test]
fn enforces_namespace_age_trust_cost_and_oidc_provenance() {
    let catalog = sample_catalog();
    let policy = QuarantinePolicy::default();

    let immature = screen_package_admission(
        &pkg("@vesper-core/bytes-fast"),
        &catalog,
        &policy,
        10_000_500,
    );
    assert_eq!(immature.decision, QuarantineDecision::Quarantined);
    assert!(
        immature
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::ImmatureNamespace)
    );

    let untrusted = screen_package_admission(&pkg("@acme/risky"), &catalog, &policy, 20_000_000);
    assert_eq!(untrusted.decision, QuarantineDecision::Rejected);
    assert!(
        untrusted
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::ExcessiveTrustCost)
    );
    assert!(
        untrusted
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::MissingOidcProvenance)
    );

    let missing =
        screen_package_admission(&pkg("@unknown/hallucinated"), &catalog, &policy, 20_000_000);
    assert_eq!(missing.decision, QuarantineDecision::Rejected);
    assert!(
        missing
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::MissingFromCatalog)
    );
}

#[test]
fn does_not_exempt_similar_packages_by_namespace_creation_order() {
    // Attacker holds an older namespace (@acm3 / @acm4) than @acme, with mature age and OIDC.
    let catalog = vec![
        CatalogEntry {
            package: pkg("@acme/bytes"),
            org_created_at: 10_000_000,
            trust_cost: 5,
            attested_oidc: true,
        },
        CatalogEntry {
            package: pkg("@acm3/bytes"),
            org_created_at: 1_000,
            trust_cost: 5,
            attested_oidc: true,
        },
        CatalogEntry {
            package: pkg("@acm4/bytes"),
            org_created_at: 1_000,
            trust_cost: 5,
            attested_oidc: true,
        },
    ];
    let mut policy = QuarantinePolicy::default();

    let conflated = screen_package_admission(&pkg("@acm3/bytes"), &catalog, &policy, 20_000_000);
    assert_eq!(conflated.decision, QuarantineDecision::Quarantined);
    assert!(
        conflated
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::IdentifierConflation)
    );

    let typosquat = screen_package_admission(&pkg("@acm4/bytes"), &catalog, &policy, 20_000_000);
    assert_eq!(typosquat.decision, QuarantineDecision::Quarantined);
    assert!(
        typosquat
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::EditDistanceTyposquat
                && f.edit_distance == Some(1))
    );

    policy.agent_mode = true;
    let agent_conflated =
        screen_package_admission(&pkg("@acm3/bytes"), &catalog, &policy, 20_000_000);
    assert_eq!(agent_conflated.decision, QuarantineDecision::Rejected);
}

#[test]
fn bounds_oversized_names_without_quadratic_allocation() {
    let long_a = pkg(&format!("@org/{}", "a".repeat(10_000)));
    let long_b = pkg(&format!("@org/{}b", "a".repeat(9_999)));
    let catalog = vec![CatalogEntry {
        package: long_b.clone(),
        org_created_at: 1_000,
        trust_cost: 5,
        attested_oidc: true,
    }];
    let policy = QuarantinePolicy::default();

    // MissingFromCatalog oversized candidate is immediately rejected without quadratic DP allocation.
    let missing_long = screen_package_admission(&long_a, &catalog, &policy, 20_000_000);
    assert_eq!(missing_long.decision, QuarantineDecision::Rejected);
    assert!(
        missing_long
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::OversizedPackageName)
    );
    assert!(
        missing_long
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::MissingFromCatalog)
    );

    // Cataloged oversized entry is also rejected.
    let present_long = screen_package_admission(&long_b, &catalog, &policy, 20_000_000);
    assert_eq!(present_long.decision, QuarantineDecision::Rejected);
    assert!(
        present_long
            .findings
            .iter()
            .any(|f| f.reason == QuarantineReason::OversizedPackageName)
    );

    // Rolling-row damerau_levenshtein preserves exact OSA distances.
    assert_eq!(damerau_levenshtein("", "abc"), 3);
    assert_eq!(damerau_levenshtein("abc", ""), 3);
    assert_eq!(damerau_levenshtein("ca", "abc"), 3);
    assert_eq!(damerau_levenshtein("ab", "ba"), 1);
    assert_eq!(
        damerau_levenshtein(&"x".repeat(1_000), &format!("{}y", "x".repeat(999))),
        1
    );
}
