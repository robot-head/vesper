//! Durable incremental checkpoints in the loom CAS: round trips, incremental
//! reuse, tamper/missing/substitution rejection and CDC offsets.
use vesper_flux::*;
use vesper_loom_cas::{Domain, ObjectId, Store};

fn l() -> Limits {
    Limits::default()
}
fn cp() -> CheckpointLimits {
    CheckpointLimits::default()
}
fn set(width: usize, rows: &[(&[i64], i64)]) -> ZSet {
    let rows: Vec<_> = rows.iter().map(|(r, w)| (r.to_vec(), *w)).collect();
    ZSet::consolidate(width, &rows, l()).unwrap()
}
/// orders(cust, amount) ⋈ customers(cust, region), filtered and projected.
fn circuit() -> Circuit {
    let mut b = CircuitBuilder::new(l()).unwrap();
    let orders = b.input(2).unwrap();
    let customers = b.input(2).unwrap();
    let big = b
        .filter(
            orders,
            Predicate::AtLeast {
                column: 1,
                value: 10,
            },
        )
        .unwrap();
    let j = b.equijoin(big, customers, 0, 0).unwrap();
    let out = b
        .map(
            j,
            vec![Field::Column(0), Field::Column(3), Field::Column(1)],
        )
        .unwrap();
    b.finish(out).unwrap()
}
fn ticks() -> Vec<TickInput> {
    vec![
        TickInput {
            deltas: vec![
                set(2, &[(&[1, 15], 1), (&[2, 5], 1)]),
                set(2, &[(&[1, 7], 1), (&[2, 8], 1)]),
            ],
        },
        TickInput {
            deltas: vec![set(2, &[(&[2, 20], 2)]), set(2, &[])],
        },
        TickInput {
            deltas: vec![set(2, &[(&[1, 15], -1), (&[3, 30], 1)]), set(2, &[])],
        },
        TickInput {
            deltas: vec![set(2, &[]), set(2, &[(&[3, 9], 1), (&[2, 8], -1)])],
        },
    ]
}
fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("cas")).unwrap();
    (dir, store)
}
fn overwrite(store: &Store, id: &ObjectId, bytes: &[u8]) {
    let path = store.object_path(id);
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&path, perms).unwrap();
    std::fs::write(&path, bytes).unwrap();
}

#[test]
fn restore_then_continue_equals_uninterrupted_at_every_split() {
    let all = ticks();
    let mut reference = circuit().executor();
    let expected: Vec<ZSet> = all.iter().map(|t| reference.tick(t).unwrap()).collect();
    for split in 0..=all.len() {
        let (_dir, store) = store();
        let mut first = circuit().executor();
        for t in &all[..split] {
            first.tick(t).unwrap();
        }
        let report = DurableCheckpointer::new()
            .checkpoint(&first, &store, cp())
            .unwrap();
        assert_eq!(report.ticks, split as u64);
        drop(first); // crash: only the store survives
        let mut resumed = circuit().executor();
        let mut ckpt = DurableCheckpointer::new();
        let restored = ckpt
            .restore(&mut resumed, &store, &report.manifest, cp())
            .unwrap();
        assert_eq!(restored.ticks, split as u64);
        assert_eq!(restored.parent, None);
        assert_eq!(resumed.ticks(), split as u64);
        for (i, t) in all.iter().enumerate().skip(split) {
            assert_eq!(
                resumed.tick(t).unwrap(),
                expected[i],
                "split {split} tick {i}"
            );
        }
        assert_eq!(resumed.materialized(), reference.materialized());
    }
}

#[test]
fn incremental_checkpoints_write_only_changed_arrangements() {
    let (_dir, store) = store();
    let all = ticks();
    let mut e = circuit().executor();
    let nodes = e.circuit().node_count();
    let mut ckpt = DurableCheckpointer::new();
    e.tick(&all[0]).unwrap();
    let first = ckpt.checkpoint(&e, &store, cp()).unwrap();
    assert_eq!((first.written, first.reused), (nodes, 0));
    // No tick: nothing changed, only a new manifest (with a parent link).
    let same = ckpt.checkpoint(&e, &store, cp()).unwrap();
    assert_eq!((same.written, same.reused), (0, nodes));
    assert_ne!(same.manifest, first.manifest);
    // Tick 1 changes only orders: customers' input integral is reused.
    e.tick(&all[1]).unwrap();
    let second = ckpt.checkpoint(&e, &store, cp()).unwrap();
    assert!(second.reused >= 1, "{second:?}");
    assert_eq!(second.written + second.reused, nodes);
    // The chain restores and records its parent.
    let mut r = circuit().executor();
    let restored = DurableCheckpointer::new()
        .restore(&mut r, &store, &second.manifest, cp())
        .unwrap();
    assert_eq!(restored.parent, Some(same.manifest));
    assert_eq!(r.materialized(), e.materialized());
    // After restore, the checkpointer continues incrementally from the manifest.
    let mut ckpt2 = DurableCheckpointer::new();
    let mut r2 = circuit().executor();
    ckpt2
        .restore(&mut r2, &store, &second.manifest, cp())
        .unwrap();
    let again = ckpt2.checkpoint(&r2, &store, cp()).unwrap();
    assert_eq!((again.written, again.reused), (0, nodes));
    // A checkpointer used with a different executor history rewrites fully.
    let other = circuit().executor();
    let full = ckpt2.checkpoint(&other, &store, cp()).unwrap();
    assert_eq!((full.written, full.reused), (nodes, 0));
    // Restoring rebinds the checkpointer to the restored manifest, so the
    // next checkpoint is again incremental relative to it.
    ckpt.restore(&mut e, &store, &first.manifest, cp()).unwrap();
    e.tick(&all[1]).unwrap();
    let after = ckpt.checkpoint(&e, &store, cp()).unwrap();
    assert!(after.reused >= 1, "{after:?}");
    let mut r3 = circuit().executor();
    let back = DurableCheckpointer::new()
        .restore(&mut r3, &store, &after.manifest, cp())
        .unwrap();
    assert_eq!(back.parent, Some(first.manifest));
    assert_eq!(r3.materialized(), e.materialized());
}

/// Checkpoint after two ticks; returns the manifest id and arrangement ids.
fn checkpointed(store: &Store) -> (Executor, DurableReport, Vec<ObjectId>) {
    let all = ticks();
    let mut e = circuit().executor();
    e.tick(&all[0]).unwrap();
    e.tick(&all[1]).unwrap();
    let report = DurableCheckpointer::new()
        .checkpoint(&e, store, cp())
        .unwrap();
    let arrangements = DurableCheckpointer::new()
        .restore(&mut circuit().executor(), store, &report.manifest, cp())
        .unwrap()
        .arrangements;
    (e, report, arrangements)
}
/// A fresh executor at tick 1 whose state must survive every failed restore.
fn victim() -> Executor {
    let mut v = circuit().executor();
    v.tick(&ticks()[0]).unwrap();
    v
}
fn assert_untouched(v: &Executor) {
    let w = victim();
    assert_eq!(v.ticks(), 1);
    assert_eq!(v.materialized(), w.materialized());
}

#[test]
fn tampered_missing_and_substituted_objects_are_rejected_transactionally() {
    let (_dir, store) = store();
    let (_, report, arrangements) = checkpointed(&store);
    let manifest_bytes = store.read_blob(&report.manifest).unwrap();

    // 1. Flip one byte of a stored arrangement: the store's rehash rejects it.
    let target = arrangements[0];
    let original = store.read_blob(&target).unwrap();
    let mut flipped = original.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    overwrite(&store, &target, &flipped);
    let mut v = victim();
    let mut ckpt = DurableCheckpointer::new();
    assert_eq!(
        ckpt.restore(&mut v, &store, &report.manifest, cp()),
        Err(Error::Storage("corrupt object"))
    );
    assert_untouched(&v);
    assert_eq!(ckpt.last_manifest(), None);
    overwrite(&store, &target, &original);

    // 2. Tamper the manifest itself.
    let mut bad = manifest_bytes.clone();
    bad[20] ^= 0x40;
    overwrite(&store, &report.manifest, &bad);
    assert_eq!(
        ckpt.restore(&mut v, &store, &report.manifest, cp()),
        Err(Error::Storage("corrupt object"))
    );
    assert_untouched(&v);
    overwrite(&store, &report.manifest, &manifest_bytes);

    // 3. Missing arrangement object.
    let path = store.object_path(&arrangements[1]);
    let saved = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        ckpt.restore(&mut v, &store, &report.manifest, cp()),
        Err(Error::Storage("missing object"))
    );
    assert_untouched(&v);
    store.put_blob(Domain::Artifact, &saved).unwrap();

    // 4. A well-formed, correctly hashed manifest that substitutes another
    //    node's arrangement (valid content, wrong binding).
    let find = |bytes: &[u8], id: &ObjectId| {
        bytes
            .windows(32)
            .position(|w| w == id.digest.as_bytes())
            .unwrap()
    };
    let mut swapped = manifest_bytes.clone();
    let at = find(&swapped, &arrangements[0]);
    swapped[at..at + 32].copy_from_slice(arrangements[2].digest.as_bytes());
    let forged = store.put_blob(Domain::Artifact, &swapped).unwrap().id;
    assert!(matches!(
        ckpt.restore(&mut v, &store, &forged, cp()),
        Err(Error::Checkpoint(_))
    ));
    assert_untouched(&v);

    // 5. A stale arrangement of the same node from an older checkpoint whose
    //    row count happens to match: the relational revalidation rejects it.
    let mut older = circuit().executor();
    older.tick(&ticks()[0]).unwrap();
    let old = DurableCheckpointer::new()
        .checkpoint(&older, &store, cp())
        .unwrap();
    let old_arr = DurableCheckpointer::new()
        .restore(&mut circuit().executor(), &store, &old.manifest, cp())
        .unwrap()
        .arrangements;
    let mut mixed = manifest_bytes.clone();
    for node in 0..arrangements.len() {
        if old_arr[node] != arrangements[node] {
            let at = find(&mixed, &arrangements[node]);
            mixed[at..at + 32].copy_from_slice(old_arr[node].digest.as_bytes());
            break;
        }
    }
    let forged = store.put_blob(Domain::Artifact, &mixed).unwrap().id;
    assert!(matches!(
        ckpt.restore(&mut v, &store, &forged, cp()),
        Err(Error::Checkpoint(_))
    ));
    assert_untouched(&v);

    // 6. Truncated and trailing-byte manifests stored under their own hashes.
    for bytes in [
        manifest_bytes[..manifest_bytes.len() - 3].to_vec(),
        [manifest_bytes.as_slice(), &[0]].concat(),
        Vec::new(),
    ] {
        let id = store.put_blob(Domain::Artifact, &bytes).unwrap().id;
        assert!(matches!(
            ckpt.restore(&mut v, &store, &id, cp()),
            Err(Error::Checkpoint(_))
        ));
        assert_untouched(&v);
    }

    // 7. Wrong domain and wrong circuit.
    let src = store.put_blob(Domain::Source, &manifest_bytes).unwrap().id;
    assert_eq!(
        ckpt.restore(&mut v, &store, &src, cp()),
        Err(Error::Storage("wrong object kind"))
    );
    let mut b = CircuitBuilder::new(l()).unwrap();
    let x = b.input(2).unwrap();
    let y = b.input(2).unwrap();
    let j = b.equijoin(x, y, 0, 0).unwrap();
    let mut other = b.finish(j).unwrap().executor();
    assert_eq!(
        ckpt.restore(&mut other, &store, &report.manifest, cp()),
        Err(Error::Checkpoint("identity"))
    );
    // 8. Oversized image under a small byte limit.
    let small = CheckpointLimits {
        max_bytes: 64,
        ..cp()
    };
    assert_eq!(
        ckpt.restore(&mut v, &store, &report.manifest, small),
        Err(Error::Limit("checkpoint bytes"))
    );
    assert_untouched(&v);
    // Untampered restore still succeeds afterwards.
    ckpt.restore(&mut v, &store, &report.manifest, cp())
        .unwrap();
    assert_eq!(v.ticks(), 2);
}

#[test]
fn cdc_offsets_are_durable() {
    let mut b = CircuitBuilder::new(l()).unwrap();
    let a = b.input(1).unwrap();
    let c = b.finish(a).unwrap();
    let policy = CdcPolicy {
        bindings: vec![SourceBinding {
            partition_id: 4,
            schema_id: 1,
            input: 0,
            first_lsn: 100,
            weights: WeightPolicy::Unit,
        }],
        max_batch_bytes: 4096,
        max_records: 16,
    };
    let header = |lsn| CdcHeader {
        source_lsn: lsn,
        event_ts_ms: 0,
        z_weight: 1,
        partition_id: 4,
        schema_id: 1,
        flags: 0,
    };
    let (_dir, store) = store();
    let mut e = c.clone().executor();
    e.bind_sources(&policy).unwrap();
    let rec = e.encode_cdc_record(header(100), &[7]).unwrap();
    e.ingest_cdc(&rec).unwrap();
    let report = DurableCheckpointer::new()
        .checkpoint(&e, &store, cp())
        .unwrap();
    let mut r = c.clone().executor();
    r.bind_sources(&policy).unwrap();
    DurableCheckpointer::new()
        .restore(&mut r, &store, &report.manifest, cp())
        .unwrap();
    assert_eq!(r.next_source_lsn(4), Some(101));
    // Replaying the already-consumed record is rejected; the next one is accepted.
    assert!(r.ingest_cdc(&rec).is_err());
    let next = r.encode_cdc_record(header(101), &[8]).unwrap();
    r.ingest_cdc(&next).unwrap();
    // An executor without the bound policy has a different execution identity.
    assert_eq!(
        DurableCheckpointer::new().restore(&mut c.executor(), &store, &report.manifest, cp()),
        Err(Error::Checkpoint("identity"))
    );
}
