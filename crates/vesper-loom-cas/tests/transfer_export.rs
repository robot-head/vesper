use std::fs;

use tempfile::TempDir;
use vesper_loom_cas::{
    Digest, Domain, ObjectHasher, ObjectId, ObjectKind, Store, Tree,
    transfer::{TransferError, TransferLimits, export_pack},
};

fn store() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("cas")).unwrap();
    (dir, store)
}

fn id_bytes(id: ObjectId) -> Vec<u8> {
    let mut out = vec![
        match id.domain {
            Domain::Source => 1,
            Domain::Interface => 2,
            Domain::Artifact => 3,
        },
        match id.kind {
            ObjectKind::Blob => 1,
            ObjectKind::Tree => 2,
        },
    ];
    out.extend_from_slice(&id.digest.0);
    out
}

fn records(pack: &[u8]) -> Vec<(ObjectId, &[u8])> {
    assert_eq!(&pack[..8], b"VSPACK01");
    assert_eq!(&pack[8..16], &[1, 0, 0, 0, 1, 0, 0, 0]);
    let count = u32::from_le_bytes(pack[50..54].try_into().unwrap()) as usize;
    let mut pos = 54;
    let mut out = Vec::new();
    for _ in 0..count {
        let id = ObjectId::new(
            match pack[pos] {
                1 => Domain::Source,
                2 => Domain::Interface,
                3 => Domain::Artifact,
                _ => panic!("unknown domain"),
            },
            match pack[pos + 1] {
                1 => ObjectKind::Blob,
                2 => ObjectKind::Tree,
                _ => panic!("unknown kind"),
            },
            Digest(pack[pos + 2..pos + 34].try_into().unwrap()),
        );
        let length = u64::from_le_bytes(pack[pos + 34..pos + 42].try_into().unwrap()) as usize;
        pos += 42;
        out.push((id, &pack[pos..pos + length]));
        pos += length;
    }
    assert_eq!(pos, pack.len());
    out
}

fn put_raw_tree(store: &Store, bytes: &[u8]) -> ObjectId {
    let mut hasher = ObjectHasher::new(Domain::Source, ObjectKind::Tree);
    hasher.update(bytes);
    let id = hasher.finish();
    let path = store.object_path(&id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    id
}

fn writable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "non-Unix fixture clears the Windows readonly attribute; Unix uses mode 0600"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
    }
}

#[test]
fn single_blob_matches_independent_wire_construction_in_every_domain() {
    let (_dir, store) = store();
    for domain in Domain::ALL {
        let payload = b"\0\xffopaque .olean\n";
        let id = store.put_blob(domain, payload).unwrap().id;
        let mut golden = b"VSPACK01".to_vec();
        golden.extend_from_slice(&1u32.to_le_bytes());
        golden.extend_from_slice(&1u32.to_le_bytes());
        golden.extend(id_bytes(id));
        golden.extend_from_slice(&1u32.to_le_bytes());
        golden.extend(id_bytes(id));
        golden.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        golden.extend_from_slice(payload);
        assert_eq!(
            export_pack(&store, id, TransferLimits::default()).unwrap(),
            golden
        );
    }
}

#[test]
fn canonical_postorder_deduplicates_shared_subtrees_and_retains_flags() {
    let (_dir, store) = store();
    let a = store.put_blob(Domain::Artifact, b"a").unwrap().id;
    let z = store.put_blob(Domain::Artifact, b"z").unwrap().id;
    let mut shared = Tree::new(Domain::Artifact);
    shared.insert_file("run", a, 1, true).unwrap();
    let shared_id = store.put_tree(&shared).unwrap().id;
    let mut tree = Tree::new(Domain::Artifact);
    tree.insert_file("z-last", z, 1, false).unwrap();
    tree.insert_dir("b-again", shared_id).unwrap();
    tree.insert_dir("a-first", shared_id).unwrap();
    let root = store.put_tree(&tree).unwrap().id;
    let first = export_pack(&store, root, TransferLimits::default()).unwrap();
    assert_eq!(
        first,
        export_pack(&store, root, TransferLimits::default()).unwrap()
    );
    let values = records(&first);
    assert_eq!(
        values.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![a, shared_id, z, root]
    );
    assert_eq!(values[1].1, shared.encode());
    assert_eq!(values[3].1, tree.encode());
}

#[test]
fn empty_trees_and_empty_blobs_have_real_records() {
    let (_dir, store) = store();
    for domain in Domain::ALL {
        let tree = Tree::new(domain);
        let root = store.put_tree(&tree).unwrap().id;
        let pack = export_pack(&store, root, TransferLimits::default()).unwrap();
        assert_eq!(records(&pack), vec![(root, tree.encode().as_slice())]);
        let root = store.put_blob(domain, b"").unwrap().id;
        let pack = export_pack(&store, root, TransferLimits::default()).unwrap();
        assert_eq!(pack.len(), 96);
        assert_eq!(records(&pack), vec![(root, b"".as_slice())]);
    }
}

#[test]
fn corrupted_source_is_rejected_without_repair_or_staging_writes() {
    let (_dir, store) = store();
    let id = store.put_blob(Domain::Source, b"correct").unwrap().id;
    let path = store.object_path(&id);
    writable(&path);
    fs::write(&path, b"corrupt").unwrap();
    assert!(matches!(
        export_pack(&store, id, TransferLimits::default()),
        Err(TransferError::DigestMismatch { .. })
    ));
    assert_eq!(fs::read(path).unwrap(), b"corrupt");
    assert_eq!(fs::read_dir(store.root().join("tmp")).unwrap().count(), 0);
    assert_eq!(
        fs::read_dir(store.root().join("quarantine"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn valid_digest_cannot_hide_malformed_tree_or_wrong_child_size() {
    let (_dir, store) = store();
    let id = put_raw_tree(&store, b"invalid tree bytes");
    assert!(export_pack(&store, id, TransferLimits::default()).is_err());
    let blob = store.put_blob(Domain::Source, b"abc").unwrap().id;
    let mut tree = Tree::new(Domain::Source);
    tree.insert_file("file", blob, 4, false).unwrap();
    let id = put_raw_tree(&store, &tree.encode());
    assert!(matches!(
        export_pack(&store, id, TransferLimits::default()),
        Err(TransferError::InvalidPack("tree file size mismatch"))
    ));
}

#[test]
fn source_symlinks_and_missing_children_are_not_treated_as_present() {
    let (dir, store) = store();
    let blob = store.put_blob(Domain::Source, b"abc").unwrap().id;
    let mut tree = Tree::new(Domain::Source);
    tree.insert_file("file", blob, 3, false).unwrap();
    let root = store.put_tree(&tree).unwrap().id;
    writable(&store.object_path(&blob));
    fs::remove_file(store.object_path(&blob)).unwrap();
    assert!(export_pack(&store, root, TransferLimits::default()).is_err());
    #[cfg(unix)]
    {
        let outside = dir.path().join("outside");
        fs::write(&outside, b"abc").unwrap();
        std::os::unix::fs::symlink(outside, store.object_path(&blob)).unwrap();
        assert!(export_pack(&store, root, TransferLimits::default()).is_err());
    }
    #[cfg(not(unix))]
    drop(dir);
}

#[test]
fn every_transfer_limit_has_a_real_export_refusal() {
    let (_dir, store) = store();
    let blob = store.put_blob(Domain::Source, b"abcd").unwrap().id;
    let defaults = TransferLimits::default();
    let exact = export_pack(&store, blob, defaults).unwrap();
    assert_eq!(
        export_pack(
            &store,
            blob,
            TransferLimits {
                max_pack_bytes: exact.len(),
                ..defaults
            }
        )
        .unwrap(),
        exact
    );
    assert!(
        export_pack(
            &store,
            blob,
            TransferLimits {
                max_pack_bytes: exact.len() - 1,
                ..defaults
            }
        )
        .is_err()
    );
    assert!(matches!(
        export_pack(
            &store,
            blob,
            TransferLimits {
                max_metadata_bytes: 1,
                ..defaults
            }
        ),
        Err(TransferError::Limit("metadata bytes"))
    ));
    assert!(matches!(
        export_pack(
            &store,
            blob,
            TransferLimits {
                max_work: 1,
                ..defaults
            }
        ),
        Err(TransferError::Limit("work"))
    ));
    assert!(matches!(
        export_pack(
            &store,
            blob,
            TransferLimits {
                max_depth: 0,
                ..defaults
            }
        ),
        Err(TransferError::InvalidLimits("depth"))
    ));
    let mut tree = Tree::new(Domain::Source);
    tree.insert_file("a", blob, 4, false).unwrap();
    tree.insert_file("b", blob, 4, false).unwrap();
    let root = store.put_tree(&tree).unwrap().id;
    writable(&store.object_path(&blob));
    fs::remove_file(store.object_path(&blob)).unwrap();
    // These limits must fire before attempting to open the deliberately absent child.
    assert!(matches!(
        export_pack(
            &store,
            root,
            TransferLimits {
                max_objects: 1,
                ..defaults
            }
        ),
        Err(TransferError::Limit("objects"))
    ));
    assert!(matches!(
        export_pack(
            &store,
            root,
            TransferLimits {
                max_depth: 1,
                ..defaults
            }
        ),
        Err(TransferError::Limit("depth"))
    ));
    assert!(matches!(
        export_pack(
            &store,
            root,
            TransferLimits {
                max_tree_entries: 1,
                ..defaults
            }
        ),
        Err(TransferError::Limit("tree entries"))
    ));
}

#[test]
fn a_deeper_later_shared_path_still_pays_the_full_subtree_depth() {
    let (_dir, store) = store();
    let blob = store.put_blob(Domain::Source, b"abc").unwrap().id;
    let mut tail = Tree::new(Domain::Source);
    tail.insert_file("data", blob, 3, false).unwrap();
    let tail = store.put_tree(&tail).unwrap().id;
    let mut shared = Tree::new(Domain::Source);
    shared.insert_dir("tail", tail).unwrap();
    let shared = store.put_tree(&shared).unwrap().id;
    let mut wrapper = Tree::new(Domain::Source);
    wrapper.insert_dir("shared", shared).unwrap();
    let wrapper = store.put_tree(&wrapper).unwrap().id;
    let mut root = Tree::new(Domain::Source);
    root.insert_dir("a-shallow", shared).unwrap();
    root.insert_dir("z-deep", wrapper).unwrap();
    let root = store.put_tree(&root).unwrap().id;
    assert!(matches!(
        export_pack(
            &store,
            root,
            TransferLimits {
                max_depth: 4,
                ..TransferLimits::default()
            }
        ),
        Err(TransferError::Limit("depth"))
    ));
    assert_eq!(
        records(
            &export_pack(
                &store,
                root,
                TransferLimits {
                    max_depth: 5,
                    ..TransferLimits::default()
                }
            )
            .unwrap()
        )
        .len(),
        5
    );
}

#[test]
fn occupied_publication_repairs_large_sparse_short_and_same_size_corruption() {
    let (_dir, store) = store();
    let payload = b"verified payload";
    let id = store.put_blob(Domain::Artifact, payload).unwrap().id;
    let path = store.object_path(&id);
    for length in [512 * 1024 * 1024, 1, payload.len() as u64] {
        writable(&path);
        let file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(length).unwrap();
        drop(file);
        assert!(
            store
                .put_blob_expected(id, payload.as_slice())
                .unwrap()
                .newly_stored
        );
        assert_eq!(store.read_blob(&id).unwrap(), payload);
        assert!(
            !store
                .put_blob_expected(id, payload.as_slice())
                .unwrap()
                .newly_stored
        );
    }
    assert_eq!(
        fs::read_dir(store.root().join("quarantine"))
            .unwrap()
            .count(),
        3
    );
}

#[test]
fn oversized_sparse_source_fails_at_metadata_before_hash_or_payload_reserve() {
    let (_dir, store) = store();
    let id = store.put_blob(Domain::Source, b"tiny").unwrap().id;
    let path = store.object_path(&id);
    writable(&path);
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(512 * 1024 * 1024).unwrap();
    drop(file);
    assert!(matches!(
        export_pack(
            &store,
            id,
            TransferLimits {
                max_pack_bytes: 1024,
                ..TransferLimits::default()
            }
        ),
        Err(TransferError::Limit("object bytes"))
    ));
    assert_eq!(fs::metadata(path).unwrap().len(), 512 * 1024 * 1024);
    assert_eq!(
        fs::read_dir(store.root().join("quarantine"))
            .unwrap()
            .count(),
        0
    );
}
