use aws_lc_rs::{rand::SystemRandom, signature::Ed25519KeyPair};
use olpc_cjson::CanonicalFormatter;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs};
use tempfile::TempDir;
use tough::sign::Sign;
use vesper_loom_tuf::{Checkpoint, Limits, LocalRequest, verify_target};

const PAYLOAD: &[u8] = b"authenticated source archive fixture\n";

#[derive(Clone)]
struct Versions {
    timestamp: u64,
    snapshot: u64,
    targets: u64,
    delegated: u64,
}
impl Default for Versions {
    fn default() -> Self {
        Self {
            timestamp: 1,
            snapshot: 1,
            targets: 1,
            delegated: 1,
        }
    }
}

#[derive(Default)]
struct Spec {
    versions: Versions,
    rotated: bool,
    expired: Option<&'static str>,
    length_delta: u64,
    wrong_scope: bool,
}

struct Fixture {
    dir: TempDir,
    bootstrap: Vec<u8>,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn key(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
}
fn key_id(key: &Ed25519KeyPair) -> String {
    hex::encode(key.tuf_key().key_id().unwrap().as_ref())
}
async fn signed(value: Value, keys: &[&Ed25519KeyPair]) -> Vec<u8> {
    let mut bytes = Vec::new();
    value
        .serialize(&mut serde_json::Serializer::with_formatter(
            &mut bytes,
            CanonicalFormatter::new(),
        ))
        .unwrap();
    let mut signatures = Vec::new();
    for key in keys {
        let signature = Sign::sign(*key, &bytes, &SystemRandom::new())
            .await
            .unwrap();
        signatures.push(json!({"keyid":key_id(key),"sig":hex::encode(signature)}));
    }
    serde_json::to_vec(&json!({"signed":value,"signatures":signatures})).unwrap()
}
fn expiry(spec: &Spec, role: &str) -> String {
    let offset = if spec.expired == Some(role) {
        -86400
    } else {
        86400
    };
    jiff::Timestamp::from_second(jiff::Timestamp::now().as_second() + offset)
        .unwrap()
        .to_string()
}
// Fixed whole-second expiry keeps the bootstrap bytes identical across generated snapshots.
fn root_expiry(spec: &Spec) -> &'static str {
    if spec.expired == Some("root") {
        "2000-01-01T00:00:00Z"
    } else {
        "2999-01-01T00:00:00Z"
    }
}
fn root_value(version: u64, key: &Ed25519KeyPair, expires: &str) -> Value {
    let id = key_id(key);
    let roles: BTreeMap<_, _> = ["root", "timestamp", "snapshot", "targets"]
        .into_iter()
        .map(|role| (role, json!({"keyids":[id],"threshold":1})))
        .collect();
    json!({"_type":"root","spec_version":"1.0.0","version":version,"expires":expires,
        "consistent_snapshot":false,"keys":{id:key.tuf_key()},"roles":roles})
}
fn meta(bytes: &[u8], version: u64) -> Value {
    json!({"version":version,"length":bytes.len(),"hashes":{"sha256":digest(bytes)}})
}

impl Fixture {
    async fn new(spec: Spec) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root1 = key(1);
        let root2 = key(2);
        let delegated_key = key(3);
        let bootstrap = signed(root_value(1, &root1, root_expiry(&spec)), &[&root1]).await;
        fs::create_dir(dir.path().join("metadata")).unwrap();
        fs::create_dir_all(dir.path().join("targets/pkg")).unwrap();
        fs::write(dir.path().join("metadata/1.root.json"), &bootstrap).unwrap();
        let active = if spec.rotated {
            let second = signed(
                root_value(2, &root2, "2999-01-01T00:00:00Z"),
                &[&root1, &root2],
            )
            .await;
            fs::write(dir.path().join("metadata/2.root.json"), second).unwrap();
            &root2
        } else {
            &root1
        };
        let target = json!({"length":PAYLOAD.len() as u64 + spec.length_delta,"hashes":{"sha256":digest(PAYLOAD)},"custom":{"loom":{"package":"@example/pkg","version":"1.0.0"}}});
        let delegated = signed(json!({"_type":"targets","spec_version":"1.0.0","version":spec.versions.delegated,"expires":expiry(&spec,"delegated"),
            "targets":{"pkg/artifact.bin":target}}), &[&delegated_key]).await;
        let id = key_id(&delegated_key);
        let targets = signed(json!({"_type":"targets","spec_version":"1.0.0","version":spec.versions.targets,"expires":expiry(&spec,"targets"),
            "targets":{},"delegations":{"keys":{id.clone():delegated_key.tuf_key()},"roles":[{"name":"packages","keyids":[id],"threshold":1,"terminating":true,"paths":[if spec.wrong_scope { "other/*" } else {"pkg/*"}]}]}}), &[active]).await;
        let snapshot = signed(json!({"_type":"snapshot","spec_version":"1.0.0","version":spec.versions.snapshot,"expires":expiry(&spec,"snapshot"),
            "meta":{"targets.json":meta(&targets,spec.versions.targets),"packages.json":meta(&delegated,spec.versions.delegated)}}), &[active]).await;
        let timestamp = signed(json!({"_type":"timestamp","spec_version":"1.0.0","version":spec.versions.timestamp,"expires":expiry(&spec,"timestamp"),
            "meta":{"snapshot.json":meta(&snapshot,spec.versions.snapshot)}}), &[active]).await;
        for (name, bytes) in [
            ("targets.json", targets),
            ("packages.json", delegated),
            ("snapshot.json", snapshot),
            ("timestamp.json", timestamp),
        ] {
            fs::write(dir.path().join("metadata").join(name), bytes).unwrap();
        }
        fs::write(dir.path().join("targets/pkg/artifact.bin"), PAYLOAD).unwrap();
        Self { dir, bootstrap }
    }
    fn request(&self, previous: Option<Checkpoint>) -> LocalRequest {
        LocalRequest {
            repository_id: "example-registry".into(),
            bootstrap_root: self.bootstrap.clone(),
            metadata_dir: self.dir.path().join("metadata"),
            targets_dir: self.dir.path().join("targets"),
            target_name: "pkg/artifact.bin".into(),
            previous,
            limits: Limits::default(),
        }
    }
}

#[tokio::test]
async fn real_delegated_target_roundtrips_checkpoint_and_owned_bytes() {
    let fixture = Fixture::new(Spec::default()).await;
    let verified = verify_target(fixture.request(None)).await.unwrap();
    assert_eq!(verified.bytes, PAYLOAD);
    assert_eq!(verified.sha256, digest(PAYLOAD));
    assert_eq!(verified.length, PAYLOAD.len() as u64);
    assert_eq!(verified.checkpoint.root_version().unwrap(), 1);
    assert_eq!(verified.custom["loom"]["package"], "@example/pkg");
    let checkpoint =
        Checkpoint::from_json(&verified.checkpoint.to_json().unwrap(), &Limits::default()).unwrap();
    fs::write(
        fixture.dir.path().join("targets/pkg/artifact.bin"),
        b"changed after successful verification",
    )
    .unwrap();
    assert_eq!(verified.bytes, PAYLOAD);
    assert!(
        verify_target(fixture.request(Some(checkpoint)))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn root_rotation_survives_removal_of_previous_root_files() {
    let fixture = Fixture::new(Spec {
        rotated: true,
        ..Spec::default()
    })
    .await;
    let verified = verify_target(fixture.request(None)).await.unwrap();
    assert_eq!(verified.checkpoint.root_version().unwrap(), 2);
    fs::remove_file(fixture.dir.path().join("metadata/1.root.json")).unwrap();
    fs::remove_file(fixture.dir.path().join("metadata/2.root.json")).unwrap();
    let update = verify_target(fixture.request(Some(verified.checkpoint)))
        .await
        .unwrap();
    assert_eq!(update.checkpoint.root_version().unwrap(), 2);
    let old = Fixture::new(Spec::default()).await;
    let error = verify_target(old.request(Some(update.checkpoint)))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("verification"), "{error}");
}

#[tokio::test]
async fn all_persisted_role_version_floors_reject_valid_signed_rollbacks() {
    let initial = Fixture::new(Spec {
        versions: Versions {
            timestamp: 3,
            snapshot: 3,
            targets: 3,
            delegated: 3,
        },
        ..Spec::default()
    })
    .await;
    let checkpoint = verify_target(initial.request(None))
        .await
        .unwrap()
        .checkpoint;
    for versions in [
        Versions {
            timestamp: 2,
            snapshot: 3,
            targets: 3,
            delegated: 3,
        },
        Versions {
            timestamp: 4,
            snapshot: 2,
            targets: 3,
            delegated: 3,
        },
        Versions {
            timestamp: 4,
            snapshot: 4,
            targets: 2,
            delegated: 3,
        },
        Versions {
            timestamp: 4,
            snapshot: 4,
            targets: 4,
            delegated: 2,
        },
    ] {
        let fixture = Fixture::new(Spec {
            versions,
            ..Spec::default()
        })
        .await;
        let error = verify_target(fixture.request(Some(checkpoint.clone())))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("rollback"), "{error}");
    }
    let next = Fixture::new(Spec {
        versions: Versions {
            timestamp: 4,
            snapshot: 4,
            targets: 4,
            delegated: 4,
        },
        ..Spec::default()
    })
    .await;
    verify_target(next.request(Some(checkpoint))).await.unwrap();
}

/// Documents the verifier boundary: no state advancement is returned on failure. A complete TUF
/// updater must retain each newly authenticated role even if a later download fails.
#[tokio::test]
async fn failed_target_leaves_older_checkpoint_usable_despite_newer_authenticated_metadata() {
    let old = Fixture::new(Spec::default()).await;
    let checkpoint = verify_target(old.request(None)).await.unwrap().checkpoint;
    let newer = Fixture::new(Spec {
        rotated: true,
        versions: Versions {
            timestamp: 2,
            snapshot: 2,
            targets: 2,
            delegated: 2,
        },
        ..Spec::default()
    })
    .await;
    // First prove these newer metadata bytes are valid and advance the successful state.
    let advanced = verify_target(newer.request(Some(checkpoint.clone())))
        .await
        .unwrap()
        .checkpoint;
    assert!(verify_target(old.request(Some(advanced))).await.is_err());
    // A target failure discards that advancement in this API. The caller only has its old state.
    fs::write(
        newer.dir.path().join("targets/pkg/artifact.bin"),
        b"corrupt",
    )
    .unwrap();
    assert!(
        verify_target(newer.request(Some(checkpoint.clone())))
            .await
            .is_err()
    );
    verify_target(old.request(Some(checkpoint))).await.unwrap();
}

#[tokio::test]
async fn expiry_freeze_checks_include_every_role_and_root() {
    for role in ["root", "timestamp", "snapshot", "targets", "delegated"] {
        let fixture = Fixture::new(Spec {
            expired: Some(role),
            ..Spec::default()
        })
        .await;
        let error = verify_target(fixture.request(None))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("expired"), "{role}: {error}");
    }
}

#[tokio::test]
async fn wrong_hash_truncation_and_shorter_than_signed_length_return_no_update() {
    for contents in [
        vec![b'x'; PAYLOAD.len()],
        PAYLOAD[..PAYLOAD.len() - 1].to_vec(),
        [PAYLOAD, b"extra"].concat(),
    ] {
        let fixture = Fixture::new(Spec::default()).await;
        fs::write(
            fixture.dir.path().join("targets/pkg/artifact.bin"),
            contents,
        )
        .unwrap();
        assert!(verify_target(fixture.request(None)).await.is_err());
    }
    let fixture = Fixture::new(Spec {
        length_delta: 1,
        ..Spec::default()
    })
    .await;
    let error = verify_target(fixture.request(None))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("differs from signed length"), "{error}");
}

#[tokio::test]
async fn invalid_signature_and_snapshot_hash_are_not_accepted() {
    for filename in [
        "timestamp.json",
        "snapshot.json",
        "targets.json",
        "packages.json",
    ] {
        let fixture = Fixture::new(Spec::default()).await;
        let path = fixture.dir.path().join("metadata").join(filename);
        let mut document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        document["signatures"][0]["sig"] = json!("00".repeat(64));
        fs::write(path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(
            verify_target(fixture.request(None)).await.is_err(),
            "{filename}"
        );
    }
}

#[tokio::test]
async fn delegation_scope_cannot_be_bypassed_by_raw_target_enumeration() {
    let fixture = Fixture::new(Spec {
        wrong_scope: true,
        ..Spec::default()
    })
    .await;
    assert!(verify_target(fixture.request(None)).await.is_err());
}

fn edited_checkpoint(
    checkpoint: &Checkpoint,
    mutate: impl FnOnce(&mut Value),
    update_checksum: bool,
) -> Checkpoint {
    let mut value: Value = serde_json::from_slice(&checkpoint.to_json().unwrap()).unwrap();
    mutate(&mut value);
    if update_checksum {
        let mut bytes = Vec::new();
        value["state"]
            .serialize(&mut serde_json::Serializer::with_formatter(
                &mut bytes,
                CanonicalFormatter::new(),
            ))
            .unwrap();
        value["checksum"] = json!(digest(&bytes));
    }
    Checkpoint::from_json(&serde_json::to_vec(&value).unwrap(), &Limits::default()).unwrap()
}

#[tokio::test]
async fn corrupt_mismatched_or_missing_checkpoint_fields_fail_closed() {
    let fixture = Fixture::new(Spec::default()).await;
    let checkpoint = verify_target(fixture.request(None))
        .await
        .unwrap()
        .checkpoint;
    for field in ["timestamp_version", "snapshot_version"] {
        let invalid = edited_checkpoint(&checkpoint, |v| v["state"][field] = json!(999), false);
        assert!(verify_target(fixture.request(Some(invalid))).await.is_err());
    }
    let mut request = fixture.request(Some(checkpoint.clone()));
    request.repository_id = "different-registry".into();
    assert!(verify_target(request).await.is_err());
    let mut request = fixture.request(Some(checkpoint.clone()));
    request.bootstrap_root.push(b' ');
    assert!(verify_target(request).await.is_err());
    let invalid = edited_checkpoint(&checkpoint, |v| v["state"]["roots"][0] = json!([0]), true);
    assert!(verify_target(fixture.request(Some(invalid))).await.is_err());
    let future = edited_checkpoint(
        &checkpoint,
        |v| v["state"]["verified_at"] = json!("2999-01-01T00:00:00Z"),
        true,
    );
    assert!(
        verify_target(fixture.request(Some(future)))
            .await
            .unwrap_err()
            .to_string()
            .contains("precedes checkpoint")
    );
    let rotated = Fixture::new(Spec {
        rotated: true,
        ..Spec::default()
    })
    .await;
    let rotated_checkpoint = verify_target(rotated.request(None))
        .await
        .unwrap()
        .checkpoint;
    let invalid = edited_checkpoint(
        &rotated_checkpoint,
        |v| {
            let bytes: Vec<u8> = serde_json::from_value(v["state"]["roots"][1].clone()).unwrap();
            let mut root: Value = serde_json::from_slice(&bytes).unwrap();
            root["signed"]["expires"] = json!("2998-01-01T00:00:00Z");
            v["state"]["roots"][1] = json!(serde_json::to_vec(&root).unwrap());
        },
        true,
    );
    assert!(
        verify_target(rotated.request(Some(invalid)))
            .await
            .unwrap_err()
            .to_string()
            .contains("threshold")
    );
    assert!(Checkpoint::from_json(b"{}", &Limits::default()).is_err());
    let limits = Limits {
        checkpoint_bytes: 1,
        ..Limits::default()
    };
    assert!(Checkpoint::from_json(&checkpoint.to_json().unwrap(), &limits).is_err());
}

#[tokio::test]
async fn noncanonical_paths_and_resource_exhaustion_are_rejected() {
    let fixture = Fixture::new(Spec::default()).await;
    for name in [
        "../pkg/artifact.bin",
        "/pkg/artifact.bin",
        "pkg//artifact.bin",
        "pkg/./artifact.bin",
        "pkg\\artifact.bin",
        "pkg/%2e%2e/artifact.bin",
        "file:///etc/passwd",
        "pkg/CON",
        "pkg/NUL.txt",
        "pkg/COM1",
        "pkg/LPT¹",
        "pkg/trailing.",
        "pkg/trailing ",
        "",
    ] {
        let mut request = fixture.request(None);
        request.target_name = name.into();
        assert!(verify_target(request).await.is_err(), "{name}");
    }
    for which in 0..7 {
        let mut request = fixture.request(None);
        match which {
            0 => request.limits.metadata_file_bytes = 1,
            1 => request.limits.metadata_total_bytes = 1,
            2 => request.limits.target_bytes = 1,
            3 => request.limits.targets_total_bytes = 1,
            4 => request.limits.files = 1,
            5 => request.limits.roles = 1,
            _ => request.limits.checkpoint_bytes = 1,
        }
        assert!(verify_target(request).await.is_err(), "limit {which}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_and_special_files_are_rejected_without_following() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(Spec::default()).await;
    let target = fixture.dir.path().join("targets/pkg/artifact.bin");
    fs::remove_file(&target).unwrap();
    symlink("/dev/zero", &target).unwrap();
    assert!(verify_target(fixture.request(None)).await.is_err());
    let fixture = Fixture::new(Spec::default()).await;
    let link = fixture.dir.path().join("link");
    symlink(fixture.dir.path().join("metadata"), &link).unwrap();
    let mut request = fixture.request(None);
    request.metadata_dir = link;
    assert!(verify_target(request).await.is_err());
    let fixture = Fixture::new(Spec::default()).await;
    let fifo = fixture.dir.path().join("targets/fifo");
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is a valid NUL-terminated pathname and mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(verify_target(fixture.request(None)).await.is_err());
}

#[tokio::test]
async fn metadata_directory_must_be_flat_bounded_json() {
    for name in ["nested", "not-json.txt"] {
        let fixture = Fixture::new(Spec::default()).await;
        let path = fixture.dir.path().join("metadata").join(name);
        if name == "nested" {
            fs::create_dir(path).unwrap();
        } else {
            fs::write(path, b"unrelated").unwrap();
        }
        assert!(verify_target(fixture.request(None)).await.is_err());
    }
}

#[tokio::test]
async fn delegation_cycles_and_depth_are_bounded_before_signature_work() {
    let fixture = Fixture::new(Spec::default()).await;
    let path = fixture.dir.path().join("metadata/packages.json");
    let mut document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    document["signed"]["delegations"] = json!({"roles":[{"name":"packages"}]});
    fs::write(path, serde_json::to_vec(&document).unwrap()).unwrap();
    let error = verify_target(fixture.request(None))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("cycle"), "{error}");
    let fixture = Fixture::new(Spec::default()).await;
    fs::create_dir_all(fixture.dir.path().join("targets/a/b/c")).unwrap();
    let mut request = fixture.request(None);
    request.limits.directory_depth = 1;
    assert!(
        verify_target(request)
            .await
            .unwrap_err()
            .to_string()
            .contains("directory depth")
    );
}

#[tokio::test]
async fn numeric_role_names_cannot_disconnect_preflight_depth_or_cycle_edges() {
    for cycle in [false, true] {
        let fixture = Fixture::new(Spec::default()).await;
        let graph = if cycle {
            vec![("targets", "1.a"), ("1.a", "1.a")]
        } else {
            vec![("targets", "1.a"), ("1.a", "2.b"), ("2.b", "3.c")]
        };
        for (name, child) in graph {
            let document = json!({"signed":{"delegations":{"roles":[{"name":child}]}}});
            fs::write(
                fixture
                    .dir
                    .path()
                    .join("metadata")
                    .join(format!("{name}.json")),
                serde_json::to_vec(&document).unwrap(),
            )
            .unwrap();
        }
        let mut request = fixture.request(None);
        request.limits.directory_depth = 1;
        let error = verify_target(request).await.unwrap_err().to_string();
        assert!(error.contains("delegation graph depth or cycle"), "{error}");
    }
}
