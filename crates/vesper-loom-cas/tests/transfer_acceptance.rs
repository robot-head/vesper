//! Independent transfer wire oracles and actual filesystem/process acceptance.
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use vesper_loom_cas::{
    CopyStrategy, DerivationInputs, Digest, Domain, MaterializeOptions, ObjectId, ObjectKind,
    Store, Tree,
    transfer::{TransferError, TransferLimits, export_pack, import_pack},
};

fn domain_tag(domain: Domain) -> u8 {
    match domain {
        Domain::Source => 1,
        Domain::Interface => 2,
        Domain::Artifact => 3,
    }
}

fn kind_tag(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::Blob => 1,
        ObjectKind::Tree => 2,
    }
}

// Independent frozen-context hashing, without production ObjectHasher/blob_id.
fn identity(domain: Domain, kind: ObjectKind, payload: &[u8]) -> ObjectId {
    let domain_name = match domain {
        Domain::Source => "source",
        Domain::Interface => "interface",
        Domain::Artifact => "artifact",
    };
    let kind_name = match kind {
        ObjectKind::Blob => "blob",
        ObjectKind::Tree => "tree",
    };
    let context = format!("vesper-loom-cas 2026-11 v1 {domain_name} {kind_name}");
    ObjectId::new(domain, kind, Digest(blake3::derive_key(&context, payload)))
}

fn wire_id(out: &mut Vec<u8>, id: ObjectId) {
    out.extend([domain_tag(id.domain), kind_tag(id.kind)]);
    out.extend(id.digest.0);
}

fn wire_pack(root: ObjectId, records: &[(ObjectId, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = b"VSPACK01".to_vec();
    bytes.extend(1_u32.to_le_bytes());
    bytes.extend(1_u32.to_le_bytes());
    wire_id(&mut bytes, root);
    bytes.extend((records.len() as u32).to_le_bytes());
    assert_eq!(bytes.len(), 54);
    for (id, payload) in records {
        wire_id(&mut bytes, *id);
        bytes.extend((payload.len() as u64).to_le_bytes());
        bytes.extend(payload);
    }
    bytes
}

// Caller supplies explicit wire tags/order, including intentionally invalid
// names and structures. This never invokes Tree::encode or its insertion policy.
fn wire_tree(entries: &[(&[u8], u8, Digest, u64)]) -> Vec<u8> {
    let mut bytes = b"VLTR".to_vec();
    bytes.extend(1_u32.to_le_bytes());
    bytes.extend((entries.len() as u64).to_le_bytes());
    for (name, kind, digest, size) in entries {
        bytes.extend((name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(name);
        bytes.push(*kind);
        bytes.extend(digest.0);
        bytes.extend(size.to_le_bytes());
    }
    bytes
}

fn record(domain: Domain, kind: ObjectKind, bytes: Vec<u8>) -> (ObjectId, Vec<u8>) {
    (identity(domain, kind, &bytes), bytes)
}

struct Fixture {
    root: ObjectId,
    records: Vec<(ObjectId, Vec<u8>)>,
    blob: ObjectId,
    executable: ObjectId,
}

fn fixture(store: &Store, domain: Domain) -> Fixture {
    let plain = b"shared\0bytes".to_vec();
    let executable = b"\x7fELFopaque-not-executed\n{\"certificate\":\"not-a-proof\"}".to_vec();
    let blob = store.put_blob(domain, &plain).unwrap().id;
    let executable_id = store.put_blob(domain, &executable).unwrap().id;
    assert_eq!(blob, identity(domain, ObjectKind::Blob, &plain));
    assert_eq!(
        executable_id,
        identity(domain, ObjectKind::Blob, &executable)
    );
    let mut leaf = Tree::new(domain);
    // Intentionally insert in reverse name order; wire order is canonical.
    leaf.insert_file("run.bin", executable_id, executable.len() as u64, true)
        .unwrap();
    leaf.insert_file("plain.txt", blob, plain.len() as u64, false)
        .unwrap();
    let leaf_bytes = wire_tree(&[
        (b"plain.txt", 1, blob.digest, plain.len() as u64),
        (b"run.bin", 2, executable_id.digest, executable.len() as u64),
    ]);
    assert_eq!(leaf.encode(), leaf_bytes);
    let leaf_id = store.put_tree(&leaf).unwrap().id;
    assert_eq!(leaf_id, identity(domain, ObjectKind::Tree, &leaf_bytes));
    let mut root = Tree::new(domain);
    root.insert_dir("z", leaf_id).unwrap();
    root.insert_file("middle.txt", blob, plain.len() as u64, false)
        .unwrap();
    root.insert_dir("a", leaf_id).unwrap();
    let root_bytes = wire_tree(&[
        (b"a", 3, leaf_id.digest, 0),
        (b"middle.txt", 1, blob.digest, plain.len() as u64),
        (b"z", 3, leaf_id.digest, 0),
    ]);
    assert_eq!(root.encode(), root_bytes);
    let root_id = store.put_tree(&root).unwrap().id;
    assert_eq!(root_id, identity(domain, ObjectKind::Tree, &root_bytes));
    Fixture {
        root: root_id,
        records: vec![
            (blob, plain),
            (executable_id, executable),
            (leaf_id, leaf_bytes),
            (root_id, root_bytes),
        ],
        blob,
        executable: executable_id,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FileState {
    path: PathBuf,
    kind: u8,
    mode: u32,
    bytes: Vec<u8>,
}

fn snapshot(root: &Path) -> Vec<FileState> {
    fn walk(root: &Path, path: &Path, result: &mut Vec<FileState>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode()
        };
        #[cfg(not(unix))]
        let mode = u32::from(metadata.permissions().readonly());
        let (kind, bytes) = if metadata.file_type().is_symlink() {
            (
                3,
                fs::read_link(path)
                    .unwrap()
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec(),
            )
        } else if metadata.is_dir() {
            (2, vec![])
        } else {
            (1, fs::read(path).unwrap())
        };
        result.push(FileState {
            path: path.strip_prefix(root).unwrap().to_path_buf(),
            kind,
            mode,
            bytes,
        });
        if metadata.is_dir() {
            let mut entries: Vec<_> = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            entries.sort();
            for entry in entries {
                walk(root, &entry, result);
            }
        }
    }
    let mut result = vec![];
    walk(root, root, &mut result);
    result
}

fn seeded_destination(path: &Path) -> Store {
    let store = Store::open(path).unwrap();
    let source = store.put_blob(Domain::Source, b"keep source").unwrap().id;
    let output = store
        .put_blob(Domain::Artifact, b"keep artifact")
        .unwrap()
        .id;
    let derivation = DerivationInputs::new(Domain::Artifact, "acceptance-builder", "1")
        .with_input("source", source);
    store.publish_derivation(&derivation, output).unwrap();
    store
}

fn rejected_without_destination_effects(expected: ObjectId, bytes: &[u8], limits: TransferLimits) {
    let scratch = tempfile::tempdir().unwrap();
    let absent_parent = scratch.path().join("must-stay-absent");
    let absent = absent_parent.join("store");
    assert!(import_pack(&absent, expected, bytes, limits).is_err());
    assert!(
        !absent_parent.exists(),
        "invalid import created destination parents"
    );
    let existing = seeded_destination(&scratch.path().join("existing"));
    let before = snapshot(existing.root());
    assert!(import_pack(existing.root(), expected, bytes, limits).is_err());
    assert_eq!(
        snapshot(existing.root()),
        before,
        "invalid import changed existing data"
    );
}

fn materialize_and_check(store: &Store, fixture: &Fixture, path: &Path) {
    let report = store
        .materialize(
            &fixture.root,
            path,
            MaterializeOptions {
                strategy: CopyStrategy::CopyOnly,
                verify_reflinks: true,
            },
        )
        .unwrap();
    assert_eq!(report.directories, 3);
    assert_eq!(report.files, 5);
    assert_eq!(
        fs::read(path.join("a/plain.txt")).unwrap(),
        fixture.records[0].1
    );
    assert_eq!(
        fs::read(path.join("z/run.bin")).unwrap(),
        fixture.records[1].1
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            fs::metadata(path.join("a/run.bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
        assert_eq!(
            fs::metadata(path.join("a/plain.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
    let mut permissions = fs::metadata(path.join("a/plain.txt"))
        .unwrap()
        .permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(path.join("a/plain.txt"), permissions).unwrap();
    fs::write(path.join("a/plain.txt"), b"independent workspace edit").unwrap();
    assert_eq!(
        store.read_blob(&fixture.blob).unwrap(),
        fixture.records[0].1
    );
    assert_eq!(
        fs::read(path.join("z/plain.txt")).unwrap(),
        fixture.records[0].1
    );
    assert_eq!(
        store.read_blob(&fixture.executable).unwrap(),
        fixture.records[1].1
    );
}

#[test]
fn golden_header_records_and_hashes_cover_all_domains_and_both_kinds() {
    let scratch = tempfile::tempdir().unwrap();
    let source = Store::open(scratch.path().join("source")).unwrap();
    assert_eq!(
        identity(Domain::Source, ObjectKind::Blob, b"vesper")
            .digest
            .to_string(),
        "f9d86c28af87cdf6ceec016efdfb89ed94391ba12041d48c887075289f94aabe"
    );
    let mut ids = Vec::new();
    for domain in Domain::ALL {
        for kind in ObjectKind::ALL {
            let payload = match kind {
                ObjectKind::Blob => b"vesper".to_vec(),
                ObjectKind::Tree => wire_tree(&[]),
            };
            let id = identity(domain, kind, &payload);
            let actual = match kind {
                ObjectKind::Blob => source.put_blob(domain, &payload).unwrap().id,
                ObjectKind::Tree => source.put_tree(&Tree::new(domain)).unwrap().id,
            };
            assert_eq!(actual, id);
            assert!(ids.iter().all(|prior: &ObjectId| prior.digest != id.digest));
            ids.push(id);
            let expected = wire_pack(id, &[(id, payload.clone())]);
            assert_eq!(
                export_pack(&source, id, TransferLimits::default()).unwrap(),
                expected
            );
            let dest =
                scratch
                    .path()
                    .join(format!("dest-{}-{}", domain_tag(domain), kind_tag(kind)));
            let report = import_pack(&dest, id, &expected, TransferLimits::default()).unwrap();
            assert_eq!(report.root, id);
            assert_eq!(report.objects, 1);
            assert_eq!(report.inserted, 1);
            assert_eq!(report.reused, 0);
            assert_eq!(report.pack_bytes, 54 + 42 + payload.len());
            assert_eq!(report.payload_bytes, payload.len());
            let dest = Store::open(dest).unwrap();
            assert_eq!(
                export_pack(&dest, id, TransferLimits::default()).unwrap(),
                expected
            );
            assert_eq!(dest.verify_object(&id).unwrap(), payload.len() as u64);
        }
    }
}

#[test]
fn sorted_postorder_deduplicates_shared_dag_and_roundtrips_opaque_artifacts() {
    let scratch = tempfile::tempdir().unwrap();
    let source = Store::open(scratch.path().join("source")).unwrap();
    for domain in Domain::ALL {
        let f = fixture(&source, domain);
        let expected = wire_pack(f.root, &f.records);
        assert_eq!(
            export_pack(&source, f.root, TransferLimits::default()).unwrap(),
            expected
        );
        let destination = scratch
            .path()
            .join(format!("destination-{}", domain_tag(domain)));
        let imported =
            import_pack(&destination, f.root, &expected, TransferLimits::default()).unwrap();
        assert_eq!(imported.objects, f.records.len());
        assert_eq!(
            imported.payload_bytes,
            f.records.iter().map(|(_, p)| p.len()).sum::<usize>()
        );
        assert_eq!(imported.inserted, f.records.len());
        assert_eq!(imported.reused, 0);
        let again =
            import_pack(&destination, f.root, &expected, TransferLimits::default()).unwrap();
        assert_eq!(again.inserted, 0);
        assert_eq!(again.reused, f.records.len());
        let destination = Store::open(&destination).unwrap();
        assert_eq!(
            export_pack(&destination, f.root, TransferLimits::default()).unwrap(),
            expected
        );
        assert_eq!(
            destination.verify_all().unwrap().verified_objects,
            f.records.len() as u64
        );
        materialize_and_check(
            &destination,
            &f,
            &scratch.path().join(format!("work-{}", domain_tag(domain))),
        );
    }
}

#[test]
fn every_truncation_hash_tag_version_length_and_expected_root_failure_is_side_effect_free() {
    let scratch = tempfile::tempdir().unwrap();
    let source = Store::open(scratch.path().join("source")).unwrap();
    let id = source
        .put_blob(Domain::Source, b"transfer payload")
        .unwrap()
        .id;
    let pack = wire_pack(id, &[(id, b"transfer payload".to_vec())]);
    for end in 0..pack.len() {
        rejected_without_destination_effects(id, &pack[..end], TransferLimits::default());
    }
    for offset in [0, 8, 12, 16, 17, 18, 50, 54, 55, 56, 88, 96] {
        let mut changed = pack.clone();
        changed[offset] ^= 0xff;
        rejected_without_destination_effects(id, &changed, TransferLimits::default());
    }
    let mut huge_length = pack.clone();
    huge_length[88..96].copy_from_slice(&u64::MAX.to_le_bytes());
    rejected_without_destination_effects(id, &huge_length, TransferLimits::default());
    let mut huge_count = pack.clone();
    huge_count[50..54].copy_from_slice(&u32::MAX.to_le_bytes());
    rejected_without_destination_effects(id, &huge_count, TransferLimits::default());
    let mut trailing = pack.clone();
    trailing.extend_from_slice(b"VDRVopaque-injected-derivation");
    rejected_without_destination_effects(id, &trailing, TransferLimits::default());
    let wrong = identity(Domain::Artifact, ObjectKind::Blob, b"transfer payload");
    assert!(matches!(
        import_pack(
            scratch.path().join("wrong"),
            wrong,
            &pack,
            TransferLimits::default()
        ),
        Err(TransferError::UnexpectedRoot { .. })
    ));
    rejected_without_destination_effects(wrong, &pack, TransferLimits::default());
}

#[test]
fn complete_valid_hashes_do_not_authorize_duplicate_extra_missing_or_reordered_records() {
    let scratch = tempfile::tempdir().unwrap();
    let source = Store::open(scratch.path().join("source")).unwrap();
    let f = fixture(&source, Domain::Source);
    let mut cases = Vec::new();
    let mut swapped = f.records.clone();
    swapped.swap(0, 1); // Still topological, but not canonical name-DFS order.
    cases.push(swapped);
    let mut duplicate = f.records.clone();
    duplicate.insert(1, duplicate[0].clone());
    cases.push(duplicate);
    let mut extra = f.records.clone();
    extra.insert(
        0,
        record(Domain::Source, ObjectKind::Blob, b"unreferenced".to_vec()),
    );
    cases.push(extra);
    let mut missing = f.records.clone();
    missing.remove(0);
    cases.push(missing);
    let mut root_first = f.records.clone();
    root_first.rotate_right(1);
    cases.push(root_first);
    let mut foreign = f.records.clone();
    foreign[0] = record(Domain::Interface, ObjectKind::Blob, foreign[0].1.clone());
    cases.push(foreign);
    let mut wrong_kind = f.records.clone();
    wrong_kind[0] = record(Domain::Source, ObjectKind::Tree, wrong_kind[0].1.clone());
    cases.push(wrong_kind);
    for records in cases {
        for (id, bytes) in &records {
            assert_eq!(identity(id.domain, id.kind, bytes), *id);
        }
        rejected_without_destination_effects(
            f.root,
            &wire_pack(f.root, &records),
            TransferLimits::default(),
        );
    }
}

#[test]
fn rehashed_malformed_trees_and_conflicting_shared_file_sizes_reject_before_publication() {
    let domain = Domain::Source;
    let blob = record(domain, ObjectKind::Blob, b"x".to_vec());
    let empty = record(domain, ObjectKind::Tree, wire_tree(&[]));
    let mut trees = vec![
        wire_tree(&[(b"../escape", 1, blob.0.digest, 1)]),
        wire_tree(&[(b"CON", 1, blob.0.digest, 1)]),
        wire_tree(&[(b"\xff", 1, blob.0.digest, 1)]),
        wire_tree(&[(b"A", 1, blob.0.digest, 1), (b"a", 1, blob.0.digest, 1)]),
        wire_tree(&[(b"z", 1, blob.0.digest, 1), (b"a", 1, blob.0.digest, 1)]),
        wire_tree(&[(b"a", 1, blob.0.digest, 1), (b"a", 1, blob.0.digest, 1)]),
        wire_tree(&[(b"bad", 9, blob.0.digest, 1)]),
        wire_tree(&[(b"directory", 3, empty.0.digest, 1)]),
        // Both edges refer to one completed blob. The second size must still be checked.
        wire_tree(&[(b"a", 1, blob.0.digest, 1), (b"z", 1, blob.0.digest, 2)]),
    ];
    let mut count = wire_tree(&[]);
    count[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    trees.push(count);
    let mut trailing = wire_tree(&[(b"ok", 1, blob.0.digest, 1)]);
    trailing.push(0);
    trees.push(trailing);
    for tree in trees {
        let root = record(domain, ObjectKind::Tree, tree);
        // Keep possible children present and all typed hashes valid, so digest
        // corruption cannot mask canonical-name/shape/edge-size validation.
        let children = if root.1.windows(9).any(|w| w == b"directory") {
            vec![empty.clone()]
        } else {
            vec![blob.clone()]
        };
        let mut records = children;
        records.push(root.clone());
        rejected_without_destination_effects(
            root.0,
            &wire_pack(root.0, &records),
            TransferLimits::default(),
        );
    }
}

#[test]
fn a_shared_tree_first_visited_shallow_cannot_bypass_a_later_deep_path_limit() {
    let scratch = tempfile::tempdir().unwrap();
    let source = Store::open(scratch.path().join("source")).unwrap();
    let blob = source.put_blob(Domain::Source, b"x").unwrap().id;
    let mut shared = Tree::new(Domain::Source);
    shared.insert_file("value", blob, 1, false).unwrap();
    let shared_id = source.put_tree(&shared).unwrap().id;
    let mut deep = Tree::new(Domain::Source);
    deep.insert_dir("child", shared_id).unwrap();
    let deep_id = source.put_tree(&deep).unwrap().id;
    let mut deeper = Tree::new(Domain::Source);
    deeper.insert_dir("child", deep_id).unwrap();
    let deeper_id = source.put_tree(&deeper).unwrap().id;
    let mut root = Tree::new(Domain::Source);
    root.insert_dir("a-shallow", shared_id).unwrap();
    root.insert_dir("z-deeper", deeper_id).unwrap();
    let root_id = source.put_tree(&root).unwrap().id;
    let pack = export_pack(&source, root_id, TransferLimits::default()).unwrap();
    let limits = TransferLimits {
        max_depth: 3,
        ..TransferLimits::default()
    };
    assert!(export_pack(&source, root_id, limits).is_err());
    rejected_without_destination_effects(root_id, &pack, limits);
}

#[test]
fn every_limit_and_later_publication_reservation_fails_before_store_creation_or_changes() {
    let scratch = tempfile::tempdir().unwrap();
    let source = Store::open(scratch.path().join("source")).unwrap();
    let f = fixture(&source, Domain::Source);
    let pack = export_pack(&source, f.root, TransferLimits::default()).unwrap();
    let cases = [
        TransferLimits {
            max_pack_bytes: pack.len() - 1,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_objects: f.records.len() - 1,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_depth: 1,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_tree_entries: 4,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_metadata_bytes: 1,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_work: 1,
            ..TransferLimits::default()
        },
    ];
    for limits in cases {
        assert!(export_pack(&source, f.root, limits).is_err());
        rejected_without_destination_effects(f.root, &pack, limits);
    }
    let mut failed = [0, 0];
    let mut succeeded = [0, 0];
    // Sweep through early validation and much larger publication envelopes.
    // All failures, whichever phase exhausts the cap, must precede Store::open.
    for index in 0..2 {
        for cap in [
            1, 64, 1024, 8192, 65536, 262144, 524288, 1048576, 2097152, 4194304, 8388608, 16777216,
        ] {
            let mut limits = TransferLimits::default();
            if index == 0 {
                limits.max_metadata_bytes = cap;
            } else {
                limits.max_work = cap;
            }
            let target = scratch.path().join(format!("sweep-{index}-{cap}"));
            match import_pack(&target, f.root, &pack, limits) {
                Ok(report) => {
                    succeeded[index] += 1;
                    assert_eq!(report.objects, f.records.len());
                    assert!(
                        Store::open(&target)
                            .unwrap()
                            .verify_all()
                            .unwrap()
                            .is_clean()
                    );
                }
                Err(_) => {
                    failed[index] += 1;
                    assert!(
                        !target.exists(),
                        "resource failure left partial destination: {index}/{cap}"
                    );
                    rejected_without_destination_effects(f.root, &pack, limits);
                }
            }
        }
    }
    assert!(failed.into_iter().all(|n| n > 0));
    assert!(succeeded.into_iter().all(|n| n > 0));
}

#[test]
fn invalid_settings_never_open_even_an_existing_destination() {
    let id = identity(Domain::Source, ObjectKind::Blob, b"x");
    let pack = wire_pack(id, &[(id, b"x".to_vec())]);
    let cases = [
        TransferLimits {
            max_pack_bytes: 0,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_objects: 0,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_depth: 0,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_tree_entries: 0,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_metadata_bytes: 0,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_work: 0,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_pack_bytes: 256 * 1024 * 1024 + 1,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_objects: 16385,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_depth: 129,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_tree_entries: 65537,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_metadata_bytes: 64 * 1024 * 1024 + 1,
            ..TransferLimits::default()
        },
        TransferLimits {
            max_work: 2 * 1024 * 1024 * 1024 + 1,
            ..TransferLimits::default()
        },
    ];
    for limits in cases {
        rejected_without_destination_effects(id, &pack, limits);
    }
}

fn make_writable(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn corruption_is_not_reused_and_import_repairs_using_verified_expected_bytes() {
    let scratch = tempfile::tempdir().unwrap();
    let source = Store::open(scratch.path().join("source")).unwrap();
    let f = fixture(&source, Domain::Artifact);
    let pack = export_pack(&source, f.root, TransferLimits::default()).unwrap();
    let destination_path = scratch.path().join("destination");
    import_pack(&destination_path, f.root, &pack, TransferLimits::default()).unwrap();
    let destination = Store::open(&destination_path).unwrap();
    let object = destination.object_path(&f.blob);
    make_writable(&object);
    let mut corrupt = f.records[0].1.clone();
    corrupt[0] ^= 1;
    fs::write(&object, corrupt).unwrap();
    assert!(destination.contains(&f.blob).unwrap());
    assert!(export_pack(&destination, f.root, TransferLimits::default()).is_err());
    let repaired =
        import_pack(&destination_path, f.root, &pack, TransferLimits::default()).unwrap();
    assert_eq!(repaired.inserted, 1);
    assert_eq!(repaired.reused, f.records.len() - 1);
    assert_eq!(destination.read_blob(&f.blob).unwrap(), f.records[0].1);
    assert_eq!(
        export_pack(&destination, f.root, TransferLimits::default()).unwrap(),
        pack
    );
    let quarantine = fs::read_dir(destination.root().join("quarantine"))
        .unwrap()
        .count();
    assert!(quarantine > 0);
}

#[test]
fn concurrent_imports_reuse_one_verified_closure_without_derivation_transfer() {
    let scratch = tempfile::tempdir().unwrap();
    let source = Store::open(scratch.path().join("source")).unwrap();
    let f = fixture(&source, Domain::Artifact);
    let derivation = DerivationInputs::new(Domain::Artifact, "opaque-builder", "1")
        .with_input("artifact", f.root);
    source.publish_derivation(&derivation, f.root).unwrap();
    let bytes = Arc::new(export_pack(&source, f.root, TransferLimits::default()).unwrap());
    let destination = scratch.path().join("concurrent");
    let barrier = Arc::new(Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let bytes = Arc::clone(&bytes);
            let barrier = Arc::clone(&barrier);
            let destination = destination.clone();
            let root = f.root;
            thread::spawn(move || {
                barrier.wait();
                import_pack(destination, root, &bytes, TransferLimits::default()).unwrap()
            })
        })
        .collect();
    let reports: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert!(
        reports
            .iter()
            .all(|r| r.inserted + r.reused == f.records.len())
    );
    assert_eq!(
        reports.iter().map(|r| r.inserted).sum::<usize>(),
        f.records.len()
    );
    let destination = Store::open(&destination).unwrap();
    let state = destination.verify_all().unwrap();
    assert!(state.is_clean(), "{state:?}");
    assert_eq!(state.verified_objects, f.records.len() as u64);
    assert_eq!(state.verified_derivations, 0);
    assert_eq!(destination.lookup_derivation(&derivation).unwrap(), None);
    assert_eq!(source.lookup_derivation(&derivation).unwrap(), Some(f.root));
}

const WORKER: &str = "VESPER_CASPACK_ACCEPTANCE_WORKER";
const ROOT: &str = "VESPER_CASPACK_ACCEPTANCE_ROOT";
const STORE: &str = "VESPER_CASPACK_ACCEPTANCE_STORE";
const PACK: &str = "VESPER_CASPACK_ACCEPTANCE_PACK";
const WORKSPACE: &str = "VESPER_CASPACK_ACCEPTANCE_WORKSPACE";

// This real test-binary entrypoint is invoked in a separate OS process below.
#[test]
fn transfer_process_entry() {
    let Some(mode) = std::env::var_os(WORKER) else {
        return;
    };
    let root: ObjectId = std::env::var(ROOT).unwrap().parse().unwrap();
    let path = PathBuf::from(std::env::var_os(STORE).unwrap());
    if mode == "export" {
        let store = Store::open(path).unwrap();
        let bytes = export_pack(&store, root, TransferLimits::default()).unwrap();
        fs::write(PathBuf::from(std::env::var_os(PACK).unwrap()), bytes).unwrap();
    } else {
        assert_eq!(mode, "import");
        let limit = TransferLimits::default().max_pack_bytes;
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(bytes.len() <= limit);
        let report = import_pack(&path, root, &bytes, TransferLimits::default()).unwrap();
        assert!(report.objects > 0);
        Store::open(path)
            .unwrap()
            .materialize(
                &root,
                PathBuf::from(std::env::var_os(WORKSPACE).unwrap()),
                MaterializeOptions {
                    strategy: CopyStrategy::CopyOnly,
                    verify_reflinks: true,
                },
            )
            .unwrap();
    }
}

fn process(mode: &str, store: &Path, root: ObjectId) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "transfer_process_entry",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(WORKER, mode)
        .env(ROOT, root.to_string())
        .env(STORE, store)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    command
}

fn wait(mut child: Child) {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "child process failed: {status}");
            return;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("transfer worker timed out");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn separate_process_export_file_and_pipe_import_materialize_actual_immutable_objects() {
    let scratch: TempDir = tempfile::tempdir().unwrap();
    let source = Store::open(scratch.path().join("source")).unwrap();
    let f = fixture(&source, Domain::Source);
    let pack_file = scratch.path().join("closure.pack");
    wait(
        process("export", source.root(), f.root)
            .env(PACK, &pack_file)
            .spawn()
            .unwrap(),
    );
    let bytes = fs::read(&pack_file).unwrap();
    assert_eq!(bytes, wire_pack(f.root, &f.records));
    let destination = scratch.path().join("destination");
    let workspace = scratch.path().join("workspace");
    let mut importer = process("import", &destination, f.root)
        .env(WORKSPACE, &workspace)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    importer.stdin.take().unwrap().write_all(&bytes).unwrap();
    wait(importer);
    let destination = Store::open(&destination).unwrap();
    assert_eq!(
        export_pack(&destination, f.root, TransferLimits::default()).unwrap(),
        bytes
    );
    assert_eq!(
        fs::read(workspace.join("a/plain.txt")).unwrap(),
        f.records[0].1
    );
    assert_eq!(
        fs::read(workspace.join("z/run.bin")).unwrap(),
        f.records[1].1
    );
}
