use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use vesper_loom_calculus::PackageName;
use vesper_loom_cas::{DerivationInputs, Digest, Domain, ObjectId, ObjectKind, blob_id};
use vesper_loom_trust::*;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn identity() -> BuildIdentity {
    let source = ObjectId::new(Domain::Source, ObjectKind::Tree, Digest([4; 32]));
    let derivation = DerivationInputs::new(Domain::Artifact, "test-builder", "implementation-1")
        .with_target("x86_64-unknown-linux-gnu")
        .with_input("source", source)
        .key()
        .unwrap();
    BuildIdentity {
        package: PackageName::parse("@test/package").unwrap(),
        version: "1.2.3+source".into(),
        source_tree: source.to_string(),
        derivation: derivation.to_string(),
        target: "x86_64-unknown-linux-gnu".into(),
        builder_blake3: "01".repeat(32),
        sandbox_contract: "vesper:sandbox/step@0.1.0".into(),
        output: blob_id(Domain::Artifact, b"actual artifact").to_string(),
    }
}
fn key(seed: u8, id: &str, operator: &str) -> (SigningKey, TrustedBuilder) {
    let signing = SigningKey::from_bytes(&[seed; 32]);
    let trusted = TrustedBuilder {
        key_id: id.into(),
        operator_id: operator.into(),
        public_key: hex(&signing.verifying_key().to_bytes()),
        valid_from: 10,
        valid_until: 1000,
        revoked: false,
    };
    (signing, trusted)
}
fn envelope(signing: &SigningKey, id: &str, identity: &BuildIdentity) -> Attestation {
    let statement = Statement {
        schema_version: 1,
        key_id: id.into(),
        identity: identity.clone(),
        issued_at: 100,
        expires_at: 300,
    };
    let signature = hex(&signing
        .sign(&canonical_signing_bytes(&statement).unwrap())
        .to_bytes());
    Attestation {
        statement,
        signature,
    }
}
fn fixture() -> (
    BuildIdentity,
    BuilderPolicy,
    AttestationBundle,
    SigningKey,
    SigningKey,
) {
    let expected = identity();
    let (a, ka) = key(1, "key-a", "operator-a");
    let (b, kb) = key(2, "key-b", "operator-b");
    let policy = BuilderPolicy {
        schema_version: 1,
        minimum_operators: 2,
        max_statement_ttl: 500,
        builders: vec![ka, kb],
    };
    let bundle = AttestationBundle {
        schema_version: 1,
        attestations: vec![
            envelope(&a, "key-a", &expected),
            envelope(&b, "key-b", &expected),
        ],
    };
    (expected, policy, bundle, a, b)
}
fn denied(
    expected: &BuildIdentity,
    bundle: &AttestationBundle,
    policy: &BuilderPolicy,
    now: u64,
    code: &str,
) {
    let result = verify_attestations(expected, bundle, policy, now);
    assert!(!result.accepted, "{result:?}");
    assert!(
        result.diagnostics.iter().any(|d| d.code == code),
        "{result:?}"
    );
}
#[test]
fn two_real_signatures_round_trip_and_reordering_is_deterministic() {
    let (expected, policy, bundle, _, _) = fixture();
    let bytes = serde_json::to_vec_pretty(&bundle).unwrap();
    let parsed = parse_bundle(&bytes).unwrap();
    assert_eq!(parsed, bundle);
    let before = verify_attestations(&expected, &parsed, &policy, 200);
    assert!(before.accepted, "{before:?}");
    assert_eq!(before.verified_operators.len(), 2);
    let mut reordered = bundle.clone();
    reordered.attestations.reverse();
    let mut policy = policy;
    policy.builders.reverse();
    assert_eq!(
        verify_attestations(&expected, &reordered, &policy, 200),
        before
    );
    assert_eq!(
        serde_json::to_vec(&before).unwrap(),
        serde_json::to_vec(&verify_attestations(&expected, &reordered, &policy, 200)).unwrap()
    );
}
#[test]
fn every_identity_field_is_bound_even_when_the_changed_statement_is_resigned() {
    let (expected, policy, bundle, a, _) = fixture();
    for field in [
        "package",
        "version",
        "source",
        "derivation",
        "target",
        "builder",
        "contract",
        "output",
    ] {
        let mut changed = expected.clone();
        match field {
            "package" => changed.package = PackageName::parse("@other/package").unwrap(),
            "version" => changed.version = "1.2.3+different-source".into(),
            "source" => {
                changed.source_tree =
                    ObjectId::new(Domain::Source, ObjectKind::Tree, Digest([9; 32])).to_string()
            }
            "derivation" => changed.derivation = format!("drv:b3v1:{}", "09".repeat(32)),
            "target" => changed.target = "aarch64-unknown-linux-gnu".into(),
            "builder" => changed.builder_blake3 = "09".repeat(32),
            "contract" => changed.sandbox_contract = "vesper.sandbox.v2".into(),
            "output" => changed.output = blob_id(Domain::Artifact, b"other").to_string(),
            _ => unreachable!(),
        }
        let mut modified = bundle.clone();
        modified.attestations[0] = envelope(&a, "key-a", &changed);
        denied(&expected, &modified, &policy, 200, "identity_mismatch");
    }
}
#[test]
fn payload_tampering_signatures_wrong_keys_and_cross_protocol_messages_fail() {
    let (expected, policy, bundle, a, b) = fixture();
    let mut modified = bundle.clone();
    modified.attestations[0].statement.expires_at += 1;
    denied(&expected, &modified, &policy, 200, "invalid_signature");
    let mut noncanonical = bundle.clone();
    noncanonical.attestations[0]
        .signature
        .replace_range(64.., &"ff".repeat(32));
    denied(&expected, &noncanonical, &policy, 200, "invalid_signature");
    let mut modified = bundle.clone();
    modified.attestations[0].signature = "00".repeat(64);
    denied(&expected, &modified, &policy, 200, "invalid_signature");
    let mut modified = bundle.clone();
    modified.attestations[0].signature = bundle.attestations[0].signature.to_uppercase();
    denied(&expected, &modified, &policy, 200, "invalid_signature");
    let mut modified = bundle.clone();
    modified.attestations[0].signature = hex(&b
        .sign(&canonical_signing_bytes(&modified.attestations[0].statement).unwrap())
        .to_bytes());
    denied(&expected, &modified, &policy, 200, "invalid_signature");
    let mut modified = bundle.clone();
    modified.attestations[0].signature = hex(&a
        .sign(&serde_json::to_vec(&modified.attestations[0].statement).unwrap())
        .to_bytes());
    denied(&expected, &modified, &policy, 200, "invalid_signature");
}
#[test]
fn key_rotation_does_not_create_independent_operators() {
    let (expected, mut policy, mut bundle, _, _) = fixture();
    policy.builders[1].operator_id = policy.builders[0].operator_id.clone();
    denied(&expected, &bundle, &policy, 200, "insufficient_quorum");
    let (third, trusted) = key(3, "key-c", "operator-c");
    policy.builders.push(trusted);
    bundle
        .attestations
        .push(envelope(&third, "key-c", &expected));
    let report = verify_attestations(&expected, &bundle, &policy, 200);
    assert!(report.accepted, "{report:?}");
    assert_eq!(report.verified_keys.len(), 3);
    assert_eq!(report.verified_operators.len(), 2);
}
#[test]
fn duplicate_attestations_unknown_keys_and_invalid_extras_reject_the_whole_bundle() {
    let (expected, policy, bundle, _, _) = fixture();
    let mut modified = bundle.clone();
    modified.attestations.push(bundle.attestations[0].clone());
    denied(&expected, &modified, &policy, 200, "duplicate_attestation");
    let (other, _) = key(3, "unknown", "unknown");
    let mut modified = bundle.clone();
    modified
        .attestations
        .push(envelope(&other, "unknown", &expected));
    denied(&expected, &modified, &policy, 200, "unknown_key");
    let mut modified = bundle.clone();
    let mut invalid = envelope(&other, "key-c", &expected);
    invalid.signature = "bad".into();
    modified.attestations.push(invalid);
    let mut policy = policy;
    policy.builders.push(key(3, "key-c", "operator-c").1);
    denied(&expected, &modified, &policy, 200, "invalid_signature");
}
#[test]
fn expiry_future_revocation_key_intervals_ttl_and_time_boundaries() {
    let (expected, policy, bundle, a, _) = fixture();
    denied(
        &expected,
        &bundle,
        &policy,
        99,
        "statement_expired_or_future",
    );
    denied(
        &expected,
        &bundle,
        &policy,
        300,
        "statement_expired_or_future",
    );
    assert!(verify_attestations(&expected, &bundle, &policy, 100).accepted);
    let mut policy = policy;
    policy.builders[0].revoked = true;
    denied(&expected, &bundle, &policy, 200, "revoked_key");
    policy.builders[0].revoked = false;
    policy.builders[0].valid_until = 299;
    denied(
        &expected,
        &bundle,
        &policy,
        200,
        "key_expired_or_not_yet_valid",
    );
    policy.builders[0].valid_until = 1000;
    policy.max_statement_ttl = 199;
    denied(&expected, &bundle, &policy, 200, "statement_ttl_exceeded");
    policy.max_statement_ttl = 500;
    let mut modified = bundle.clone();
    modified.attestations[0].statement.issued_at = 0;
    modified.attestations[0].signature = hex(&a
        .sign(&canonical_signing_bytes(&modified.attestations[0].statement).unwrap())
        .to_bytes());
    denied(
        &expected,
        &modified,
        &policy,
        200,
        "key_expired_or_not_yet_valid",
    );
    denied(
        &expected,
        &bundle,
        &policy,
        u64::MAX,
        "statement_expired_or_future",
    );
}
#[test]
fn weak_malformed_duplicate_and_inconsistent_trusted_keys_are_rejected() {
    let (expected, policy, bundle, _, _) = fixture();
    let mut modified = policy.clone();
    modified.builders[1].key_id = modified.builders[0].key_id.clone();
    denied(&expected, &bundle, &modified, 200, "duplicate_key_id");
    let mut modified = policy.clone();
    modified.builders[1].public_key = modified.builders[0].public_key.clone();
    denied(&expected, &bundle, &modified, 200, "duplicate_public_key");
    let mut modified = policy.clone();
    modified.builders[0].public_key = "01".to_string() + &"00".repeat(31);
    denied(&expected, &bundle, &modified, 200, "invalid_public_key");
    let mut modified = policy.clone();
    modified.builders[0].public_key = "not a key".into();
    denied(&expected, &bundle, &modified, 200, "invalid_public_key");
    let mut modified = policy.clone();
    modified.builders[0].valid_until = modified.builders[0].valid_from;
    denied(&expected, &bundle, &modified, 200, "invalid_policy");
    let mut modified = policy.clone();
    modified.minimum_operators = 0;
    denied(&expected, &bundle, &modified, 200, "invalid_policy");
    let mut modified = policy;
    modified.max_statement_ttl = 0;
    denied(&expected, &bundle, &modified, 200, "invalid_policy");
}
#[test]
fn typed_object_ids_and_schema_versions_fail_closed() {
    let (expected, policy, bundle, _, _) = fixture();
    for field in [
        "source_blob",
        "source_interface",
        "output_source",
        "mif_derivation",
    ] {
        let mut modified = expected.clone();
        match field {
            "source_blob" => modified.source_tree = blob_id(Domain::Source, b"bytes").to_string(),
            "source_interface" => {
                modified.source_tree =
                    ObjectId::new(Domain::Interface, ObjectKind::Tree, Digest([4; 32])).to_string()
            }
            "output_source" => modified.output = blob_id(Domain::Source, b"bytes").to_string(),
            "mif_derivation" => {
                modified.derivation = blob_id(Domain::Interface, b"mif").to_string()
            }
            _ => unreachable!(),
        }
        denied(&modified, &bundle, &policy, 200, "invalid_expected");
    }
    let mut modified = bundle.clone();
    modified.schema_version = 999;
    denied(&expected, &modified, &policy, 200, "unsupported_schema");
    let mut modified = policy.clone();
    modified.schema_version = 999;
    denied(&expected, &bundle, &modified, 200, "unsupported_schema");
    let mut modified = bundle;
    modified.attestations[0].statement.schema_version = 999;
    denied(&expected, &modified, &policy, 200, "invalid_statement");
}
#[test]
fn malformed_json_duplicate_fields_unknown_fields_and_limits_are_rejected() {
    let (expected, policy, bundle, _, _) = fixture();
    assert!(parse_bundle(br#"{"schema_version":1,"schema_version":1,"attestations":[]}"#).is_err());
    let mut value = serde_json::to_value(&bundle).unwrap();
    value["extra"] = json!(true);
    assert!(parse_bundle(&serde_json::to_vec(&value).unwrap()).is_err());
    assert!(parse_policy(&vec![b' '; MAX_JSON_BYTES + 1]).is_err());
    let mut modified = policy.clone();
    modified.builders = vec![policy.builders[0].clone(); MAX_POLICY_KEYS + 1];
    denied(&expected, &bundle, &modified, 200, "limit_exceeded");
    let mut modified = bundle.clone();
    modified.attestations = vec![bundle.attestations[0].clone(); MAX_ATTESTATIONS + 1];
    denied(&expected, &modified, &policy, 200, "limit_exceeded");
    let mut statement = bundle.attestations[0].statement.clone();
    statement.identity.target = "a".repeat(257);
    assert!(canonical_signing_bytes(&statement).is_err());
    statement = bundle.attestations[0].statement.clone();
    statement.expires_at = statement.issued_at;
    assert!(canonical_signing_bytes(&statement).is_err());
}
#[test]
fn empty_or_single_builder_bundle_never_reaches_quorum() {
    let (expected, policy, mut bundle, _, _) = fixture();
    bundle.attestations.clear();
    denied(&expected, &bundle, &policy, 200, "insufficient_quorum");
    let (_, _, complete, _, _) = fixture();
    bundle.attestations.push(complete.attestations[0].clone());
    denied(&expected, &bundle, &policy, 200, "insufficient_quorum");
}

#[test]
fn committed_real_signed_fixture_is_reproducible_and_matches_artifact_bytes() {
    const ARTIFACT: &[u8] = b"vesper attestation fixture artifact\n";
    const SOURCE: &[u8] = b"fn answer() -> i64 { 42 }\n";
    let mut tree = vesper_loom_cas::Tree::new(Domain::Source);
    tree.insert_file(
        "main.ves",
        blob_id(Domain::Source, SOURCE),
        SOURCE.len() as u64,
        false,
    )
    .unwrap();
    let builder = blake3::hash(b"fixture compiler implementation v1")
        .to_hex()
        .to_string();
    let contract = "vesper:sandbox/step@0.1.0";
    let source = tree.id();
    let derivation = DerivationInputs::new(Domain::Artifact, "test-builder", "implementation-1")
        .with_target("x86_64-unknown-linux-gnu")
        .with_input("source", source)
        .with_option("builder_blake3", &builder)
        .with_option("sandbox_contract", contract)
        .key()
        .unwrap();
    let expected = BuildIdentity {
        package: PackageName::parse("@test/package").unwrap(),
        version: "1.2.3+source".into(),
        source_tree: source.to_string(),
        derivation: derivation.to_string(),
        target: "x86_64-unknown-linux-gnu".into(),
        builder_blake3: builder,
        sandbox_contract: contract.into(),
        output: blob_id(Domain::Artifact, ARTIFACT).to_string(),
    };
    let (_, policy, _, a, b) = fixture();
    let bundle = AttestationBundle {
        schema_version: 1,
        attestations: vec![
            envelope(&a, "key-a", &expected),
            envelope(&b, "key-b", &expected),
        ],
    };
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let records = [
        (
            "identity.json",
            serde_json::to_vec_pretty(&expected).unwrap(),
        ),
        ("policy.json", serde_json::to_vec_pretty(&policy).unwrap()),
        ("bundle.json", serde_json::to_vec_pretty(&bundle).unwrap()),
        ("artifact.bin", ARTIFACT.to_vec()),
    ];
    if std::env::var_os("VESPER_BLESS_ATTESTATION_FIXTURE").is_some() {
        std::fs::create_dir_all(&path).unwrap();
        for (name, bytes) in &records {
            std::fs::write(path.join(name), bytes).unwrap();
        }
    }
    for (name, bytes) in records {
        assert_eq!(std::fs::read(path.join(name)).unwrap(), bytes);
    }
    assert!(verify_attestations(&expected, &bundle, &policy, 150).accepted);
}

#[test]
fn validity_near_u64_max_does_not_wrap_or_turn_expiry_into_success() {
    let (expected, mut policy, mut bundle, a, b) = fixture();
    for builder in &mut policy.builders {
        builder.valid_until = u64::MAX;
    }
    for (attestation, key) in bundle.attestations.iter_mut().zip([a, b]) {
        attestation.statement.issued_at = u64::MAX - 2;
        attestation.statement.expires_at = u64::MAX;
        attestation.signature = hex(&key
            .sign(&canonical_signing_bytes(&attestation.statement).unwrap())
            .to_bytes());
    }
    assert!(verify_attestations(&expected, &bundle, &policy, u64::MAX - 1).accepted);
    denied(
        &expected,
        &bundle,
        &policy,
        u64::MAX,
        "statement_expired_or_future",
    );
}
