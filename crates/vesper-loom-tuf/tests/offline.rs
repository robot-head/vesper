use aws_lc_rs::{rand::SystemRandom, signature::Ed25519KeyPair};
use olpc_cjson::CanonicalFormatter;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs};
use tempfile::TempDir;
use tough::sign::Sign;
use vesper_loom_tuf::{Checkpoint, Limits, LocalRequest, verify_target, verify_target_attempt};

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
    snapshot_length_delta: u64,
    targets_length_delta: u64,
    delegated_length_delta: u64,
    target_name: Option<&'static str>,
    consistent: bool,
}

struct Fixture {
    dir: TempDir,
    bootstrap: Vec<u8>,
    target_name: String,
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
fn root_value(version: u64, key: &Ed25519KeyPair, expires: &str, consistent: bool) -> Value {
    let id = key_id(key);
    let roles: BTreeMap<_, _> = ["root", "timestamp", "snapshot", "targets"]
        .into_iter()
        .map(|role| (role, json!({"keyids":[id],"threshold":1})))
        .collect();
    json!({"_type":"root","spec_version":"1.0.0","version":version,"expires":expires,
        "consistent_snapshot":consistent,"keys":{id:key.tuf_key()},"roles":roles})
}
fn meta(bytes: &[u8], version: u64) -> Value {
    json!({"version":version,"length":bytes.len(),"hashes":{"sha256":digest(bytes)}})
}
fn metadata_pin(bytes: &[u8], version: u64, extra_length: u64) -> Value {
    let mut value = meta(bytes, version);
    value["length"] = json!(bytes.len() as u64 + extra_length);
    value
}

impl Fixture {
    async fn new(spec: Spec) -> Self {
        Self::new_in(spec, tempfile::tempdir().unwrap()).await
    }
    async fn new_in(spec: Spec, dir: TempDir) -> Self {
        let root1 = key(1);
        let root2 = key(2);
        let delegated_key = key(3);
        let bootstrap = signed(
            root_value(1, &root1, root_expiry(&spec), spec.consistent),
            &[&root1],
        )
        .await;
        fs::create_dir(dir.path().join("metadata")).unwrap();
        fs::create_dir_all(dir.path().join("targets/pkg")).unwrap();
        fs::write(dir.path().join("metadata/1.root.json"), &bootstrap).unwrap();
        let active = if spec.rotated {
            let second = signed(
                root_value(2, &root2, "2999-01-01T00:00:00Z", spec.consistent),
                &[&root1, &root2],
            )
            .await;
            fs::write(dir.path().join("metadata/2.root.json"), second).unwrap();
            &root2
        } else {
            &root1
        };
        let target = json!({"length":PAYLOAD.len() as u64 + spec.length_delta,"hashes":{"sha256":digest(PAYLOAD)},"custom":{"loom":{"package":"@example/pkg","version":"1.0.0"}}});
        let target_name = spec.target_name.unwrap_or("pkg/artifact.bin");
        let delegated = signed(json!({"_type":"targets","spec_version":"1.0.0","version":spec.versions.delegated,"expires":expiry(&spec,"delegated"),
            "targets":{target_name:target}}), &[&delegated_key]).await;
        let id = key_id(&delegated_key);
        let targets = signed(json!({"_type":"targets","spec_version":"1.0.0","version":spec.versions.targets,"expires":expiry(&spec,"targets"),
            "targets":{},"delegations":{"keys":{id.clone():delegated_key.tuf_key()},"roles":[{"name":"packages","keyids":[id],"threshold":1,"terminating":true,"paths":[if spec.wrong_scope { "other/*" } else {"pkg/*"}]}]}}), &[active]).await;
        let snapshot = signed(json!({"_type":"snapshot","spec_version":"1.0.0","version":spec.versions.snapshot,"expires":expiry(&spec,"snapshot"),
            "meta":{"targets.json":metadata_pin(&targets,spec.versions.targets,spec.targets_length_delta),"packages.json":metadata_pin(&delegated,spec.versions.delegated,spec.delegated_length_delta)}}), &[active]).await;
        let timestamp = signed(json!({"_type":"timestamp","spec_version":"1.0.0","version":spec.versions.timestamp,"expires":expiry(&spec,"timestamp"),
            "meta":{"snapshot.json":metadata_pin(&snapshot,spec.versions.snapshot,spec.snapshot_length_delta)}}), &[active]).await;
        for (name, version, bytes) in [
            ("targets.json", spec.versions.targets, targets),
            ("packages.json", spec.versions.delegated, delegated),
            ("snapshot.json", spec.versions.snapshot, snapshot),
            ("timestamp.json", spec.versions.timestamp, timestamp),
        ] {
            let name = if spec.consistent && name != "timestamp.json" {
                format!("{version}.{name}")
            } else {
                name.into()
            };
            fs::write(dir.path().join("metadata").join(name), bytes).unwrap();
        }
        let filename = if spec.consistent {
            format!("{}.{target_name}", digest(PAYLOAD))
        } else {
            target_name.into()
        };
        let target_path = dir.path().join("targets").join(filename);
        fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        fs::write(target_path, PAYLOAD).unwrap();
        Self {
            dir,
            bootstrap,
            target_name: target_name.to_owned(),
        }
    }
    fn request(&self, previous: Option<Checkpoint>) -> LocalRequest {
        LocalRequest {
            repository_id: "example-registry".into(),
            bootstrap_root: self.bootstrap.clone(),
            metadata_dir: self.dir.path().join("metadata"),
            targets_dir: self.dir.path().join("targets"),
            target_name: self.target_name.clone(),
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

#[cfg(unix)]
#[tokio::test]
async fn symlink_ancestor_in_writable_directory_is_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = Fixture::new(Spec::default()).await;
    // A link inside a world-writable directory could be replaced by any local user, so it is
    // never trusted, whoever owns it. This also holds when the tests run as root.
    let shared = fixture.dir.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
    let link = shared.join("repo");
    symlink(fixture.dir.path(), &link).unwrap();
    let mut request = fixture.request(None);
    request.metadata_dir = link.join("metadata");
    let error = verify_target(request).await.unwrap_err().to_string();
    assert!(error.contains("symlink directory ancestor"), "{error}");
    let mut request = fixture.request(None);
    request.targets_dir = link.join("targets");
    let error = verify_target(request).await.unwrap_err().to_string();
    assert!(error.contains("symlink directory ancestor"), "{error}");
}

/// Regression: macOS temporary directories live under `/var`, a root-owned link to
/// `/private/var`. Verification must accept repositories below such system links.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn system_symlink_ancestor_is_accepted() {
    // Build the fixture explicitly below `/var` so the test does not depend on `TMPDIR`.
    assert!(
        fs::symlink_metadata("/var")
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let dir = tempfile::tempdir_in("/var/tmp").unwrap();
    let fixture = Fixture::new_in(Spec::default(), dir).await;
    verify_target(fixture.request(None)).await.unwrap();
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

fn corrupt_signature(fixture: &Fixture, filename: &str) {
    let path = fixture.dir.path().join("metadata").join(filename);
    let mut document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    document["signatures"][0]["sig"] = json!("00".repeat(64));
    fs::write(path, serde_json::to_vec(&document).unwrap()).unwrap();
}

fn version_two() -> Spec {
    Spec {
        versions: Versions {
            timestamp: 2,
            snapshot: 2,
            targets: 2,
            delegated: 2,
        },
        ..Spec::default()
    }
}

#[tokio::test]
async fn attempt_retains_root_advancement_when_timestamp_fails() {
    let newer = Fixture::new(Spec {
        rotated: true,
        ..Spec::default()
    })
    .await;
    corrupt_signature(&newer, "timestamp.json");
    let attempt = verify_target_attempt(newer.request(None)).await;
    assert!(attempt.result.is_err());
    let checkpoint = attempt.checkpoint.expect("authenticated root progression");
    assert_eq!(checkpoint.root_version().unwrap(), 2);
    let wire: Value = serde_json::from_slice(&checkpoint.to_json().unwrap()).unwrap();
    assert_eq!(wire["state"]["schema_version"], 2);
    assert_eq!(wire["state"]["timestamp_version"], 0);
    assert_eq!(wire["state"]["snapshot_version"], 0);
    assert_eq!(wire["state"]["role_versions"], json!({}));
    let checkpoint =
        Checkpoint::from_json(&checkpoint.to_json().unwrap(), &Limits::default()).unwrap();
    let old = Fixture::new(Spec::default()).await;
    assert!(
        verify_target_attempt(old.request(Some(checkpoint.clone())))
            .await
            .result
            .is_err()
    );
    let valid = Fixture::new(Spec {
        rotated: true,
        ..Spec::default()
    })
    .await;
    fs::remove_file(valid.dir.path().join("metadata/1.root.json")).unwrap();
    fs::remove_file(valid.dir.path().join("metadata/2.root.json")).unwrap();
    assert_eq!(
        verify_target_attempt(valid.request(Some(checkpoint)))
            .await
            .result
            .unwrap()
            .bytes,
        PAYLOAD
    );
}

#[tokio::test]
async fn attempt_retains_timestamp_and_snapshot_pin_when_snapshot_body_fails() {
    let old = Fixture::new(Spec::default()).await;
    let previous = verify_target(old.request(None)).await.unwrap().checkpoint;
    let newer = Fixture::new(version_two()).await;
    corrupt_signature(&newer, "snapshot.json");
    let attempt = verify_target_attempt(newer.request(Some(previous))).await;
    assert!(attempt.result.is_err());
    let checkpoint = attempt.checkpoint.unwrap();
    let wire: Value = serde_json::from_slice(&checkpoint.to_json().unwrap()).unwrap();
    assert_eq!(wire["state"]["timestamp_version"], 2);
    assert_eq!(wire["state"]["snapshot_version"], 2);
    assert_eq!(wire["state"]["role_versions"]["packages.json"], 1);
    assert!(
        verify_target_attempt(old.request(Some(checkpoint.clone())))
            .await
            .result
            .is_err()
    );
    let rollback_pin = Fixture::new(Spec {
        versions: Versions {
            timestamp: 3,
            snapshot: 1,
            targets: 3,
            delegated: 3,
        },
        ..Spec::default()
    })
    .await;
    let replay = verify_target_attempt(rollback_pin.request(Some(checkpoint.clone()))).await;
    assert!(
        replay
            .result
            .unwrap_err()
            .to_string()
            .contains("pinned snapshot rollback")
    );
    let valid = Fixture::new(version_two()).await;
    verify_target_attempt(valid.request(Some(checkpoint)))
        .await
        .result
        .unwrap();
}

#[tokio::test]
async fn attempt_retains_snapshot_role_floors_when_delegation_or_target_fails() {
    for fail_delegation in [true, false] {
        let old = Fixture::new(Spec::default()).await;
        let previous = verify_target(old.request(None)).await.unwrap().checkpoint;
        let newer = Fixture::new(version_two()).await;
        if fail_delegation {
            corrupt_signature(&newer, "packages.json");
        } else {
            fs::write(
                newer.dir.path().join("targets/pkg/artifact.bin"),
                b"corrupt",
            )
            .unwrap();
        }
        let attempt = verify_target_attempt(newer.request(Some(previous))).await;
        assert!(attempt.result.is_err());
        let checkpoint = attempt.checkpoint.unwrap();
        let wire: Value = serde_json::from_slice(&checkpoint.to_json().unwrap()).unwrap();
        assert_eq!(wire["state"]["timestamp_version"], 2);
        assert_eq!(wire["state"]["snapshot_version"], 2);
        assert_eq!(wire["state"]["role_versions"]["packages.json"], 2);
        assert!(
            verify_target_attempt(old.request(Some(checkpoint.clone())))
                .await
                .result
                .is_err()
        );
        let rollback = Fixture::new(Spec {
            versions: Versions {
                timestamp: 3,
                snapshot: 3,
                targets: 3,
                delegated: 1,
            },
            ..Spec::default()
        })
        .await;
        let replay = verify_target_attempt(rollback.request(Some(checkpoint))).await;
        assert!(
            replay
                .result
                .unwrap_err()
                .to_string()
                .contains("role rollback")
        );
    }
}

#[tokio::test]
async fn attempt_does_not_promote_unauthenticated_metadata_or_unverified_state() {
    let fixture = Fixture::new(Spec::default()).await;
    corrupt_signature(&fixture, "timestamp.json");
    let attempt = verify_target_attempt(fixture.request(None)).await;
    assert!(attempt.result.is_err());
    let checkpoint = attempt.checkpoint.unwrap();
    let valid = Fixture::new(Spec::default()).await;
    let update = verify_target_attempt(valid.request(Some(checkpoint)))
        .await
        .result
        .unwrap();
    assert_eq!(update.metadata.timestamp, 1);
    let invalid = edited_checkpoint(
        &update.checkpoint,
        |value| value["state"]["repository_id"] = json!("different-registry"),
        true,
    );
    let attempt = verify_target_attempt(valid.request(Some(invalid))).await;
    assert!(attempt.result.is_err());
    assert!(attempt.checkpoint.is_none());
    let legacy = edited_checkpoint(
        &update.checkpoint,
        |value| value["state"]["schema_version"] = json!(1),
        true,
    );
    verify_target_attempt(valid.request(Some(legacy)))
        .await
        .result
        .unwrap();
}

#[tokio::test]
async fn checkpoint_budget_is_reserved_before_remote_root_authentication() {
    let bad = Fixture::new(Spec::default()).await;
    corrupt_signature(&bad, "timestamp.json");
    let baseline = verify_target_attempt(bad.request(None))
        .await
        .checkpoint
        .unwrap();
    let newer = Fixture::new(Spec {
        rotated: true,
        ..Spec::default()
    })
    .await;
    let mut request = newer.request(Some(baseline.clone()));
    request.limits.checkpoint_bytes = baseline.to_json().unwrap().len() + 64;
    let attempt = verify_target_attempt(request).await;
    assert!(
        attempt
            .result
            .unwrap_err()
            .to_string()
            .contains("checkpoint exceeds limit")
    );
    assert_eq!(attempt.checkpoint.unwrap().root_version().unwrap(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn store_persists_failure_progress_across_reopen_then_allows_verified_success() {
    use vesper_loom_tuf::state::CheckpointStore;
    let state_root = tempfile::tempdir().unwrap();
    let path = state_root.path().join("registry");
    let limits = Limits::default();
    let mut store = CheckpointStore::create(&path).unwrap();
    let newer = Fixture::new(version_two()).await;
    corrupt_signature(&newer, "packages.json");
    let attempt =
        verify_target_attempt(newer.request(store.load_checkpoint(&limits).unwrap())).await;
    assert!(attempt.result.is_err());
    store
        .commit_checkpoint(&attempt.checkpoint.unwrap(), &limits)
        .unwrap();
    drop(store);
    let mut store = CheckpointStore::open(&path).unwrap();
    let old = Fixture::new(Spec::default()).await;
    let attempt = verify_target_attempt(old.request(store.load_checkpoint(&limits).unwrap())).await;
    assert!(attempt.result.is_err());
    store
        .commit_checkpoint(&attempt.checkpoint.unwrap(), &limits)
        .unwrap();
    drop(store);
    let mut store = CheckpointStore::open(&path).unwrap();
    let valid = Fixture::new(version_two()).await;
    let attempt =
        verify_target_attempt(valid.request(store.load_checkpoint(&limits).unwrap())).await;
    assert_eq!(attempt.result.unwrap().bytes, PAYLOAD);
    store
        .commit_checkpoint(&attempt.checkpoint.unwrap(), &limits)
        .unwrap();
    drop(store);
    let store = CheckpointStore::open(&path).unwrap();
    let attempt =
        verify_target_attempt(valid.request(store.load_checkpoint(&limits).unwrap())).await;
    assert_eq!(attempt.result.unwrap().bytes, PAYLOAD);
}

#[tokio::test]
async fn source_directory_spaces_do_not_require_encoded_temporary_paths() {
    let fixture = Fixture::new(Spec::default()).await;
    let metadata = fixture.dir.path().join("metadata with space é %");
    let targets = fixture.dir.path().join("targets with space é %");
    fs::rename(fixture.dir.path().join("metadata"), &metadata).unwrap();
    fs::rename(fixture.dir.path().join("targets"), &targets).unwrap();
    let mut request = fixture.request(None);
    request.metadata_dir = metadata;
    request.targets_dir = targets;
    verify_target_attempt(request).await.result.unwrap();
}

async fn raw_tough_repository(fixture: &Fixture) -> tough::Repository {
    tough::RepositoryLoader::new(
        &fixture.bootstrap,
        url::Url::from_directory_path(fixture.dir.path().join("metadata")).unwrap(),
        url::Url::from_directory_path(fixture.dir.path().join("targets")).unwrap(),
    )
    .transport(tough::FilesystemTransport)
    .expiration_enforcement(tough::ExpirationEnforcement::Safe)
    .load()
    .await
    .unwrap()
}

#[tokio::test]
async fn exact_metadata_lengths_reject_larger_signed_pins_and_retain_error_progress() {
    use tough::IntoVec;
    for consistent in [false, true] {
        for which in 0..3 {
            let old = Fixture::new(Spec {
                consistent,
                ..Spec::default()
            })
            .await;
            let previous = verify_target_attempt(old.request(None))
                .await
                .result
                .unwrap()
                .checkpoint;
            let mut spec = version_two();
            spec.consistent = consistent;
            match which {
                0 => spec.snapshot_length_delta = 1,
                1 => spec.targets_length_delta = 1,
                _ => spec.delegated_length_delta = 1,
            }
            let fixture = Fixture::new(spec).await;
            // Independent confirmation: Tough 0.24 accepts all three larger length pins,
            // despite matching signatures/hashes, and returns the valid target bytes.
            let backend = raw_tough_repository(&fixture).await;
            let name = tough::TargetName::new("pkg/artifact.bin").unwrap();
            assert_eq!(
                backend
                    .read_target(&name)
                    .await
                    .unwrap()
                    .unwrap()
                    .into_vec()
                    .await
                    .unwrap(),
                PAYLOAD
            );
            let attempt = verify_target_attempt(fixture.request(Some(previous))).await;
            let error = attempt.result.unwrap_err().to_string();
            let role = match which {
                0 => "snapshot.json",
                1 => "targets.json",
                _ => "packages.json",
            };
            assert!(
                error.contains(&format!("metadata length differs from signed pin: {role}")),
                "{error}"
            );
            let checkpoint = attempt.checkpoint.unwrap();
            let wire: Value = serde_json::from_slice(&checkpoint.to_json().unwrap()).unwrap();
            assert_eq!(wire["state"]["timestamp_version"], 2);
            assert_eq!(wire["state"]["snapshot_version"], 2);
            assert_eq!(
                wire["state"]["role_versions"]["packages.json"],
                if which == 0 { 1 } else { 2 }
            );
            assert!(
                verify_target_attempt(old.request(Some(checkpoint.clone())))
                    .await
                    .result
                    .is_err()
            );
            let valid = Fixture::new(Spec {
                consistent,
                ..version_two()
            })
            .await;
            assert_eq!(
                verify_target_attempt(valid.request(Some(checkpoint)))
                    .await
                    .result
                    .unwrap()
                    .bytes,
                PAYLOAD
            );
        }
    }
}

#[tokio::test]
async fn unicode_target_names_are_rejected_explicitly_and_supported_ascii_names_work() {
    let fixture = Fixture::new(Spec {
        target_name: Some("pkg/é.bin"),
        ..Spec::default()
    })
    .await;
    let backend = raw_tough_repository(&fixture).await;
    let name = tough::TargetName::new("pkg/é.bin").unwrap();
    // The metadata is real and valid; Tough preserves the escaped leaf name and cannot open it.
    backend.targets().signed.find_target(&name, false).unwrap();
    assert!(backend.read_target(&name).await.is_err());
    let attempt = verify_target_attempt(fixture.request(None)).await;
    assert!(
        attempt
            .result
            .unwrap_err()
            .to_string()
            .contains("supported ASCII relative path")
    );
    assert!(attempt.checkpoint.is_none());
    let valid = Fixture::new(Spec {
        target_name: Some("pkg/@example+v1~.bin"),
        ..Spec::default()
    })
    .await;
    assert_eq!(
        verify_target_attempt(valid.request(None))
            .await
            .result
            .unwrap()
            .bytes,
        PAYLOAD
    );
    for name in ["pkg/name space.bin", "pkg/[name].bin", "pkg/naïve.bin"] {
        let mut request = valid.request(None);
        request.target_name = name.into();
        assert!(
            verify_target_attempt(request)
                .await
                .result
                .unwrap_err()
                .to_string()
                .contains("supported ASCII relative path")
        );
    }
}

#[cfg(unix)]
#[test]
fn owned_updater_persists_failure_floors_rejects_replay_and_commits_success() {
    use vesper_loom_tuf::{
        state::CheckpointStore,
        updater::{UpdateRequest, start_update},
    };
    fn request(fixture: &Fixture) -> UpdateRequest {
        let local = fixture.request(None);
        UpdateRequest {
            repository_id: local.repository_id,
            bootstrap_root: local.bootstrap_root,
            metadata_dir: local.metadata_dir,
            targets_dir: local.targets_dir,
            target_name: local.target_name,
            limits: local.limits,
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-state");
    let newer = runtime.block_on(Fixture::new(version_two()));
    corrupt_signature(&newer, "packages.json");
    let error = start_update(CheckpointStore::create(&path).unwrap(), request(&newer))
        .unwrap()
        .wait()
        .unwrap_err();
    assert!(error.to_string().contains("verification"));
    let store = CheckpointStore::open(&path).unwrap();
    let limits = Limits::default();
    let checkpoint = store.load_checkpoint(&limits).unwrap().unwrap();
    let wire: Value = serde_json::from_slice(&checkpoint.to_json().unwrap()).unwrap();
    assert_eq!(wire["state"]["timestamp_version"], 2);
    assert_eq!(wire["state"]["role_versions"]["packages.json"], 2);
    let old = runtime.block_on(Fixture::new(Spec::default()));
    assert!(start_update(store, request(&old)).unwrap().wait().is_err());
    let valid = runtime.block_on(Fixture::new(version_two()));
    let update = start_update(CheckpointStore::open(&path).unwrap(), request(&valid))
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(update.bytes, PAYLOAD);
    let stored = CheckpointStore::open(&path)
        .unwrap()
        .load_checkpoint(&limits)
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.to_json().unwrap(),
        update.checkpoint.to_json().unwrap()
    );
}
