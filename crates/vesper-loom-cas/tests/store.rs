use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;
use vesper_loom_cas::*;

fn store() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

fn tmp_entries(store: &Store) -> usize {
    fs::read_dir(store.root().join("tmp")).unwrap().count()
}

fn make_writable(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(path, perms).unwrap();
}

fn corrupt_byte(path: &Path, index: usize) {
    make_writable(path);
    let mut bytes = fs::read(path).unwrap();
    bytes[index] ^= 0x01;
    fs::write(path, bytes).unwrap();
}

/// Siblings of `dir` whose names start with `.` (materialization staging).
fn staging_leftovers(parent: &Path) -> Vec<PathBuf> {
    fs::read_dir(parent)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("vesper-staging-")
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Deterministic identities
// ---------------------------------------------------------------------------

/// Pinned vectors. They were reproduced independently with the Python
/// `blake3` package from the documented encodings, so a change here is a
/// schema change, not a refactor.
#[test]
fn golden_hashes_are_stable() {
    assert_eq!(
        blob_id(Domain::Source, b"vesper").to_string(),
        "b3v1:source:blob:f9d86c28af87cdf6ceec016efdfb89ed94391ba12041d48c887075289f94aabe"
    );
    assert_eq!(
        blob_id(Domain::Interface, b"vesper").to_string(),
        "b3v1:interface:blob:b3f96a3ec1be1e0fdb7389b92cbc8afe5f34f490aaaa4d22bf56bc26588c031b"
    );
    assert_eq!(
        blob_id(Domain::Artifact, b"vesper").to_string(),
        "b3v1:artifact:blob:db1f99cf072c68667d55e4f23ef619ff770f7796769c41562298291aa8c46ac0"
    );
    let mut tree = Tree::new(Domain::Source);
    tree.insert_file(
        "main.vsp",
        blob_id(Domain::Source, b"fn main() {}\n"),
        13,
        false,
    )
    .unwrap();
    tree.insert_file("run.sh", blob_id(Domain::Source, b"#!/bin/sh\n"), 10, true)
        .unwrap();
    assert_eq!(
        tree.id().to_string(),
        "b3v1:source:tree:2482ff30792f107f7e939990c1ab45197a413626d177cb96d4b38e5612d146d5"
    );
    let key = DerivationInputs::new(Domain::Artifact, "vesper-codegen-clif", "0.1.0")
        .with_target("x86_64-unknown-linux-gnu")
        .with_option("opt-level", "2")
        .with_input("source", tree.id())
        .key()
        .unwrap();
    assert_eq!(
        key.to_string(),
        "drv:b3v1:3e90a8c83313fd544b0f07549bd8af11c2ec9d85f8aa26971a846dcabe11948c"
    );
}

#[test]
fn domains_and_kinds_are_separated() {
    let ids: Vec<_> = Domain::ALL.iter().map(|d| blob_id(*d, b"same")).collect();
    assert_ne!(ids[0].digest, ids[1].digest);
    assert_ne!(ids[1].digest, ids[2].digest);
    assert_ne!(ids[0].digest, ids[2].digest);

    // A blob whose bytes equal a tree encoding does not share the tree's id.
    let tree = Tree::new(Domain::Source);
    assert_ne!(
        blob_id(Domain::Source, &tree.encode()).digest,
        tree.id().digest
    );

    let (_d, store) = store();
    let a = store.put_blob(Domain::Source, b"same").unwrap();
    let b = store.put_blob(Domain::Artifact, b"same").unwrap();
    assert!(a.newly_stored && b.newly_stored);
    assert_ne!(store.object_path(&a.id), store.object_path(&b.id));
}

#[test]
fn object_id_text_form_is_strict() {
    let id = blob_id(Domain::Interface, b"x");
    assert_eq!(id.to_string().parse::<ObjectId>().unwrap(), id);
    let text = id.to_string();
    for bad in [
        text.to_uppercase(),
        text.replace("b3v1", "b3v2"),
        text.replace("interface", "../../etc"),
        format!("{text}:extra"),
        text[..text.len() - 1].to_owned(),
        "b3v1:source:blob:../../../../../../etc/passwd".to_owned(),
    ] {
        assert!(bad.parse::<ObjectId>().is_err(), "{bad}");
    }
}

// ---------------------------------------------------------------------------
// Ingestion and readback
// ---------------------------------------------------------------------------

#[test]
fn round_trip_dedupe_and_readonly_objects() {
    let (_d, store) = store();
    let first = store.put_blob(Domain::Source, b"hello").unwrap();
    assert!(first.newly_stored);
    assert_eq!(first.size, 5);
    assert_eq!(first.id, blob_id(Domain::Source, b"hello"));
    let second = store
        .put_blob_reader(Domain::Source, &b"hello"[..])
        .unwrap();
    assert_eq!(second.id, first.id);
    assert!(!second.newly_stored);
    assert_eq!(store.read_blob(&first.id).unwrap(), b"hello");
    assert!(
        fs::metadata(store.object_path(&first.id))
            .unwrap()
            .permissions()
            .readonly()
    );
    assert_eq!(tmp_entries(&store), 0);

    let missing = blob_id(Domain::Source, b"absent");
    assert!(matches!(
        store.read_blob(&missing),
        Err(CasError::NotFound(id)) if id == missing
    ));
    assert!(matches!(
        store.read_tree(&first.id),
        Err(CasError::WrongKind { .. })
    ));
}

#[test]
fn expected_digest_mismatch_publishes_nothing() {
    let (_d, store) = store();
    let expected = blob_id(Domain::Source, b"locked contents");
    let err = store
        .put_blob_expected(expected, &b"tampered contents"[..])
        .unwrap_err();
    assert!(matches!(err, CasError::DigestMismatch { .. }));
    assert!(!store.contains(&expected).unwrap());
    assert!(
        !store
            .contains(&blob_id(Domain::Source, b"tampered contents"))
            .unwrap()
    );
    assert_eq!(tmp_entries(&store), 0);

    let ok = store
        .put_blob_expected(expected, &b"locked contents"[..])
        .unwrap();
    assert_eq!(ok.id, expected);
}

struct FailingReader {
    sent: usize,
    fail_after: usize,
    panic: bool,
}

impl Read for FailingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.sent >= self.fail_after {
            if self.panic {
                panic!("writer crashed mid-ingestion");
            }
            return Err(io::Error::new(io::ErrorKind::ConnectionReset, "cut off"));
        }
        let n = buf.len().min(self.fail_after - self.sent).min(4096);
        buf[..n].fill(0xab);
        self.sent += n;
        Ok(n)
    }
}

#[test]
fn interrupted_writes_leave_no_visible_object() {
    let (_d, store) = store();
    let err = store
        .put_blob_reader(
            Domain::Artifact,
            FailingReader {
                sent: 0,
                fail_after: 200_000,
                panic: false,
            },
        )
        .unwrap_err();
    assert!(matches!(err, CasError::Io { .. }));
    let prefix = vec![0xab; 200_000];
    assert!(!store.contains(&blob_id(Domain::Artifact, &prefix)).unwrap());
    assert_eq!(tmp_entries(&store), 0, "staging file removed on error");

    // A panicking writer unwinds through the staging guard as well.
    let s2 = store.clone();
    let result = std::panic::catch_unwind(move || {
        let _ = s2.put_blob_reader(
            Domain::Artifact,
            FailingReader {
                sent: 0,
                fail_after: 70_000,
                panic: true,
            },
        );
    });
    assert!(result.is_err());
    assert_eq!(tmp_entries(&store), 0);
    assert!(store.verify_all().unwrap().is_clean());
}

#[test]
fn crash_residue_is_invisible_and_sweepable() {
    let (dir, store) = store();
    // Simulate a writer killed after staging part of an object: the file
    // exists only under tmp/, never at an object path.
    let stale = store.root().join("tmp").join("stage-dead-0-0");
    fs::write(&stale, b"partial obj").unwrap();
    let reopened = Store::open(dir.path().join("store")).unwrap();
    assert!(
        !reopened
            .contains(&blob_id(Domain::Source, b"partial object"))
            .unwrap()
    );
    let report = reopened.verify_all().unwrap();
    assert!(report.is_clean());
    assert_eq!(report.verified_objects, 0);

    // Too young to sweep, then swept.
    assert_eq!(
        reopened
            .sweep_temporaries(Duration::from_secs(3600))
            .unwrap(),
        0
    );
    assert!(stale.exists());
    assert_eq!(reopened.sweep_temporaries(Duration::ZERO).unwrap(), 1);
    assert!(!stale.exists());
}

#[test]
fn corrupted_objects_are_rejected_reported_and_repaired() {
    let (_d, store) = store();
    let blob = store.put_blob(Domain::Source, b"precious bytes").unwrap();
    let path = store.object_path(&blob.id);
    corrupt_byte(&path, 3);

    assert!(matches!(
        store.read_blob(&blob.id),
        Err(CasError::Corrupt { id, .. }) if id == blob.id
    ));
    assert!(matches!(
        store.verify_object(&blob.id),
        Err(CasError::Corrupt { .. })
    ));
    let report = store.verify_all().unwrap();
    assert_eq!(report.corrupt_objects, vec![blob.id]);

    // Re-ingesting the true contents quarantines the bad copy and republishes.
    let again = store.put_blob(Domain::Source, b"precious bytes").unwrap();
    assert!(again.newly_stored);
    assert_eq!(store.read_blob(&blob.id).unwrap(), b"precious bytes");
    assert_eq!(
        fs::read_dir(store.root().join("quarantine"))
            .unwrap()
            .count(),
        1
    );
    assert!(store.verify_all().unwrap().is_clean());
}

#[test]
fn truncated_object_is_rejected() {
    let (_d, store) = store();
    let data = vec![7u8; 100_000];
    let blob = store.put_blob(Domain::Artifact, &data).unwrap();
    let path = store.object_path(&blob.id);
    make_writable(&path);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(50_000)
        .unwrap();
    assert!(matches!(
        store.read_blob(&blob.id),
        Err(CasError::Corrupt { .. })
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_objects_are_not_followed() {
    let (dir, store) = store();
    let blob = store.put_blob(Domain::Source, b"real").unwrap();
    let path = store.object_path(&blob.id);
    let decoy = dir.path().join("decoy");
    fs::write(&decoy, b"real").unwrap();
    make_writable(&path);
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&decoy, &path).unwrap();
    assert!(matches!(
        store.read_blob(&blob.id),
        Err(CasError::UnsafeStoreLayout { .. })
    ));
    assert_eq!(store.verify_all().unwrap().corrupt_objects, vec![blob.id]);
    // Re-ingest replaces the symlink with a real object.
    assert!(
        store
            .put_blob(Domain::Source, b"real")
            .unwrap()
            .newly_stored
    );
    assert!(fs::symlink_metadata(&path).unwrap().is_file());
}

#[test]
fn put_tree_requires_present_children_with_matching_sizes() {
    let (_d, store) = store();
    let mut tree = Tree::new(Domain::Source);
    let absent = blob_id(Domain::Source, b"not stored");
    tree.insert_file("a", absent, 10, false).unwrap();
    assert!(matches!(
        store.put_tree(&tree),
        Err(CasError::NotFound(id)) if id == absent
    ));

    let stored = store.put_blob(Domain::Source, b"12345").unwrap();
    let mut tree = Tree::new(Domain::Source);
    tree.insert_file("a", stored.id, 6, false).unwrap();
    assert!(matches!(
        store.put_tree(&tree),
        Err(CasError::SizeMismatch { .. })
    ));

    // Children from another domain are refused at construction.
    let artifact = store.put_blob(Domain::Artifact, b"x").unwrap();
    let mut tree = Tree::new(Domain::Source);
    assert!(matches!(
        tree.insert_file("a", artifact.id, 1, false),
        Err(TreeError::ChildMismatch { .. })
    ));
}

// ---------------------------------------------------------------------------
// Concurrency
// ---------------------------------------------------------------------------

#[test]
fn concurrent_duplicate_and_distinct_writers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let threads = 16;
    let barrier = Arc::new(Barrier::new(threads));
    let shared = vec![0x5au8; 300_000];
    let handles: Vec<_> = (0..threads)
        .map(|i| {
            let root = root.clone();
            let barrier = barrier.clone();
            let shared = shared.clone();
            thread::spawn(move || {
                // Each thread opens its own handle, as separate processes would.
                let store = Store::open(&root).unwrap();
                barrier.wait();
                let mut ids = Vec::new();
                for round in 0..8 {
                    ids.push(store.put_blob(Domain::Artifact, &shared).unwrap());
                    let own = format!("thread {i} round {round}");
                    ids.push(store.put_blob(Domain::Artifact, own.as_bytes()).unwrap());
                }
                ids
            })
        })
        .collect();
    let results: Vec<Vec<Ingested>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let store = Store::open(&root).unwrap();
    let shared_id = blob_id(Domain::Artifact, &shared);
    let mut newly = 0;
    for ingested in results.iter().flatten() {
        if ingested.id == shared_id {
            newly += ingested.newly_stored as usize;
        } else {
            assert!(ingested.newly_stored);
        }
    }
    assert_eq!(newly, 1, "exactly one writer publishes the shared object");
    assert_eq!(store.read_blob(&shared_id).unwrap(), shared);
    let report = store.verify_all().unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.verified_objects, 1 + (threads as u64) * 8);
    assert_eq!(tmp_entries(&store), 0);
}

fn derivation_fixture(store: &Store) -> (DerivationInputs, ObjectId, ObjectId) {
    let src = store
        .put_blob(Domain::Source, b"fn f() -> i64 { 1 }")
        .unwrap();
    let iface = store.put_blob(Domain::Interface, b"fn f() -> i64").unwrap();
    let inputs = DerivationInputs::new(Domain::Artifact, "vesper-codegen-clif", "0.1.0")
        .with_target("x86_64-unknown-linux-gnu")
        .with_option("abi", "AbiLayoutV1")
        .with_input("interface", iface.id)
        .with_input("source", src.id);
    let a = store.put_blob(Domain::Artifact, b"object code A").unwrap();
    let b = store.put_blob(Domain::Artifact, b"object code B").unwrap();
    (inputs, a.id, b.id)
}

#[test]
fn concurrent_derivation_publication_never_replaces() {
    let (dir, store) = store();
    let (inputs, out_a, out_b) = derivation_fixture(&store);
    let root = store.root().to_owned();
    let threads = 12;
    let barrier = Arc::new(Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|i| {
            let root = root.clone();
            let barrier = barrier.clone();
            let inputs = inputs.clone();
            thread::spawn(move || {
                let store = Store::open(&root).unwrap();
                barrier.wait();
                let output = if i % 2 == 0 { out_a } else { out_b };
                store.publish_derivation(&inputs, output)
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winner = store.lookup_derivation(&inputs).unwrap().unwrap();
    let mut recorded = 0;
    for r in results {
        match r {
            Ok(outcome) => {
                assert_eq!(outcome.output, winner);
                recorded += outcome.newly_recorded as usize;
            }
            Err(CasError::DerivationConflict {
                existing, proposed, ..
            }) => {
                assert_eq!(existing, winner);
                assert_ne!(proposed, winner);
            }
            Err(e) => panic!("unexpected error {e}"),
        }
    }
    assert_eq!(recorded, 1);
    assert!(store.verify_all().unwrap().is_clean());
    drop(dir);
}

// ---------------------------------------------------------------------------
// Separate processes (the test binary re-executes itself)
// ---------------------------------------------------------------------------

const CHILD_ROOT: &str = "VESPER_CAS_CHILD_ROOT";
const CHILD_MODE: &str = "VESPER_CAS_CHILD_MODE";

/// Child entry point; a no-op unless spawned by the tests below.
#[test]
fn child_process_entry() {
    let (Some(root), Some(mode)) = (std::env::var_os(CHILD_ROOT), std::env::var(CHILD_MODE).ok())
    else {
        return;
    };
    let store = Store::open(PathBuf::from(root)).unwrap();
    match mode.as_str() {
        "write" => {
            for round in 0..20 {
                store
                    .put_blob(Domain::Artifact, &vec![0x33; 250_000])
                    .unwrap();
                store
                    .put_blob(
                        Domain::Artifact,
                        format!("pid {} round {round}", std::process::id()).as_bytes(),
                    )
                    .unwrap();
            }
        }
        "hang" => {
            // Stream forever; the parent kills this process mid-write.
            struct Endless;
            impl Read for Endless {
                fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                    thread::sleep(Duration::from_millis(5));
                    let n = buf.len().min(1024);
                    buf[..n].fill(0x77);
                    Ok(n)
                }
            }
            let _ = store.put_blob_reader(Domain::Artifact, Endless);
            unreachable!("endless reader returned");
        }
        other => panic!("unknown child mode {other}"),
    }
}

fn spawn_child(root: &Path, mode: &str) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_process_entry",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ROOT, root)
        .env(CHILD_MODE, mode)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap()
}

#[test]
fn concurrent_writer_processes() {
    let (_d, store) = store();
    let children: Vec<_> = (0..6).map(|_| spawn_child(store.root(), "write")).collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let shared = blob_id(Domain::Artifact, &vec![0x33; 250_000]);
    assert_eq!(store.read_blob(&shared).unwrap().len(), 250_000);
    let report = store.verify_all().unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.verified_objects, 1 + 6 * 20);
    assert_eq!(tmp_entries(&store), 0);
}

#[test]
fn killed_writer_process_leaves_only_staging_residue() {
    let (_d, store) = store();
    let before = store.verify_all().unwrap();
    let mut child = spawn_child(store.root(), "hang");
    // Wait until the child has started staging, then kill it mid-write.
    let staging = store.root().join("tmp");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let size: u64 = fs::read_dir(&staging)
            .unwrap()
            .filter_map(|e| e.ok()?.metadata().ok())
            .map(|m| m.len())
            .sum();
        if size > 0 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "child never staged");
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    // Nothing became addressable; the partial file is only in tmp/.
    let after = store.verify_all().unwrap();
    assert!(after.is_clean(), "{after:?}");
    assert_eq!(after.verified_objects, before.verified_objects);
    assert_eq!(tmp_entries(&store), 1);
    // The store remains fully usable, and the residue can be swept.
    store.put_blob(Domain::Artifact, b"after crash").unwrap();
    assert_eq!(store.sweep_temporaries(Duration::ZERO).unwrap(), 1);
    assert_eq!(tmp_entries(&store), 0);
}

// ---------------------------------------------------------------------------
// Derivations
// ---------------------------------------------------------------------------

#[test]
fn derivation_records_validate_inputs_and_outputs() {
    let (_d, store) = store();
    let (inputs, out_a, _) = derivation_fixture(&store);
    assert_eq!(store.lookup_derivation(&inputs).unwrap(), None);

    // Output must be in the declared domain.
    let iface = *inputs.inputs.get("interface").unwrap();
    assert!(matches!(
        store.publish_derivation(&inputs, iface),
        Err(CasError::DomainMismatch { .. })
    ));
    // Every input must be stored.
    let missing = inputs
        .clone()
        .with_input("extra", blob_id(Domain::Artifact, b"never stored"));
    assert!(matches!(
        store.publish_derivation(&missing, out_a),
        Err(CasError::NotFound(_))
    ));
    // Outputs must be stored too.
    assert!(matches!(
        store.publish_derivation(&inputs, blob_id(Domain::Artifact, b"ghost")),
        Err(CasError::NotFound(_))
    ));

    let outcome = store.publish_derivation(&inputs, out_a).unwrap();
    assert!(outcome.newly_recorded);
    assert_eq!(outcome.key, inputs.key().unwrap());
    assert!(
        !store
            .publish_derivation(&inputs, out_a)
            .unwrap()
            .newly_recorded
    );
    assert_eq!(store.lookup_derivation(&inputs).unwrap(), Some(out_a));
}

#[test]
fn body_change_with_same_interface_misses_the_cache() {
    let (_d, store) = store();
    let (inputs, out_a, _) = derivation_fixture(&store);
    store.publish_derivation(&inputs, out_a).unwrap();

    let fixed_body = store
        .put_blob(Domain::Source, b"fn f() -> i64 { 2 }")
        .unwrap();
    let same_interface = inputs.clone().with_input("source", fixed_body.id);
    assert_eq!(store.lookup_derivation(&same_interface).unwrap(), None);

    let other_target = inputs.clone().with_target("aarch64-unknown-linux-gnu");
    assert_eq!(store.lookup_derivation(&other_target).unwrap(), None);
}

#[test]
fn corrupt_or_dangling_derivation_records_are_errors() {
    let (_d, store) = store();
    let (inputs, out_a, out_b) = derivation_fixture(&store);
    let key = store.publish_derivation(&inputs, out_a).unwrap().key;
    let hex = key.0.to_hex();
    let record = store
        .root()
        .join("derivations/v1")
        .join(&hex[..2])
        .join(&hex);

    // Flip a byte inside the stored inputs.
    let len = fs::metadata(&record).unwrap().len() as usize;
    corrupt_byte(&record, len - 1);
    assert!(matches!(
        store.lookup_derivation(&inputs),
        Err(CasError::CorruptDerivationRecord { .. })
    ));
    assert_eq!(store.verify_all().unwrap().corrupt_derivations.len(), 1);
    // A corrupt record is quarantined on republish, never treated as a conflict.
    assert!(
        store
            .publish_derivation(&inputs, out_b)
            .unwrap()
            .newly_recorded
    );
    assert_eq!(store.lookup_derivation(&inputs).unwrap(), Some(out_b));

    // A record whose output disappeared is reported, not silently missed.
    let out_path = store.object_path(&out_b);
    make_writable(&out_path);
    fs::remove_file(&out_path).unwrap();
    assert!(matches!(
        store.lookup_derivation(&inputs),
        Err(CasError::NotFound(id)) if id == out_b
    ));
}

// ---------------------------------------------------------------------------
// Directory ingestion and materialization
// ---------------------------------------------------------------------------

fn sample_tree(base: &Path) -> PathBuf {
    let src = base.join("pkg");
    fs::create_dir_all(src.join("src/nested")).unwrap();
    fs::create_dir_all(src.join("empty")).unwrap();
    fs::write(src.join("Loom.toml"), b"[package]\nname = \"@x/y\"\n").unwrap();
    fs::write(src.join("src/main.vsp"), b"fn main() -> i64 { 0 }\n").unwrap();
    fs::write(src.join("src/nested/blank"), b"").unwrap();
    fs::write(src.join("run.sh"), b"#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(src.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    src
}

fn assert_same_tree(a: &Path, b: &Path) {
    let mut ea: Vec<_> = fs::read_dir(a)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let mut eb: Vec<_> = fs::read_dir(b)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    ea.sort();
    eb.sort();
    assert_eq!(ea, eb);
    for name in ea {
        let (pa, pb) = (a.join(&name), b.join(&name));
        let (ma, mb) = (
            fs::symlink_metadata(&pa).unwrap(),
            fs::symlink_metadata(&pb).unwrap(),
        );
        assert_eq!(ma.is_dir(), mb.is_dir());
        if ma.is_dir() {
            assert_same_tree(&pa, &pb);
        } else {
            assert!(mb.is_file());
            assert_eq!(fs::read(&pa).unwrap(), fs::read(&pb).unwrap());
        }
    }
}

#[test]
fn ingest_and_materialize_round_trip() {
    let (dir, store) = store();
    let src = sample_tree(dir.path());
    let root = store.ingest_dir(Domain::Source, &src).unwrap();
    assert_eq!(root.id.kind, ObjectKind::Tree);
    // Ingestion is deterministic.
    let again = store.ingest_dir(Domain::Source, &src).unwrap();
    assert_eq!(again.id, root.id);
    assert!(!again.newly_stored);

    let tree = store.read_tree(&root.id).unwrap();
    let names: Vec<_> = tree.entries().map(|(n, _)| n.to_owned()).collect();
    assert_eq!(names, ["Loom.toml", "empty", "run.sh", "src"]);
    #[cfg(unix)]
    assert_eq!(
        tree.get("run.sh").unwrap().kind,
        EntryKind::File { executable: true }
    );

    for strategy in [CopyStrategy::ReflinkOrCopy, CopyStrategy::CopyOnly] {
        let dest = dir.path().join(format!("ws-{strategy:?}"));
        let report = store
            .materialize(
                &root.id,
                &dest,
                MaterializeOptions {
                    strategy,
                    verify_reflinks: true,
                },
            )
            .unwrap();
        assert_eq!(report.files, 4);
        assert_eq!(report.directories, 4);
        assert_eq!(report.reflinked + report.copied, 4);
        if strategy == CopyStrategy::CopyOnly {
            assert_eq!(report.reflinked, 0);
        } else if std::env::var_os("VESPER_CAS_EXPECT_REFLINK").is_some() {
            assert_eq!(report.reflinked, 4, "{report:?}");
        }
        assert_same_tree(&src, &dest);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let exec = fs::metadata(dest.join("run.sh")).unwrap();
            assert_eq!(exec.permissions().mode() & 0o777, 0o755);
            let plain = fs::metadata(dest.join("Loom.toml")).unwrap();
            assert_eq!(plain.permissions().mode() & 0o777, 0o644);
            // Workspace files are independent of store objects.
            assert_eq!(plain.nlink(), 1);
            let obj = tree.child_id(tree.get("Loom.toml").unwrap());
            let obj_meta = fs::metadata(store.object_path(&obj)).unwrap();
            assert_ne!(obj_meta.ino(), plain.ino());
        }
        // Editing the workspace cannot change the store.
        fs::write(dest.join("Loom.toml"), b"edited").unwrap();
        assert!(store.verify_all().unwrap().is_clean());
        assert!(staging_leftovers(dir.path()).is_empty());
    }
}

#[test]
fn reflink_only_either_clones_or_fails_cleanly() {
    let (dir, store) = store();
    let src = sample_tree(dir.path());
    let root = store.ingest_dir(Domain::Source, &src).unwrap();
    let dest = dir.path().join("ws");
    let options = MaterializeOptions {
        strategy: CopyStrategy::ReflinkOnly,
        verify_reflinks: true,
    };
    match store.materialize(&root.id, &dest, options) {
        Ok(report) => {
            assert_eq!(report.reflinked, 4);
            assert_same_tree(&src, &dest);
        }
        Err(CasError::ReflinkUnavailable { .. }) => {
            // This file system cannot clone: no partial workspace appears.
            assert!(
                std::env::var_os("VESPER_CAS_EXPECT_REFLINK").is_none(),
                "VESPER_CAS_EXPECT_REFLINK is set but cloning failed"
            );
            assert!(!dest.exists());
        }
        Err(e) => panic!("unexpected error {e}"),
    }
    assert!(staging_leftovers(dir.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn materialize_accepts_a_maximum_length_destination_name() {
    let (dir, store) = store();
    let root = store.put_tree(&Tree::new(Domain::Source)).unwrap();
    let dest = dir.path().join("a".repeat(255));
    store
        .materialize(&root.id, &dest, MaterializeOptions::default())
        .unwrap();
    assert!(dest.is_dir());
    assert!(staging_leftovers(dir.path()).is_empty());
}

#[test]
fn materialize_refuses_existing_destinations() {
    let (dir, store) = store();
    let src = sample_tree(dir.path());
    let root = store.ingest_dir(Domain::Source, &src).unwrap();

    let existing_dir = dir.path().join("exists");
    fs::create_dir(&existing_dir).unwrap();
    let existing_file = dir.path().join("file");
    fs::write(&existing_file, b"keep me").unwrap();
    let targets = [
        existing_dir.clone(),
        existing_file.clone(),
        #[cfg(unix)]
        {
            let dangling = dir.path().join("dangling");
            std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).unwrap();
            dangling
        },
    ];
    for target in targets {
        assert!(matches!(
            store.materialize(&root.id, &target, MaterializeOptions::default()),
            Err(CasError::DestinationExists(_))
        ));
    }
    assert_eq!(fs::read(&existing_file).unwrap(), b"keep me");
    assert!(fs::read_dir(&existing_dir).unwrap().next().is_none());
    assert!(!dir.path().join("nowhere").exists());
    assert!(matches!(
        store.materialize(
            &root.id,
            dir.path().join(".."),
            MaterializeOptions::default()
        ),
        Err(CasError::InvalidName { .. })
    ));
}

#[test]
fn materialize_rejects_corruption_without_partial_workspace() {
    let (dir, store) = store();
    let src = sample_tree(dir.path());
    let root = store.ingest_dir(Domain::Source, &src).unwrap();
    let main = blob_id(Domain::Source, b"fn main() -> i64 { 0 }\n");
    corrupt_byte(&store.object_path(&main), 0);
    for strategy in [CopyStrategy::ReflinkOrCopy, CopyStrategy::CopyOnly] {
        let dest = dir.path().join("ws");
        let err = store
            .materialize(
                &root.id,
                &dest,
                MaterializeOptions {
                    strategy,
                    verify_reflinks: true,
                },
            )
            .unwrap_err();
        assert!(
            matches!(err, CasError::Corrupt { id, .. } if id == main),
            "{err}"
        );
        assert!(!dest.exists());
        assert!(staging_leftovers(dir.path()).is_empty());
    }
}

/// Plant a tree object whose encoding was crafted outside `Tree` (as a
/// malicious substituter might serve it) and try to materialize it.
fn plant_raw_tree(store: &Store, encoding: &[u8]) -> ObjectId {
    let mut hasher = ObjectHasher::new(Domain::Source, ObjectKind::Tree);
    hasher.update(encoding);
    let id = hasher.finish();
    let path = store.object_path(&id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, encoding).unwrap();
    id
}

fn raw_tree(entries: &[(&[u8], u8, Digest, u64)]) -> Vec<u8> {
    let mut out = b"VLTR".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (name, kind, digest, size) in entries {
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
        out.push(*kind);
        out.extend_from_slice(&digest.0);
        out.extend_from_slice(&size.to_le_bytes());
    }
    out
}

#[test]
fn adversarial_tree_names_cannot_escape_the_workspace() {
    let (dir, store) = store();
    let payload = store.put_blob(Domain::Source, b"pwned").unwrap();
    let ws_parent = dir.path().join("ws-parent");
    fs::create_dir(&ws_parent).unwrap();
    let names: &[&[u8]] = &[
        b"..",
        b".",
        b"../escape",
        b"/abs",
        b"a/../../escape",
        b"..\\escape",
        b"C:escape",
        b"nul\0byte",
        b"",
        b"\xff\xfe",
        b"CON",
    ];
    for name in names {
        let id = plant_raw_tree(
            &store,
            &raw_tree(&[(name, 1, payload.id.digest, payload.size)]),
        );
        let dest = ws_parent.join("ws");
        let err = store
            .materialize(&id, &dest, MaterializeOptions::default())
            .unwrap_err();
        assert!(
            matches!(err, CasError::InvalidTree(TreeError::InvalidName { .. })),
            "{name:?}: {err}"
        );
        assert!(!dest.exists());
    }
    // Unknown entry kinds (e.g. a would-be symlink tag) are rejected too.
    let id = plant_raw_tree(&store, &raw_tree(&[(b"link", 4, payload.id.digest, 5)]));
    assert!(matches!(
        store.materialize(&id, ws_parent.join("ws"), MaterializeOptions::default()),
        Err(CasError::InvalidTree(TreeError::UnknownEntryKind(4)))
    ));
    // Duplicate names (which would overwrite) and case collisions.
    let dup = raw_tree(&[
        (b"a", 1, payload.id.digest, 5),
        (b"a", 1, payload.id.digest, 5),
    ]);
    let id = plant_raw_tree(&store, &dup);
    assert!(matches!(
        store.materialize(&id, ws_parent.join("ws"), MaterializeOptions::default()),
        Err(CasError::InvalidTree(TreeError::NotSorted { .. }))
    ));
    let case = raw_tree(&[
        (b"A", 1, payload.id.digest, 5),
        (b"a", 1, payload.id.digest, 5),
    ]);
    let id = plant_raw_tree(&store, &case);
    assert!(matches!(
        store.materialize(&id, ws_parent.join("ws"), MaterializeOptions::default()),
        Err(CasError::InvalidTree(TreeError::CaseCollision { .. }))
    ));
    // Size metadata that lies about the blob.
    let id = plant_raw_tree(&store, &raw_tree(&[(b"f", 1, payload.id.digest, 999)]));
    assert!(matches!(
        store.materialize(&id, ws_parent.join("ws"), MaterializeOptions::default()),
        Err(CasError::SizeMismatch { .. })
    ));

    assert!(!dir.path().join("escape").exists());
    assert!(!Path::new("/abs").exists());
    assert_eq!(fs::read_dir(&ws_parent).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn ingest_rejects_symlinks_and_special_files() {
    use std::os::unix::fs::symlink;
    let (dir, store) = store();
    let outside = dir.path().join("secret");
    fs::write(&outside, b"secret").unwrap();

    type Maker = Box<dyn Fn(&Path)>;
    let cases: Vec<(&str, Maker)> = vec![
        (
            "file symlink",
            Box::new(|p: &Path| symlink("/etc/passwd", p.join("link")).unwrap()),
        ),
        (
            "dir symlink",
            Box::new(|p: &Path| symlink("/", p.join("root")).unwrap()),
        ),
        (
            "dangling symlink",
            Box::new(|p: &Path| symlink(p.join("missing"), p.join("dangling")).unwrap()),
        ),
        (
            "fifo",
            Box::new(|p: &Path| {
                let c = std::ffi::CString::new(p.join("fifo").to_str().unwrap()).unwrap();
                // SAFETY: valid NUL-terminated path.
                assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
            }),
        ),
    ];
    for (label, make) in cases {
        let src = dir.path().join(label.replace(' ', "-"));
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("ok.txt"), b"ok").unwrap();
        make(&src.join("sub"));
        let err = store.ingest_dir(Domain::Source, &src).unwrap_err();
        assert!(
            matches!(err, CasError::UnsupportedFileType { .. }),
            "{label}: {err}"
        );
    }
    // The root itself may not be a symlink either.
    let real = sample_tree(dir.path());
    let link = dir.path().join("pkg-link");
    symlink(&real, &link).unwrap();
    assert!(matches!(
        store.ingest_dir(Domain::Source, &link),
        Err(CasError::UnsupportedFileType {
            kind: "symlink",
            ..
        })
    ));
    assert!(
        store
            .put_file(Domain::Source, link.join("Loom.toml"))
            .is_ok()
    );
    let file_link = dir.path().join("file-link");
    symlink(&outside, &file_link).unwrap();
    assert!(matches!(
        store.put_file(Domain::Source, &file_link),
        Err(CasError::UnsupportedFileType {
            kind: "symlink",
            ..
        })
    ));
}

#[test]
fn ingest_rejects_nonportable_names() {
    let (dir, store) = store();
    let mut bad = vec!["a:b", "trailing.", "CON", "x*y"];
    if cfg!(windows) {
        bad.clear();
    }
    for name in bad {
        let src = dir.path().join(format!("pkg-{}", name.len()));
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join(name), b"x").unwrap();
        assert!(
            matches!(
                store.ingest_dir(Domain::Source, &src),
                Err(CasError::InvalidName { .. })
            ),
            "{name}"
        );
    }
    // Case collisions are detectable only on a case-sensitive host.
    let src = dir.path().join("case");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("Readme"), b"1").unwrap();
    fs::write(src.join("README"), b"2").unwrap();
    if fs::read_dir(&src).unwrap().count() == 2 {
        assert!(matches!(
            store.ingest_dir(Domain::Source, &src),
            Err(CasError::InvalidTree(TreeError::CaseCollision { .. }))
        ));
    }
}

#[cfg(unix)]
#[test]
fn hardlinked_inputs_are_copied_not_linked() {
    use std::os::unix::fs::MetadataExt;
    let (dir, store) = store();
    let src = dir.path().join("pkg");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("a"), b"shared inode").unwrap();
    fs::hard_link(src.join("a"), src.join("b")).unwrap();
    let root = store.ingest_dir(Domain::Source, &src).unwrap();
    let blob = blob_id(Domain::Source, b"shared inode");
    let obj = fs::metadata(store.object_path(&blob)).unwrap();
    assert_eq!(obj.nlink(), 1, "the store never links to inputs");
    assert_ne!(obj.ino(), fs::metadata(src.join("a")).unwrap().ino());

    let dest = dir.path().join("ws");
    store
        .materialize(&root.id, &dest, MaterializeOptions::default())
        .unwrap();
    let a = fs::metadata(dest.join("a")).unwrap();
    let b = fs::metadata(dest.join("b")).unwrap();
    assert_eq!(a.nlink(), 1);
    assert_ne!(a.ino(), b.ino(), "materialized entries are independent");
    // Mutating the input after ingestion does not affect the store.
    fs::write(src.join("a"), b"changed").unwrap();
    assert_eq!(store.read_blob(&blob).unwrap(), b"shared inode");
}

#[test]
fn ingest_enforces_depth_limit() {
    let (dir, store) = store();
    let mut path = dir.path().join("deep");
    for _ in 0..=MAX_TREE_DEPTH {
        path.push("d");
    }
    fs::create_dir_all(&path).unwrap();
    assert!(matches!(
        store.ingest_dir(Domain::Source, dir.path().join("deep")),
        Err(CasError::InvalidTree(TreeError::TooDeep { .. }))
    ));
}

// ---------------------------------------------------------------------------
// Store layout
// ---------------------------------------------------------------------------

#[test]
fn unknown_layout_marker_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    Store::open(&root).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("LAYOUT")).unwrap(),
        "vesper-loom-cas layout 1\n"
    );
    make_writable(&root.join("LAYOUT"));
    fs::write(root.join("LAYOUT"), b"vesper-loom-cas layout 2\n").unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(CasError::UnsafeStoreLayout { .. })
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_store_internals_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, root.join("tmp")).unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(CasError::UnsafeStoreLayout { .. })
    ));
}

#[test]
fn stray_files_are_reported_not_trusted() {
    let (_d, store) = store();
    store.put_blob(Domain::Source, b"x").unwrap();
    let shard = store.root().join("objects/v1/source/blob/zz");
    fs::create_dir_all(&shard).unwrap();
    fs::write(shard.join("not-a-digest"), b"x").unwrap();
    let report = store.verify_all().unwrap();
    assert_eq!(report.verified_objects, 1);
    assert_eq!(report.stray_paths.len(), 1);
    assert!(!report.is_clean());
}

/// Many writers find the same corrupt record and race to repair it with
/// different outputs. Exactly one repair may win; a stale corruption check
/// must never quarantine the winner's fresh record.
#[test]
fn concurrent_repair_of_a_corrupt_record_has_one_winner() {
    for _ in 0..20 {
        let (_d, store) = store();
        let (inputs, out_a, out_b) = derivation_fixture(&store);
        let out_c = store
            .put_blob(Domain::Artifact, b"object code C")
            .unwrap()
            .id;
        let key = store.publish_derivation(&inputs, out_c).unwrap().key;
        let hex = key.0.to_hex();
        let record = store
            .root()
            .join("derivations/v1")
            .join(&hex[..2])
            .join(&hex);
        let len = fs::metadata(&record).unwrap().len() as usize;
        corrupt_byte(&record, len - 1);

        let threads = 8;
        let barrier = Arc::new(Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|i| {
                let store = store.clone();
                let barrier = barrier.clone();
                let inputs = inputs.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let output = if i % 2 == 0 { out_a } else { out_b };
                    store.publish_derivation(&inputs, output)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let winner = store.lookup_derivation(&inputs).unwrap().unwrap();
        let mut newly = 0;
        for r in results {
            match r {
                Ok(o) => {
                    assert_eq!(o.output, winner, "a reported success was replaced");
                    newly += o.newly_recorded as usize;
                }
                Err(CasError::DerivationConflict { existing, .. }) => {
                    assert_eq!(existing, winner)
                }
                Err(e) => panic!("unexpected error {e}"),
            }
        }
        assert_eq!(newly, 1);
        assert_eq!(
            fs::read_dir(store.root().join("quarantine"))
                .unwrap()
                .count(),
            1,
            "only the corrupt record is quarantined"
        );
    }
}

#[test]
fn concurrent_repair_of_a_corrupt_object_quarantines_once() {
    for _ in 0..20 {
        let (_d, store) = store();
        let data = vec![0x42u8; 100_000];
        let id = store.put_blob(Domain::Source, &data).unwrap().id;
        corrupt_byte(&store.object_path(&id), 10);
        let threads = 8;
        let barrier = Arc::new(Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let store = store.clone();
                let barrier = barrier.clone();
                let data = data.clone();
                thread::spawn(move || {
                    barrier.wait();
                    store.put_blob(Domain::Source, &data).unwrap()
                })
            })
            .collect();
        let newly: usize = handles
            .into_iter()
            .map(|h| h.join().unwrap().newly_stored as usize)
            .sum();
        assert_eq!(newly, 1);
        assert_eq!(store.read_blob(&id).unwrap(), data);
        assert_eq!(
            fs::read_dir(store.root().join("quarantine"))
                .unwrap()
                .count(),
            1
        );
    }
}
