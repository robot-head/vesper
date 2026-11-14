//! Durable incremental checkpoints in the loom CAS: round trips, LSM spine
//! appends and compaction, tamper/missing/substitution rejection and CDC
//! offsets.
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
    // Tick 1 changes only orders: customers' spine is reused; every changed
    // spine gets a one-row batch, merged into its older batch (geometric).
    e.tick(&all[1]).unwrap();
    let second = ckpt.checkpoint(&e, &store, cp()).unwrap();
    assert!(second.reused >= 1, "{second:?}");
    assert!(second.written >= 1 && second.merged >= 1, "{second:?}");
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
    let mut other = circuit().executor();
    other.tick(&all[0]).unwrap();
    let full = ckpt2.checkpoint(&other, &store, cp()).unwrap();
    assert_eq!((full.written, full.reused, full.merged), (nodes, 0, 0));
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

/// Every node's single batch (all spines of a full write have one).
fn single_batches(store: &Store, manifest: &ObjectId) -> Vec<ObjectId> {
    let spines = DurableCheckpointer::new()
        .restore(&mut circuit().executor(), store, manifest, cp())
        .unwrap()
        .spines;
    assert!(spines.iter().all(|s| s.len() == 1), "{spines:?}");
    spines.into_iter().flatten().collect()
}
/// Checkpoint after two ticks; returns the manifest id and batch ids.
fn checkpointed(store: &Store) -> (Executor, DurableReport, Vec<ObjectId>) {
    let all = ticks();
    let mut e = circuit().executor();
    e.tick(&all[0]).unwrap();
    e.tick(&all[1]).unwrap();
    let report = DurableCheckpointer::new()
        .checkpoint(&e, store, cp())
        .unwrap();
    let arrangements = single_batches(store, &report.manifest);
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
    let old_arr = single_batches(&store, &old.manifest);
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

#[test]
fn reuse_never_crosses_stores_or_missing_objects() {
    let (_a_dir, a) = store();
    let (_b_dir, b) = store();
    let all = ticks();
    let mut e = circuit().executor();
    let mut ckpt = DurableCheckpointer::new();
    e.tick(&all[0]).unwrap();
    ckpt.checkpoint(&e, &a, cp()).unwrap();
    e.tick(&all[1]).unwrap();
    // Switching stores rewrites every arrangement into the new store.
    let into_b = ckpt.checkpoint(&e, &b, cp()).unwrap();
    assert_eq!(into_b.reused, 0);
    let mut r = circuit().executor();
    DurableCheckpointer::new()
        .restore(&mut r, &b, &into_b.manifest, cp())
        .unwrap();
    assert_eq!(r.materialized(), e.materialized());
    // An arrangement deleted from the store behind the checkpointer's back is
    // rewritten instead of referenced.
    let arrangements: Vec<ObjectId> = DurableCheckpointer::new()
        .restore(&mut circuit().executor(), &b, &into_b.manifest, cp())
        .unwrap()
        .spines
        .into_iter()
        .flatten()
        .collect();
    std::fs::remove_file(b.object_path(&arrangements[1])).unwrap();
    let again = ckpt.checkpoint(&e, &b, cp()).unwrap();
    assert_eq!(again.written, 1);
    DurableCheckpointer::new()
        .restore(&mut circuit().executor(), &b, &again.manifest, cp())
        .unwrap();
}

#[test]
fn a_checkpoint_accepted_under_limits_restores_under_the_same_limits() {
    let (_dir, store) = store();
    let all = ticks();
    let mut e = circuit().executor();
    e.tick(&all[0]).unwrap();
    e.tick(&all[1]).unwrap();
    let mut tight = None;
    for max_work in 1..10_000 {
        let limits = CheckpointLimits { max_work, ..cp() };
        if let Ok(report) = DurableCheckpointer::new().checkpoint(&e, &store, limits) {
            tight = Some((limits, report));
            break;
        }
    }
    let (limits, report) = tight.expect("some budget admits the checkpoint");
    let mut r = circuit().executor();
    DurableCheckpointer::new()
        .restore(&mut r, &store, &report.manifest, limits)
        .unwrap();
    assert_eq!(r.materialized(), e.materialized());
    // The aggregate cell bound is enforced from the manifest.
    let few_cells = CheckpointLimits {
        max_total_cells: 3,
        ..cp()
    };
    assert_eq!(
        DurableCheckpointer::new().restore(
            &mut circuit().executor(),
            &store,
            &report.manifest,
            few_cells
        ),
        Err(Error::Limit("checkpoint state"))
    );
}

/// One large input integral with a small change per tick: every checkpoint
/// writes O(change) rows, the spine stays within its level bound, merges
/// elide cancelled rows, and every checkpoint restores to the live state.
#[test]
fn spine_appends_deltas_and_compacts_geometrically() {
    let (_dir, store) = store();
    let mut b = CircuitBuilder::new(l()).unwrap();
    let x = b.input(2).unwrap();
    let f = b
        .filter(
            x,
            Predicate::AtLeast {
                column: 1,
                value: 0,
            },
        )
        .unwrap();
    let c = b.finish(f).unwrap();
    let mut e = c.clone().executor();
    let base: Vec<_> = (0..400).map(|i| (vec![i, i % 7], 1)).collect();
    e.tick(&TickInput {
        deltas: vec![ZSet::consolidate(2, &base, l()).unwrap()],
    })
    .unwrap();
    let mut ckpt = DurableCheckpointer::new();
    let first = ckpt.checkpoint(&e, &store, cp()).unwrap();
    assert_eq!(first.rows_written, 800);
    let mut merged = 0;
    let mut max_levels = 0;
    for t in 0..40i64 {
        // Insert a row, and retract the row inserted two ticks ago.
        let mut rows = vec![(vec![1000 + t, 1], 1)];
        if t >= 2 {
            rows.push((vec![1000 + t - 2, 1], -1));
        }
        e.tick(&TickInput {
            deltas: vec![ZSet::consolidate(2, &rows, l()).unwrap()],
        })
        .unwrap();
        let r = ckpt.checkpoint(&e, &store, cp()).unwrap();
        merged += r.merged;
        // Small batches are merged among themselves, never into the 400-row base.
        assert!(r.rows_written < 100, "tick {t}: {r:?}");
        let mut back = c.clone().executor();
        let restored = DurableCheckpointer::new()
            .restore(&mut back, &store, &r.manifest, cp())
            .unwrap();
        max_levels = max_levels.max(restored.spines.iter().map(Vec::len).max().unwrap());
        assert_eq!(back.materialized(), e.materialized(), "tick {t}");
        assert_eq!(back.ticks(), e.ticks());
    }
    assert!(merged > 10, "{merged}");
    assert!((2..=MAX_SPINE_LEVELS).contains(&max_levels), "{max_levels}");
}

#[test]
fn malformed_spines_are_rejected() {
    let (_dir, store) = store();
    let all = ticks();
    let mut e = circuit().executor();
    let mut ckpt = DurableCheckpointer::new();
    e.tick(&all[0]).unwrap();
    ckpt.checkpoint(&e, &store, cp()).unwrap();
    e.tick(&all[2]).unwrap();
    let report = ckpt.checkpoint(&e, &store, cp()).unwrap();
    let bytes = store.read_blob(&report.manifest).unwrap();
    let spines = DurableCheckpointer::new()
        .restore(&mut circuit().executor(), &store, &report.manifest, cp())
        .unwrap()
        .spines;
    let find = |bytes: &[u8], id: &ObjectId| {
        bytes
            .windows(32)
            .position(|w| w == id.digest.as_bytes())
            .unwrap()
    };
    // Naming one batch twice, and dropping a batch's range bound past the
    // checkpoint's tick count, are typed errors.
    let two: Vec<&ObjectId> = spines.iter().flatten().take(2).collect();
    let mut dup = bytes.clone();
    let at = find(&dup, two[1]);
    dup[at..at + 32].copy_from_slice(two[0].digest.as_bytes());
    let id = store.put_blob(Domain::Artifact, &dup).unwrap().id;
    assert!(matches!(
        DurableCheckpointer::new().restore(&mut victim(), &store, &id, cp()),
        Err(Error::Checkpoint(_))
    ));
    let mut far = bytes.clone();
    let at = find(&far, two[0]) + 32 + 8; // the batch's `hi`
    far[at..at + 8].copy_from_slice(&99u64.to_le_bytes());
    let id = store.put_blob(Domain::Artifact, &far).unwrap().id;
    let mut v = victim();
    assert_eq!(
        DurableCheckpointer::new().restore(&mut v, &store, &id, cp()),
        Err(Error::Checkpoint("spine ranges"))
    );
    assert_untouched(&v);
}

/// A retained base plus a delta can need more batch rows than the integral:
/// the checkpoint folds that spine so it restores under the same limits.
#[test]
fn incremental_checkpoints_stay_within_the_batch_row_bound() {
    let (_dir, store) = store();
    let mut b = CircuitBuilder::new(l()).unwrap();
    let x = b.input(2).unwrap();
    let c = b.finish(x).unwrap();
    let limits = CheckpointLimits {
        max_total_rows: 100,
        ..cp()
    };
    let mut e = c.clone().executor();
    let base: Vec<_> = (0..100).map(|i| (vec![i, 0], 1)).collect();
    e.tick(&TickInput {
        deltas: vec![ZSet::consolidate(2, &base, l()).unwrap()],
    })
    .unwrap();
    let mut ckpt = DurableCheckpointer::new();
    ckpt.checkpoint(&e, &store, limits).unwrap();
    e.tick(&TickInput {
        deltas: vec![set(2, &[(&[0, 0], 1)])],
    })
    .unwrap();
    let report = ckpt.checkpoint(&e, &store, limits).unwrap();
    let mut back = c.clone().executor();
    let restored = DurableCheckpointer::new()
        .restore(&mut back, &store, &report.manifest, limits)
        .unwrap();
    assert_eq!(back.materialized(), e.materialized());
    assert_eq!(restored.spines[0].len(), 1);
}
