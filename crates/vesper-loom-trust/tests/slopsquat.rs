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
    let report =
        screen_package_admission(&pkg("@vesper-core/bytes"), &catalog, &policy, 10_000_000);
    assert_eq!(report.decision, QuarantineDecision::Admit);
    assert!(report.findings.is_empty());
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
