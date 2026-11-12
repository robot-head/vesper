use ed25519_dalek::{Signer, SigningKey};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use vesper_loom_admission::*;
use vesper_loom_cas::{Domain, Store, Tree, blob_id};
use vesper_loom_trust::*;

const ARTIFACT: &[u8] = include_bytes!("../../vesper-loom-trust/tests/fixtures/artifact.bin");
#[derive(Clone)]
struct Fixture {
    expected: BuildIdentity,
    policy: BuilderPolicy,
    bundle: AttestationBundle,
}
struct JsonInputs {
    expected: Vec<u8>,
    policy: Vec<u8>,
    bundle: Vec<u8>,
}
impl JsonInputs {
    fn context(&self, evaluated_at: u64) -> Context<'_> {
        Context {
            expected_json: &self.expected,
            policy_json: &self.policy,
            bundle_json: &self.bundle,
            evaluated_at,
        }
    }
}
impl Fixture {
    fn new() -> Self {
        Self {
            expected: parse_identity(include_bytes!(
                "../../vesper-loom-trust/tests/fixtures/identity.json"
            ))
            .unwrap(),
            policy: parse_policy(include_bytes!(
                "../../vesper-loom-trust/tests/fixtures/policy.json"
            ))
            .unwrap(),
            bundle: parse_bundle(include_bytes!(
                "../../vesper-loom-trust/tests/fixtures/bundle.json"
            ))
            .unwrap(),
        }
    }
    fn json(&self) -> JsonInputs {
        JsonInputs {
            expected: serde_json::to_vec(&self.expected).unwrap(),
            policy: serde_json::to_vec(&self.policy).unwrap(),
            bundle: serde_json::to_vec(&self.bundle).unwrap(),
        }
    }
    fn sign_all(&mut self) {
        for (at, envelope) in self.bundle.attestations.iter_mut().enumerate() {
            envelope.statement.identity = self.expected.clone();
            let key = SigningKey::from_bytes(&[at as u8 + 1; 32]);
            envelope.signature = hex(&key
                .sign(&canonical_signing_bytes(&envelope.statement).unwrap())
                .to_bytes());
        }
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn import(dir: &TempDir, inputs: &JsonInputs) -> AdmissionReport {
    admit_blob(
        dir.path().join("store"),
        ARTIFACT.to_vec(),
        inputs.context(150),
        Limits::default(),
    )
    .unwrap()
}
fn write_object(path: &Path, bytes: &[u8]) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).unwrap();
    fs::write(path, bytes).unwrap();
}
fn snapshot(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    fn walk(root: &Path, relative: &Path, out: &mut Vec<(PathBuf, Option<Vec<u8>>)>) {
        let mut paths = fs::read_dir(root.join(relative))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        paths.sort();
        for name in paths {
            let path = relative.join(name);
            if fs::symlink_metadata(root.join(&path)).unwrap().is_dir() {
                out.push((path.clone(), None));
                walk(root, &path, out);
            } else {
                out.push((path.clone(), Some(fs::read(root.join(&path)).unwrap())));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, Path::new(""), &mut out);
    out
}
fn rejected_without_store(
    fixture: &Fixture,
    bytes: Vec<u8>,
    at: u64,
    predicate: impl FnOnce(&AdmissionError) -> bool,
) {
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("must-not-exist");
    let inputs = fixture.json();
    let pointer = bytes.as_ptr();
    let expected = bytes.clone();
    let failure =
        admit_blob(&store_path, bytes, inputs.context(at), Limits::default()).unwrap_err();
    assert!(predicate(&failure.error), "{:?}", failure.error);
    assert_eq!(failure.artifact, expected);
    assert_eq!(
        failure.artifact.as_ptr(),
        pointer,
        "the original allocation must return"
    );
    assert!(!store_path.exists());
    assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[test]
fn real_signed_bytes_import_read_and_no_clobber_materialization() {
    let fixture = Fixture::new();
    let inputs = fixture.json();
    let dir = tempfile::tempdir().unwrap();
    let report = import(&dir, &inputs);
    assert!(report.verification.accepted);
    assert_eq!(report.verification.verified_operators.len(), 2);
    assert_eq!(
        report.output,
        blob_id(Domain::Artifact, ARTIFACT).to_string()
    );
    assert_eq!(report.size, ARTIFACT.len() as u64);
    assert_eq!(report.operation, Operation::Import);
    assert_eq!(report.newly_stored, Some(true));
    let store = Store::open(dir.path().join("store")).unwrap();
    let summary = store.verify_all().unwrap();
    assert_eq!(summary.verified_objects, 1);
    assert_eq!(summary.verified_derivations, 0);
    let again = import(&dir, &inputs);
    assert_eq!(again.newly_stored, Some(false));
    assert_eq!(again.context, report.context);
    assert_ne!(again.report_blake3, report.report_blake3);
    let read = retrieve_blob(&store, inputs.context(150), Limits::default()).unwrap();
    assert_eq!(read.artifact, ARTIFACT);
    assert_eq!(read.report.context, report.context);
    assert_eq!(read.report.operation, Operation::Retrieve);
    let destination = dir.path().join("artifact-copy");
    let materialized =
        materialize_blob(&store, &destination, inputs.context(150), Limits::default()).unwrap();
    assert_eq!(materialized.path, destination);
    assert_eq!(materialized.report.operation, Operation::Materialize);
    assert_eq!(fs::read(&destination).unwrap(), ARTIFACT);
    fs::write(&destination, b"independent edit").unwrap();
    assert_eq!(
        retrieve_blob(&store, inputs.context(150), Limits::default())
            .unwrap()
            .artifact,
        ARTIFACT
    );
    let before = snapshot(dir.path());
    assert!(
        materialize_blob(&store, &destination, inputs.context(150), Limits::default()).is_err()
    );
    assert_eq!(
        snapshot(dir.path()),
        before,
        "no overwrite or staging leftovers"
    );
}

#[test]
fn every_expected_identity_field_is_checked_before_store_creation() {
    let original = Fixture::new();
    for field in 0..8 {
        let mut fixture = original.clone();
        match field {
            0 => {
                fixture.expected.package =
                    vesper_loom_calculus::PackageName::parse("@other/package").unwrap()
            }
            1 => fixture.expected.version = "9.8.7".into(),
            2 => fixture.expected.source_tree = Tree::new(Domain::Source).id().to_string(),
            3 => fixture.expected.derivation = format!("drv:b3v1:{}", "08".repeat(32)),
            4 => fixture.expected.target = "aarch64-unknown-linux-gnu".into(),
            5 => fixture.expected.builder_blake3 = "08".repeat(32),
            6 => fixture.expected.sandbox_contract = "other.contract/2".into(),
            7 => fixture.expected.output = blob_id(Domain::Artifact, b"other").to_string(),
            _ => unreachable!(),
        }
        rejected_without_store(&fixture, ARTIFACT.to_vec(), 150, |error| {
            matches!(error, AdmissionError::QuorumRejected(_))
        });
    }
}

#[test]
fn signature_blob_tamper_and_tree_outputs_reject_with_original_owner() {
    let mut fixture = Fixture::new();
    fixture.bundle.attestations[0].signature = "00".repeat(64);
    rejected_without_store(&fixture, ARTIFACT.to_vec(), 150, |error| {
        matches!(error, AdmissionError::QuorumRejected(_))
    });
    rejected_without_store(&Fixture::new(), b"changed blob".to_vec(), 150, |error| {
        matches!(error, AdmissionError::BlobMismatch { .. })
    });
    let mut tree = Fixture::new();
    tree.expected.output = Tree::new(Domain::Artifact).id().to_string();
    tree.sign_all();
    rejected_without_store(&tree, ARTIFACT.to_vec(), 150, |error| {
        matches!(error, AdmissionError::UnsupportedOutput)
    });
}

#[test]
fn revoked_current_policy_blocks_every_operation_without_filesystem_mutation() {
    let original = Fixture::new();
    let inputs = original.json();
    let dir = tempfile::tempdir().unwrap();
    let accepted = import(&dir, &inputs);
    assert!(accepted.verification.accepted);
    // Public reports can be cloned/forged; neither is an API authorization input.
    let mut forged = accepted;
    forged.verification.accepted = true;
    forged.context.policy_blake3 = "00".repeat(32);
    let mut revoked = original;
    revoked.policy.builders[0].revoked = true;
    let current = revoked.json();
    let store = Store::open(dir.path().join("store")).unwrap();
    let before = snapshot(dir.path());
    let fail = admit_blob(
        store.root(),
        ARTIFACT.to_vec(),
        current.context(150),
        Limits::default(),
    )
    .unwrap_err();
    assert!(matches!(fail.error, AdmissionError::QuorumRejected(_)));
    assert!(matches!(
        retrieve_blob(&store, current.context(150), Limits::default()),
        Err(AdmissionError::QuorumRejected(_))
    ));
    assert!(matches!(
        materialize_blob(
            &store,
            dir.path().join("rejected-copy"),
            current.context(150),
            Limits::default()
        ),
        Err(AdmissionError::QuorumRejected(_))
    ));
    assert_eq!(snapshot(dir.path()), before);
    assert!(!dir.path().join("rejected-copy").exists());
}

#[test]
fn revalidation_at_current_clock_rejects_expiry_future_and_ttl() {
    let fixture = Fixture::new();
    for at in [99, 300, u64::MAX] {
        rejected_without_store(&fixture, ARTIFACT.to_vec(), at, |error| {
            matches!(error, AdmissionError::QuorumRejected(_))
        });
    }
    let mut short_ttl = fixture.clone();
    short_ttl.policy.max_statement_ttl = 199;
    rejected_without_store(&short_ttl, ARTIFACT.to_vec(), 150, |error| {
        matches!(error, AdmissionError::QuorumRejected(_))
    });
    let inputs = fixture.json();
    let dir = tempfile::tempdir().unwrap();
    import(&dir, &inputs);
    let store = Store::open(dir.path().join("store")).unwrap();
    assert!(retrieve_blob(&store, inputs.context(299), Limits::default()).is_ok());
    assert!(matches!(
        retrieve_blob(&store, inputs.context(300), Limits::default()),
        Err(AdmissionError::QuorumRejected(_))
    ));
}

#[test]
fn policy_content_and_order_canonicalization_affect_exact_context_identity() {
    let fixture = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let first = import(&dir, &fixture.json());
    let mut reordered = fixture.clone();
    reordered.policy.builders.reverse();
    reordered.bundle.attestations.reverse();
    let reordered_inputs = reordered.json();
    let store = Store::open(dir.path().join("store")).unwrap();
    let read = retrieve_blob(&store, reordered_inputs.context(150), Limits::default()).unwrap();
    assert_eq!(read.report.context, first.context);
    assert_eq!(
        serde_json::from_slice::<AdmissionReport>(&serde_json::to_vec(&read.report).unwrap())
            .unwrap(),
        read.report
    );
    let again = retrieve_blob(&store, reordered_inputs.context(150), Limits::default()).unwrap();
    assert_eq!(again.report, read.report, "identical report determinism");
    let mut changed = fixture.clone();
    changed.policy.max_statement_ttl += 1;
    let changed_inputs = changed.json();
    let policy_changed =
        retrieve_blob(&store, changed_inputs.context(150), Limits::default()).unwrap();
    assert_ne!(
        policy_changed.report.context.policy_blake3,
        first.context.policy_blake3
    );
    assert_ne!(
        policy_changed.report.context.context_blake3,
        first.context.context_blake3
    );
    let later = retrieve_blob(&store, reordered_inputs.context(151), Limits::default()).unwrap();
    assert_ne!(
        later.report.context.context_blake3,
        first.context.context_blake3
    );
    let limited = retrieve_blob(
        &store,
        reordered_inputs.context(150),
        Limits {
            max_blob_bytes: ARTIFACT.len(),
        },
    )
    .unwrap();
    assert_ne!(
        limited.report.context.context_blake3,
        first.context.context_blake3
    );
}

#[test]
fn rotated_keys_do_not_invent_operator_independence_and_duplicates_fail() {
    let mut fixture = Fixture::new();
    fixture.policy.builders[1].operator_id = fixture.policy.builders[0].operator_id.clone();
    rejected_without_store(&fixture, ARTIFACT.to_vec(), 150, |error| {
        matches!(error, AdmissionError::QuorumRejected(_))
    });
    let signing = SigningKey::from_bytes(&[3; 32]);
    let mut builder = fixture.policy.builders[0].clone();
    builder.key_id = "third-key".into();
    builder.operator_id = "third-operator".into();
    builder.public_key = hex(&signing.verifying_key().to_bytes());
    let mut envelope = fixture.bundle.attestations[0].clone();
    envelope.statement.key_id = builder.key_id.clone();
    envelope.signature = hex(&signing
        .sign(&canonical_signing_bytes(&envelope.statement).unwrap())
        .to_bytes());
    fixture.policy.builders.push(builder);
    fixture.bundle.attestations.push(envelope);
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        import(&dir, &fixture.json())
            .verification
            .verified_operators
            .len(),
        2
    );
    fixture
        .bundle
        .attestations
        .push(fixture.bundle.attestations[0].clone());
    rejected_without_store(&fixture, ARTIFACT.to_vec(), 150, |error| {
        matches!(error, AdmissionError::QuorumRejected(_))
    });
    let mut duplicate = Fixture::new();
    duplicate.policy.builders[1].public_key = duplicate.policy.builders[0].public_key.clone();
    rejected_without_store(&duplicate, ARTIFACT.to_vec(), 150, |error| {
        matches!(error, AdmissionError::QuorumRejected(_))
    });
}

#[test]
fn corrupt_store_bytes_are_rehashed_without_quarantine_or_materialization() {
    let inputs = Fixture::new().json();
    let dir = tempfile::tempdir().unwrap();
    import(&dir, &inputs);
    let store = Store::open(dir.path().join("store")).unwrap();
    let id = blob_id(Domain::Artifact, ARTIFACT);
    let held = retrieve_blob(&store, inputs.context(150), Limits::default()).unwrap();
    write_object(&store.object_path(&id), b"corrupt store payload");
    let before = snapshot(dir.path());
    assert!(matches!(
        retrieve_blob(&store, inputs.context(150), Limits::default()),
        Err(AdmissionError::BlobMismatch { .. })
    ));
    assert!(matches!(
        materialize_blob(
            &store,
            dir.path().join("rejected-copy"),
            inputs.context(150),
            Limits::default()
        ),
        Err(AdmissionError::BlobMismatch { .. })
    ));
    assert_eq!(snapshot(dir.path()), before);
    assert_eq!(
        held.artifact, ARTIFACT,
        "returned bytes own independent storage"
    );
    // Fresh valid owned bytes may repair CAS corruption only after full gates.
    assert!(import(&dir, &inputs).verification.accepted);
    assert_eq!(
        retrieve_blob(&store, inputs.context(150), Limits::default())
            .unwrap()
            .artifact,
        ARTIFACT
    );
}

#[test]
fn bounds_malformed_json_and_storage_failures_preserve_owners() {
    let fixture = Fixture::new();
    let mut inputs = fixture.json();
    let dir = tempfile::tempdir().unwrap();
    for bad_json in [
        b"not JSON".to_vec(),
        br#"{"schema_version":1,"schema_version":1,"builders":[]}"#.to_vec(),
        vec![b' '; MAX_JSON_BYTES + 1],
    ] {
        inputs.policy = bad_json;
        let failure = admit_blob(
            dir.path().join("absent"),
            ARTIFACT.to_vec(),
            inputs.context(150),
            Limits::default(),
        )
        .unwrap_err();
        assert!(matches!(
            failure.error,
            AdmissionError::Parse {
                field: ContextField::Policy,
                ..
            }
        ));
        assert_eq!(failure.artifact, ARTIFACT);
        assert!(!dir.path().join("absent").exists());
    }
    let inputs = fixture.json();
    let failure = admit_blob(
        dir.path().join("absent"),
        ARTIFACT.to_vec(),
        inputs.context(150),
        Limits {
            max_blob_bytes: ARTIFACT.len() - 1,
        },
    )
    .unwrap_err();
    assert!(matches!(failure.error, AdmissionError::BlobLimit));
    let failure = admit_blob(
        dir.path().join("absent"),
        ARTIFACT.to_vec(),
        inputs.context(150),
        Limits {
            max_blob_bytes: usize::MAX,
        },
    )
    .unwrap_err();
    assert!(matches!(failure.error, AdmissionError::InvalidLimits));
    assert!(!dir.path().join("absent").exists());
    let not_directory = dir.path().join("file");
    fs::write(&not_directory, b"retain existing").unwrap();
    let bytes = ARTIFACT.to_vec();
    let pointer = bytes.as_ptr();
    let failure = admit_blob(
        &not_directory,
        bytes,
        inputs.context(150),
        Limits::default(),
    )
    .unwrap_err();
    assert!(matches!(failure.error, AdmissionError::Cas(_)));
    assert_eq!(failure.artifact.as_ptr(), pointer);
    assert_eq!(fs::read(not_directory).unwrap(), b"retain existing");
}

#[test]
fn oversized_stored_object_is_rejected_before_retaining_its_payload() {
    let inputs = Fixture::new().json();
    let dir = tempfile::tempdir().unwrap();
    import(&dir, &inputs);
    let store = Store::open(dir.path().join("store")).unwrap();
    let id = blob_id(Domain::Artifact, ARTIFACT);
    write_object(&store.object_path(&id), &[0; 1024]);
    assert!(matches!(
        retrieve_blob(
            &store,
            inputs.context(150),
            Limits {
                max_blob_bytes: 100
            }
        ),
        Err(AdmissionError::BlobLimit)
    ));
}

#[cfg(unix)]
#[test]
fn symlink_object_never_becomes_an_artifact_read() {
    use std::os::unix::fs::symlink;
    let inputs = Fixture::new().json();
    let dir = tempfile::tempdir().unwrap();
    import(&dir, &inputs);
    let store = Store::open(dir.path().join("store")).unwrap();
    let path = store.object_path(&blob_id(Domain::Artifact, ARTIFACT));
    let outside = dir.path().join("outside");
    fs::write(&outside, ARTIFACT).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();
    assert!(matches!(
        retrieve_blob(&store, inputs.context(150), Limits::default()),
        Err(AdmissionError::UnsafeStoredObject)
    ));
    assert_eq!(fs::read(outside).unwrap(), ARTIFACT);
}

#[test]
fn unused_configured_keys_and_revocation_are_in_the_policy_content_identity() {
    let fixture = Fixture::new();
    let inputs = fixture.json();
    let dir = tempfile::tempdir().unwrap();
    let first = import(&dir, &inputs);
    let store = Store::open(dir.path().join("store")).unwrap();
    let mut extended = fixture;
    let signing = SigningKey::from_bytes(&[7; 32]);
    let mut unused = extended.policy.builders[0].clone();
    unused.key_id = "unused-key".into();
    unused.operator_id = "unused-operator".into();
    unused.public_key = hex(&signing.verifying_key().to_bytes());
    unused.revoked = true;
    extended.policy.builders.push(unused);
    let revoked_inputs = extended.json();
    let revoked = retrieve_blob(&store, revoked_inputs.context(150), Limits::default()).unwrap();
    assert_ne!(
        revoked.report.context.policy_blake3,
        first.context.policy_blake3
    );
    extended.policy.builders.last_mut().unwrap().revoked = false;
    let unrevoked_inputs = extended.json();
    let unrevoked =
        retrieve_blob(&store, unrevoked_inputs.context(150), Limits::default()).unwrap();
    assert_ne!(
        unrevoked.report.context.policy_blake3,
        revoked.report.context.policy_blake3
    );
    assert_eq!(
        unrevoked.report.verification.verified_keys,
        revoked.report.verification.verified_keys
    );
}
