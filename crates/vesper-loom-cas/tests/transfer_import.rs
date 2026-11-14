use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
    thread,
};
use tempfile::TempDir;
use vesper_loom_cas::{
    DerivationInputs, Domain, EntryKind, MaterializeOptions, ObjectHasher, ObjectId, ObjectKind,
    Store, Tree, blob_id,
    transfer::{TransferError, TransferLimits, import_pack},
};

fn id_bytes(id: ObjectId, bytes: &mut Vec<u8>) {
    bytes.push(match id.domain {
        Domain::Source => 1,
        Domain::Interface => 2,
        Domain::Artifact => 3,
    });
    bytes.push(match id.kind {
        ObjectKind::Blob => 1,
        ObjectKind::Tree => 2,
    });
    bytes.extend_from_slice(id.digest.as_bytes());
}
fn pack(root: ObjectId, records: &[(ObjectId, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = b"VSPACK01".to_vec();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    id_bytes(root, &mut bytes);
    bytes.extend_from_slice(&(records.len() as u32).to_le_bytes());
    for (id, payload) in records {
        id_bytes(*id, &mut bytes);
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(payload);
    }
    bytes
}
fn hashed(domain: Domain, kind: ObjectKind, payload: &[u8]) -> ObjectId {
    let mut hash = ObjectHasher::new(domain, kind);
    hash.update(payload);
    hash.finish()
}
fn snapshot(path: &Path) -> Vec<(std::path::PathBuf, bool, bool, Vec<u8>)> {
    fn visit(root: &Path, at: &Path, out: &mut Vec<(std::path::PathBuf, bool, bool, Vec<u8>)>) {
        let mut entries: Vec<_> = fs::read_dir(at)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for entry in entries {
            let meta = fs::symlink_metadata(&entry).unwrap();
            assert!(!meta.file_type().is_symlink());
            if meta.is_dir() {
                out.push((
                    entry.strip_prefix(root).unwrap().to_owned(),
                    true,
                    meta.permissions().readonly(),
                    vec![],
                ));
                visit(root, &entry, out);
            } else {
                out.push((
                    entry.strip_prefix(root).unwrap().to_owned(),
                    false,
                    meta.permissions().readonly(),
                    fs::read(entry).unwrap(),
                ));
            }
        }
    }
    let mut out = vec![];
    visit(path, path, &mut out);
    out
}
fn reject_before_init(root: ObjectId, bytes: &[u8], limits: TransferLimits) -> TransferError {
    let tmp = TempDir::new().unwrap();
    let destination = tmp.path().join("absent").join("store");
    let error = import_pack(&destination, root, bytes, limits).unwrap_err();
    assert!(!tmp.path().join("absent").exists(), "{error}");
    error
}
fn closure(domain: Domain) -> (ObjectId, Vec<(ObjectId, Vec<u8>)>) {
    let payload = b"hello".to_vec();
    let blob = blob_id(domain, &payload);
    let mut child = Tree::new(domain);
    child.insert_file("file", blob, 5, true).unwrap();
    let mut root = Tree::new(domain);
    root.insert_dir("a", child.id()).unwrap();
    root.insert_dir("b", child.id()).unwrap();
    root.insert_file("loose", blob, 5, false).unwrap();
    (
        root.id(),
        vec![
            (blob, payload),
            (child.id(), child.encode()),
            (root.id(), root.encode()),
        ],
    )
}

#[test]
fn real_blob_roots_in_every_domain_report_insert_and_verified_reuse() {
    for domain in Domain::ALL {
        let tmp = TempDir::new().unwrap();
        let payload = b"opaque\0artifact\xff".to_vec();
        let root = blob_id(domain, &payload);
        let bytes = pack(root, &[(root, payload.clone())]);
        let destination = tmp.path().join("store");
        let first = import_pack(&destination, root, &bytes, TransferLimits::default()).unwrap();
        assert_eq!(
            (
                first.root,
                first.objects,
                first.pack_bytes,
                first.payload_bytes,
                first.inserted,
                first.reused
            ),
            (root, 1, bytes.len(), payload.len(), 1, 0)
        );
        let second = import_pack(&destination, root, &bytes, TransferLimits::default()).unwrap();
        assert_eq!((second.inserted, second.reused), (0, 1));
        let store = Store::open(&destination).unwrap();
        assert_eq!(store.read_blob(&root).unwrap(), payload);
        assert_eq!(
            fs::read_dir(destination.join("derivations/v1"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn canonical_shared_closure_materializes_real_files_and_executable_metadata() {
    for domain in Domain::ALL {
        let tmp = TempDir::new().unwrap();
        let (root, records) = closure(domain);
        let bytes = pack(root, &records);
        let report = import_pack(
            tmp.path().join("store"),
            root,
            &bytes,
            TransferLimits::default(),
        )
        .unwrap();
        assert_eq!((report.objects, report.inserted, report.reused), (3, 3, 0));
        assert_eq!(
            report.payload_bytes,
            records.iter().map(|r| r.1.len()).sum()
        );
        let store = Store::open(tmp.path().join("store")).unwrap();
        let child = store.read_tree(&records[1].0).unwrap();
        assert_eq!(
            child.get("file").unwrap().kind,
            EntryKind::File { executable: true }
        );
        let destination = tmp.path().join("workspace");
        store
            .materialize(&root, &destination, MaterializeOptions::default())
            .unwrap();
        for relative in ["a/file", "b/file", "loose"] {
            assert_eq!(fs::read(destination.join(relative)).unwrap(), b"hello");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(destination.join("a/file"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
            assert_eq!(
                fs::metadata(destination.join("loose"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
    }
}

#[test]
fn every_truncation_tamper_and_wrong_root_preserves_absent_or_existing_destination() {
    let (root, records) = closure(Domain::Source);
    let bytes = pack(root, &records);
    let tmp = TempDir::new().unwrap();
    let existing = Store::open(tmp.path().join("existing")).unwrap();
    let source = existing.put_blob(Domain::Source, b"source").unwrap();
    let output = existing.put_blob(Domain::Artifact, b"existing").unwrap();
    let inputs =
        DerivationInputs::new(Domain::Artifact, "compiler", "1").with_input("source", source.id);
    existing.publish_derivation(&inputs, output.id).unwrap();
    let before = snapshot(existing.root());
    for end in 0..bytes.len() {
        reject_before_init(root, &bytes[..end], TransferLimits::default());
        assert!(
            import_pack(
                existing.root(),
                root,
                &bytes[..end],
                TransferLimits::default()
            )
            .is_err()
        );
        assert_eq!(snapshot(existing.root()), before);
    }
    let mut damaged = bytes.clone();
    *damaged.last_mut().unwrap() ^= 1;
    assert!(matches!(
        reject_before_init(root, &damaged, TransferLimits::default()),
        TransferError::DigestMismatch { .. }
    ));
    let wrong = blob_id(Domain::Source, b"wrong");
    assert!(
        matches!(reject_before_init(wrong, &bytes, TransferLimits::default()), TransferError::UnexpectedRoot { expected, found } if expected == wrong && found == root)
    );
}

#[test]
fn envelope_versions_tags_counts_lengths_and_trailing_bytes_fail_before_init() {
    let payload = b"x".to_vec();
    let root = blob_id(Domain::Source, &payload);
    let original = pack(root, &[(root, payload)]);
    let mut cases = vec![];
    for (offset, byte) in [
        (0, b'X'),
        (8, 2),
        (12, 2),
        (16, 255),
        (17, 255),
        (54, 255),
        (55, 255),
    ] {
        let mut bytes = original.clone();
        bytes[offset] = byte;
        cases.push(bytes);
    }
    let mut zero = original.clone();
    zero[50..54].copy_from_slice(&0u32.to_le_bytes());
    cases.push(zero);
    let mut many = original.clone();
    many[50..54].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push(many);
    let mut large = original.clone();
    large[88..96].copy_from_slice(&u64::MAX.to_le_bytes());
    cases.push(large);
    let mut trailing = original;
    trailing.push(0);
    cases.push(trailing);
    for bytes in cases {
        reject_before_init(root, &bytes, TransferLimits::default());
    }
}

#[test]
fn known_domain_and_kind_tags_cannot_reinterpret_an_existing_digest() {
    let payload = b"opaque".to_vec();
    let root = blob_id(Domain::Source, &payload);
    for wrong in [
        ObjectId::new(Domain::Artifact, ObjectKind::Blob, root.digest),
        ObjectId::new(Domain::Source, ObjectKind::Tree, root.digest),
    ] {
        assert!(matches!(
            reject_before_init(root, &pack(root, &[(wrong, payload.clone())]), TransferLimits::default()),
            TransferError::DigestMismatch { expected, actual } if expected == wrong && actual != wrong
        ));
    }
}

#[test]
fn missing_duplicate_extra_and_reordered_valid_objects_are_not_alternate_encodings() {
    let a = (blob_id(Domain::Source, b"a"), b"a".to_vec());
    let b = (blob_id(Domain::Source, b"b"), b"b".to_vec());
    let extra = (blob_id(Domain::Source, b"extra"), b"extra".to_vec());
    let mut tree = Tree::new(Domain::Source);
    tree.insert_file("a", a.0, 1, false).unwrap();
    tree.insert_file("b", b.0, 1, false).unwrap();
    let root = tree.id();
    let r = (root, tree.encode());
    for records in [
        vec![a.clone(), r.clone()],
        vec![b.clone(), a.clone(), r.clone()],
        vec![a.clone(), b.clone(), extra, r.clone()],
        vec![a.clone(), b.clone(), a.clone(), r.clone()],
        vec![r, a.clone(), b.clone()],
        vec![a.clone(), b.clone()],
    ] {
        reject_before_init(root, &pack(root, &records), TransferLimits::default());
    }
    reject_before_init(a.0, &pack(a.0, &[b, a]), TransferLimits::default());
}

fn raw_tree(entries: &[(&[u8], u8, ObjectId, u64)]) -> Vec<u8> {
    let mut bytes = b"VLTR".to_vec();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (name, kind, child, size) in entries {
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(name);
        bytes.push(*kind);
        bytes.extend_from_slice(child.digest.as_bytes());
        bytes.extend_from_slice(&size.to_le_bytes());
    }
    bytes
}

#[test]
fn valid_hashes_do_not_authorize_malformed_or_nonportable_trees() {
    let child = blob_id(Domain::Source, b"x");
    let mut trees = vec![
        raw_tree(&[(b"..", 1, child, 1)]),
        raw_tree(&[(b"A", 1, child, 1), (b"a", 1, child, 1)]),
        raw_tree(&[(b"b", 1, child, 1), (b"a", 1, child, 1)]),
        raw_tree(&[(b"a", 1, child, 1), (b"a", 1, child, 1)]),
        raw_tree(&[(&[255], 1, child, 1)]),
        raw_tree(&[(b"dir", 3, child, 1)]),
        raw_tree(&[(b"a", 99, child, 1)]),
    ];
    let mut version = raw_tree(&[]);
    version[4] = 2;
    trees.push(version);
    let mut count = raw_tree(&[]);
    count[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    trees.push(count);
    let mut trailing = raw_tree(&[]);
    trailing.push(0);
    trees.push(trailing);
    for payload in trees {
        let root = hashed(Domain::Source, ObjectKind::Tree, &payload);
        let bytes = pack(root, &[(child, b"x".to_vec()), (root, payload)]);
        reject_before_init(root, &bytes, TransferLimits::default());
    }
}

#[test]
fn recorded_file_sizes_must_match_actual_verified_blob_bytes() {
    let child = blob_id(Domain::Source, b"x");
    let payload = raw_tree(&[(b"a", 1, child, 2)]);
    let root = hashed(Domain::Source, ObjectKind::Tree, &payload);
    assert!(matches!(
        reject_before_init(
            root,
            &pack(root, &[(child, b"x".to_vec()), (root, payload)]),
            TransferLimits::default()
        ),
        TransferError::InvalidPack("recorded file size")
    ));
}

#[test]
fn limits_and_publication_reservations_refuse_before_store_creation() {
    let (root, records) = closure(Domain::Source);
    let bytes = pack(root, &records);
    let defaults = TransferLimits::default();
    for limits in [
        TransferLimits {
            max_pack_bytes: bytes.len() - 1,
            ..defaults
        },
        TransferLimits {
            max_objects: 1,
            ..defaults
        },
        TransferLimits {
            max_depth: 1,
            ..defaults
        },
        TransferLimits {
            max_tree_entries: 1,
            ..defaults
        },
        TransferLimits {
            max_metadata_bytes: 1,
            ..defaults
        },
        TransferLimits {
            max_work: 1,
            ..defaults
        },
    ] {
        assert!(matches!(
            reject_before_init(root, &bytes, limits),
            TransferError::Limit(_)
        ));
    }
    for limits in [
        TransferLimits {
            max_pack_bytes: 0,
            ..defaults
        },
        TransferLimits {
            max_objects: 0,
            ..defaults
        },
        TransferLimits {
            max_depth: 0,
            ..defaults
        },
        TransferLimits {
            max_tree_entries: 0,
            ..defaults
        },
        TransferLimits {
            max_metadata_bytes: 0,
            ..defaults
        },
        TransferLimits {
            max_work: 0,
            ..defaults
        },
        TransferLimits {
            max_pack_bytes: 256 * 1024 * 1024 + 1,
            ..defaults
        },
        TransferLimits {
            max_objects: 16385,
            ..defaults
        },
        TransferLimits {
            max_depth: 129,
            ..defaults
        },
        TransferLimits {
            max_tree_entries: 65537,
            ..defaults
        },
        TransferLimits {
            max_metadata_bytes: 64 * 1024 * 1024 + 1,
            ..defaults
        },
        TransferLimits {
            max_work: 2 * 1024 * 1024 * 1024 + 1,
            ..defaults
        },
    ] {
        assert!(matches!(
            reject_before_init(root, &bytes, limits),
            TransferError::InvalidLimits(_)
        ));
    }
    let blob = blob_id(Domain::Source, b"x");
    let bytes = pack(blob, &[(blob, b"x".to_vec())]);
    // Record/hash/graph validation fit, but legacy publication buffers do not.
    for limits in [
        TransferLimits {
            max_metadata_bytes: 100_000,
            ..defaults
        },
        TransferLimits {
            max_work: 100_000,
            ..defaults
        },
    ] {
        assert!(matches!(
            reject_before_init(blob, &bytes, limits),
            TransferError::Limit(_)
        ));
    }
}

#[test]
fn cached_subtree_height_is_checked_on_a_deeper_shared_path() {
    let domain = Domain::Source;
    let blob = blob_id(domain, b"x");
    let mut shared = Tree::new(domain);
    shared.insert_file("f", blob, 1, false).unwrap();
    let mut middle = Tree::new(domain);
    middle.insert_dir("z", shared.id()).unwrap();
    let mut long = Tree::new(domain);
    long.insert_dir("z", middle.id()).unwrap();
    let mut root = Tree::new(domain);
    root.insert_dir("a", shared.id()).unwrap();
    root.insert_dir("b", long.id()).unwrap();
    let records = [
        (blob, b"x".to_vec()),
        (shared.id(), shared.encode()),
        (middle.id(), middle.encode()),
        (long.id(), long.encode()),
        (root.id(), root.encode()),
    ];
    let bytes = pack(root.id(), &records);
    assert!(matches!(
        reject_before_init(
            root.id(),
            &bytes,
            TransferLimits {
                max_depth: 4,
                ..Default::default()
            }
        ),
        TransferError::Limit("depth")
    ));
    let tmp = TempDir::new().unwrap();
    assert!(
        import_pack(
            tmp.path().join("store"),
            root.id(),
            &bytes,
            TransferLimits {
                max_depth: 5,
                ..Default::default()
            }
        )
        .is_ok()
    );
}

fn make_writable(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn corrupt_existing_objects_are_repaired_and_valid_ones_are_reused() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let payload = b"verified payload".to_vec();
    let id = store.put_blob(Domain::Artifact, &payload).unwrap().id;
    let path = store.object_path(&id);
    make_writable(&path);
    fs::write(
        &path,
        b"corrupt and longer than the expected immutable payload",
    )
    .unwrap();
    let bytes = pack(id, &[(id, payload.clone())]);
    let repaired = import_pack(store.root(), id, &bytes, TransferLimits::default()).unwrap();
    assert_eq!((repaired.inserted, repaired.reused), (1, 0));
    assert_eq!(store.read_blob(&id).unwrap(), payload);
    let reused = import_pack(store.root(), id, &bytes, TransferLimits::default()).unwrap();
    assert_eq!((reused.inserted, reused.reused), (0, 1));
    assert_eq!(
        fs::read_dir(store.root().join("quarantine"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn corrupted_same_size_children_are_rehashed_before_parent_publication() {
    let tmp = TempDir::new().unwrap();
    let (root, records) = closure(Domain::Source);
    let bytes = pack(root, &records);
    let destination = tmp.path().join("store");
    import_pack(&destination, root, &bytes, TransferLimits::default()).unwrap();
    let store = Store::open(&destination).unwrap();
    for id in [records[0].0, root] {
        let path = store.object_path(&id);
        make_writable(&path);
        let mut corrupt = fs::read(&path).unwrap();
        *corrupt.last_mut().unwrap() ^= 1;
        fs::write(&path, corrupt).unwrap();
    }
    let report = import_pack(&destination, root, &bytes, TransferLimits::default()).unwrap();
    assert_eq!((report.inserted, report.reused), (2, 1));
    assert_eq!(store.read_blob(&records[0].0).unwrap(), b"hello");
    assert_eq!(store.read_tree(&root).unwrap().len(), 3);
    let workspace = tmp.path().join("workspace");
    store
        .materialize(&root, &workspace, MaterializeOptions::default())
        .unwrap();
    assert_eq!(fs::read(workspace.join("a/file")).unwrap(), b"hello");
}

#[test]
fn concurrent_imports_only_report_actual_unique_immutable_publications() {
    let tmp = TempDir::new().unwrap();
    let destination = Arc::new(tmp.path().join("store"));
    let (root, records) = closure(Domain::Source);
    let bytes = Arc::new(pack(root, &records));
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let destination = destination.clone();
            let bytes = bytes.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                import_pack(&*destination, root, &bytes, TransferLimits::default()).unwrap()
            })
        })
        .collect();
    let reports: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(
        reports.iter().map(|r| r.inserted).sum::<usize>(),
        records.len()
    );
    assert!(
        reports
            .iter()
            .all(|r| r.objects == 3 && r.inserted + r.reused == 3)
    );
    let store = Store::open(&*destination).unwrap();
    assert_eq!(store.read_tree(&root).unwrap().len(), 3);
}

#[test]
fn store_io_failure_may_leave_verified_children_but_not_a_claimed_import() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let blob = blob_id(Domain::Source, b"x");
    let mut tree = Tree::new(Domain::Source);
    tree.insert_file("f", blob, 1, false).unwrap();
    let root = tree.id();
    let tree_path = store.object_path(&root);
    let shard = tree_path.parent().unwrap();
    fs::create_dir_all(shard.parent().unwrap()).unwrap();
    fs::write(shard, b"not a directory").unwrap();
    let error = import_pack(
        store.root(),
        root,
        &pack(root, &[(blob, b"x".to_vec()), (root, tree.encode())]),
        TransferLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(error, TransferError::Cas(_)));
    assert_eq!(store.read_blob(&blob).unwrap(), b"x");
    assert!(!tree_path.exists());
}
