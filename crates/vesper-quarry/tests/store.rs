//! Table store behaviour: B+Tree shape and page reuse, MVCC snapshot
//! isolation anomalies, group commit, fuzzy checkpoints, SQL over durable
//! tables, typed errors and pin conservation.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use vesper_quarry::device::{CrashChoice, FileId, FsDevice, SimDevice};
use vesper_quarry::store::{SharedStore, Store, StoreErrorKind, StoreLimits};
use vesper_quarry::vector::VectorEngine;
use vesper_quarry::{Limits, ParamDef, ScalarType, Value};

fn small() -> StoreLimits {
    StoreLimits {
        pool_frames: 8,
        btree_fanout: 3,
        leaf_max_versions: 4,
        ..StoreLimits::default()
    }
}
fn row(id: u64, name: &str) -> Vec<Value> {
    vec![Value::U64(id), Value::Text(name.into()), Value::Null]
}
const DDL: &str = "CREATE TABLE t (id U64 NOT NULL, name TEXT NOT NULL, note TEXT NULL)";

fn store(limits: StoreLimits) -> Store<SimDevice> {
    let (mut s, rep) = Store::open(SimDevice::new(), limits).unwrap();
    assert!(rep.created);
    s.execute_ddl(DDL).unwrap();
    s
}
fn ids(rows: &[(u64, Vec<Value>)]) -> Vec<u64> {
    rows.iter().map(|(id, _)| *id).collect()
}

#[test]
fn btree_splits_reuses_freed_pages_and_survives_reopen() {
    let mut s = store(small());
    let mut ids = Vec::new();
    for batch in 0..10 {
        let mut t = s.begin().unwrap();
        for i in 0..20 {
            let n = batch * 20 + i;
            ids.push(
                s.insert(&mut t, "t", &row(n, &format!("name-{n}")))
                    .unwrap(),
            );
        }
        s.commit(t).unwrap();
    }
    let shape = s.check().unwrap()["t"];
    assert_eq!(shape.versions, 200);
    assert!(shape.height >= 4, "fanout 3 builds a deep tree: {shape:?}");
    // Point reads through the tree.
    let t = s.begin().unwrap();
    for (n, &id) in ids.iter().enumerate() {
        assert_eq!(
            s.get(&t, "t", id).unwrap(),
            Some(row(n as u64, &format!("name-{n}")))
        );
    }
    assert_eq!(s.get(&t, "t", 10_000).unwrap(), None);
    s.commit(t).unwrap();

    let mut t = s.begin().unwrap();
    for &id in &ids[..90] {
        s.delete(&mut t, "t", id).unwrap();
    }
    for &id in &ids[90..150] {
        s.update(&mut t, "t", id, &row(id * 1000, "updated"))
            .unwrap();
    }
    s.commit(t).unwrap();
    s.check().unwrap();
    let pages_before = s.stats().pages;

    // Nothing reads the old versions any more: vacuum removes them (and the
    // delete tombstones), frees emptied leaves and compacts leaf heaps.
    let v = s.vacuum().unwrap();
    assert_eq!(v.versions_removed, 90 * 2 + 60);
    assert!(v.leaves_compacted > 0);
    let shape = s.check().unwrap()["t"];
    assert_eq!(shape.versions, 110);
    assert!(
        s.stats().free_pages > 0,
        "emptied leaves return to the free-space map"
    );
    // New inserts reuse free pages before growing the file.
    let free = s.stats().free_pages;
    let mut t = s.begin().unwrap();
    for n in 0..40 {
        s.insert(&mut t, "t", &row(n, "again")).unwrap();
    }
    s.commit(t).unwrap();
    assert!(s.stats().free_pages < free);
    assert_eq!(s.stats().pages, pages_before);
    s.check().unwrap();

    let dev = s.close().unwrap();
    let (mut s, rep) = Store::open(dev, small()).unwrap();
    assert_eq!(rep.records, 0, "close checkpointed");
    assert_eq!(s.check().unwrap()["t"].versions, 150);
    let t = s.begin().unwrap();
    assert_eq!(s.scan(&t, "t").unwrap().len(), 150);
    s.commit(t).unwrap();
    assert_eq!(s.stats().pinned, 0);
    assert_eq!(s.stats().leaked_pins, 0);
}

#[test]
fn snapshot_isolation_anomalies() {
    let mut s = store(StoreLimits::default());
    let mut t = s.begin().unwrap();
    let x = s.insert(&mut t, "t", &row(100, "x")).unwrap();
    let y = s.insert(&mut t, "t", &row(100, "y")).unwrap();
    s.commit(t).unwrap();

    // Dirty write: a second writer of a locked row conflicts (no wait).
    let mut a = s.begin().unwrap();
    let mut b = s.begin().unwrap();
    s.update(&mut a, "t", x, &row(1, "x-a")).unwrap();
    assert_eq!(
        s.update(&mut b, "t", x, &row(2, "x-b")).unwrap_err().kind,
        StoreErrorKind::Conflict
    );
    // Dirty read: b never sees a's uncommitted write.
    assert_eq!(s.get(&b, "t", x).unwrap(), Some(row(100, "x")));
    // Non-repeatable read: after a commits, b still reads its snapshot.
    s.commit(a).unwrap();
    assert_eq!(s.get(&b, "t", x).unwrap(), Some(row(100, "x")));
    // Lost update: b's write of x is rejected, first committer wins.
    assert_eq!(
        s.update(&mut b, "t", x, &row(3, "x-b")).unwrap_err().kind,
        StoreErrorKind::Conflict
    );
    s.abort(b).unwrap();
    let c = s.begin().unwrap();
    assert_eq!(s.get(&c, "t", x).unwrap(), Some(row(1, "x-a")));

    // Phantom: rows inserted and committed later are invisible to c.
    let mut d = s.begin().unwrap();
    let z = s.insert(&mut d, "t", &row(7, "phantom")).unwrap();
    s.commit(d).unwrap();
    assert_eq!(ids(&s.scan(&c, "t").unwrap()), vec![x, y]);
    let q = "SELECT name FROM t WHERE id = :id";
    let p = [ParamDef {
        name: "id".into(),
        ty: ScalarType::U64,
        nullable: false,
    }];
    let b = BTreeMap::from([("id".to_string(), Value::U64(7))]);
    let none = s.query(&c, q, &p, &b, Limits::default()).unwrap();
    assert!(none.rows.is_empty());
    s.commit(c).unwrap();
    let e = s.begin().unwrap();
    assert_eq!(ids(&s.scan(&e, "t").unwrap()), vec![x, y, z]);
    s.commit(e).unwrap();

    // Read skew: a reader of x and y sees both before or both after a
    // concurrent transfer, never a mix.
    let r = s.begin().unwrap();
    let mut w = s.begin().unwrap();
    s.update(&mut w, "t", x, &row(0, "x-moved")).unwrap();
    s.update(&mut w, "t", y, &row(200, "y-moved")).unwrap();
    assert_eq!(s.get(&r, "t", x).unwrap(), Some(row(1, "x-a")));
    s.commit(w).unwrap();
    assert_eq!(s.get(&r, "t", y).unwrap(), Some(row(100, "y")));
    s.commit(r).unwrap();

    // Write skew is permitted by snapshot isolation (it is not
    // serializable): each transaction reads both rows and writes a
    // different one; both commit.
    let mut p = s.begin().unwrap();
    let mut q2 = s.begin().unwrap();
    let both = |s: &mut Store<SimDevice>, t: &vesper_quarry::store::Txn| {
        (s.get(t, "t", x).unwrap(), s.get(t, "t", y).unwrap())
    };
    assert_eq!(both(&mut s, &p), both(&mut s, &q2));
    s.update(&mut p, "t", x, &row(0, "p")).unwrap();
    s.update(&mut q2, "t", y, &row(0, "q")).unwrap();
    s.commit(p).unwrap();
    s.commit(q2).unwrap();
    let f = s.begin().unwrap();
    assert_eq!(s.get(&f, "t", x).unwrap(), Some(row(0, "p")));
    assert_eq!(s.get(&f, "t", y).unwrap(), Some(row(0, "q")));
    s.commit(f).unwrap();

    // A transaction sees its own writes; deleting twice is NotFound.
    let mut g = s.begin().unwrap();
    s.delete(&mut g, "t", x).unwrap();
    assert_eq!(s.get(&g, "t", x).unwrap(), None);
    assert_eq!(
        s.delete(&mut g, "t", x).unwrap_err().kind,
        StoreErrorKind::NotFound
    );
    s.abort(g).unwrap();
    let h = s.begin().unwrap();
    assert!(
        s.get(&h, "t", x).unwrap().is_some(),
        "abort discards writes"
    );
    s.commit(h).unwrap();
    assert_eq!(s.stats().locks, 0);
    assert_eq!(s.stats().pinned, 0);
}

#[test]
fn old_versions_stay_until_no_snapshot_needs_them() {
    let mut s = store(small());
    let mut t = s.begin().unwrap();
    let x = s.insert(&mut t, "t", &row(1, "v1")).unwrap();
    s.commit(t).unwrap();
    let old = s.begin().unwrap();
    for v in 2..6 {
        let mut t = s.begin().unwrap();
        s.update(&mut t, "t", x, &row(v, "vN")).unwrap();
        s.commit(t).unwrap();
    }
    // `old` pins version 1; intermediate versions are collected by commits.
    assert_eq!(s.get(&old, "t", x).unwrap(), Some(row(1, "v1")));
    assert_eq!(
        s.check().unwrap()["t"].versions,
        3,
        "v1 (old snapshot), v4 (visible when v5 committed) and v5"
    );
    assert_eq!(s.vacuum().unwrap().versions_removed, 1, "v4");
    assert_eq!(s.get(&old, "t", x).unwrap(), Some(row(1, "v1")));
    s.commit(old).unwrap();
    assert_eq!(s.vacuum().unwrap().versions_removed, 1, "v1");
    assert_eq!(s.check().unwrap()["t"].versions, 1);
}

#[test]
fn deferred_commits_share_one_sync_and_become_visible_when_durable() {
    let s = store(StoreLimits::default());
    let flushes = s.stats().log_flushes;
    let mut lsns = Vec::new();
    for i in 0..10 {
        let mut t = s.begin().unwrap();
        s.insert(&mut t, "t", &row(i, "g")).unwrap();
        let c = s.commit_deferred(t).unwrap();
        assert!(!c.durable);
        lsns.push(c.lsn);
    }
    assert_eq!(s.stats().log_flushes, flushes, "no sync yet");
    assert_eq!(s.stats().pending_commits, 10);
    let before = s.begin().unwrap();
    assert!(
        s.scan(&before, "t").unwrap().is_empty(),
        "not durable, not visible"
    );
    s.commit(before).unwrap();
    s.flush().unwrap();
    assert_eq!(
        s.stats().log_flushes,
        flushes + 1,
        "one sync for ten commits"
    );
    assert!(lsns.iter().all(|&l| l <= s.durable_lsn()));
    let after = s.begin().unwrap();
    assert_eq!(s.scan(&after, "t").unwrap().len(), 10);
    s.commit(after).unwrap();
}

#[test]
fn shared_store_groups_concurrent_commits() {
    let s = store(StoreLimits::default());
    let shared = Arc::new(SharedStore::new(s, Duration::from_millis(2)));
    let threads: Vec<_> = (0..8)
        .map(|th| {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                for i in 0..25 {
                    let t = shared.with(|s| {
                        let mut t = s.begin().unwrap();
                        s.insert(&mut t, "t", &row(th * 1000 + i, "thread"))
                            .unwrap();
                        t
                    });
                    let c = shared.commit(t).unwrap();
                    assert!(c.durable);
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let (commits, syncs) = shared.counts();
    eprintln!("group commit: {commits} commits, {syncs} log syncs");
    assert_eq!(commits, 200);
    assert!(syncs < commits, "{syncs} syncs for {commits} commits");
    let s = Arc::try_unwrap(shared).ok().unwrap().into_inner();
    let t = s.begin().unwrap();
    assert_eq!(s.scan(&t, "t").unwrap().len(), 200);
    s.commit(t).unwrap();
}

#[test]
fn fuzzy_checkpoint_runs_while_transactions_commit() {
    let limits = StoreLimits {
        log_region_bytes: 1 << 20,
        checkpoint_step_pages: 1,
        ..small()
    };
    let mut s = store(limits);
    let long = s.begin().unwrap(); // stays open across the checkpoint
    let mut t = s.begin().unwrap();
    for i in 0..30 {
        s.insert(&mut t, "t", &row(i, "before")).unwrap();
    }
    s.commit(t).unwrap();
    s.checkpoint_begin().unwrap();
    assert!(s.stats().checkpoint_running);
    let mut steps = 0;
    loop {
        let mut t = s.begin().unwrap();
        s.insert(&mut t, "t", &row(steps, "during")).unwrap();
        s.commit(t).unwrap();
        steps += 1;
        if s.checkpoint_step(1).unwrap() {
            break;
        }
    }
    assert!(steps > 1, "commits ran during the checkpoint");
    assert_eq!(s.stats().checkpoints_done, 1);
    s.commit(long).unwrap();
    // A crash now recovers from the new region only.
    let dev = s.into_device();
    let (mut s, rep) = Store::open(dev, limits).unwrap();
    assert!(!rep.previous_region);
    let t = s.begin().unwrap();
    assert_eq!(s.scan(&t, "t").unwrap().len(), 30 + steps as usize);
    s.commit(t).unwrap();
    // Log regions switch automatically once half full.
    for i in 0..200 {
        let mut t = s.begin().unwrap();
        s.insert(&mut t, "t", &row(i, &"y".repeat(800))).unwrap();
        s.commit(t).unwrap();
    }
    assert!(s.stats().checkpoints_begun > 1);
    s.check().unwrap();
}

#[test]
fn sql_queries_and_vectorized_joins_run_over_durable_tables() {
    let (mut s, _) = Store::open(SimDevice::new(), small()).unwrap();
    s.execute_ddl(
        "CREATE TABLE orders (order_id U64 NOT NULL, org_id U64 NOT NULL, total_cents I64 NOT NULL, customer TEXT NULL); \
         CREATE TABLE orgs (org_id U64 NOT NULL, name TEXT NOT NULL)",
    )
    .unwrap();
    let mut t = s.begin().unwrap();
    for org in 0..4u64 {
        s.insert(
            &mut t,
            "orgs",
            &[Value::U64(org), Value::Text(format!("org{org}"))],
        )
        .unwrap();
    }
    for o in 0..120u64 {
        s.insert(
            &mut t,
            "orders",
            &[
                Value::U64(o),
                Value::U64(o % 5),
                Value::I64((o as i64 * 37) % 1000),
                if o % 7 == 0 {
                    Value::Null
                } else {
                    Value::Text(format!("c{o}"))
                },
            ],
        )
        .unwrap();
    }
    s.commit(t).unwrap();
    let dev = s.close().unwrap();
    let (mut s, _) = Store::open(dev, small()).unwrap();
    let t = s.begin().unwrap();
    let sql = "SELECT o.order_id, g.name, o.total_cents FROM orders o \
               LEFT JOIN orgs g ON o.org_id = g.org_id WHERE o.total_cents >= :min";
    let params = [ParamDef {
        name: "min".into(),
        ty: ScalarType::I64,
        nullable: false,
    }];
    let binds = BTreeMap::from([("min".to_string(), Value::I64(500))]);
    let rows = s
        .query(&t, sql, &params, &binds, Limits::default())
        .unwrap();
    let vec = s
        .query_vectorized(
            &t,
            sql,
            &params,
            &binds,
            Limits::default(),
            &VectorEngine::detect(),
        )
        .unwrap();
    assert_eq!(rows.rows, vec.rows, "row and vectorized executors agree");
    assert_eq!(rows.columns, vec.columns);
    assert!(
        rows.rows.iter().any(|r| r[1] == Value::Null),
        "org 4 does not exist"
    );
    assert_eq!(
        rows.rows.len(),
        (0..120).filter(|o| (o * 37) % 1000 >= 500).count()
    );
    s.commit(t).unwrap();
    // Schema changes are durable and drop frees pages.
    s.execute_ddl("DROP TABLE orders").unwrap();
    assert!(s.stats().free_pages > 0);
    s.check().unwrap();
    let t = s.begin().unwrap();
    assert_eq!(
        s.query(
            &t,
            "SELECT * FROM orders",
            &[],
            &BTreeMap::new(),
            Limits::default()
        )
        .unwrap_err()
        .kind,
        StoreErrorKind::Sql(vesper_quarry::ErrorKind::Unknown)
    );
    s.commit(t).unwrap();
}

#[test]
fn typed_errors_and_no_leaked_pins() {
    let mut s = store(small());
    let mut t = s.begin().unwrap();
    assert_eq!(
        s.insert(&mut t, "t", &[Value::U64(1)]).unwrap_err().kind,
        StoreErrorKind::Data
    );
    assert_eq!(
        s.insert(
            &mut t,
            "t",
            &[Value::I64(1), Value::Text("x".into()), Value::Null]
        )
        .unwrap_err()
        .kind,
        StoreErrorKind::Data
    );
    assert_eq!(
        s.insert(&mut t, "nope", &row(1, "x")).unwrap_err().kind,
        StoreErrorKind::NotFound
    );
    let huge = vec![Value::U64(1), Value::Text("z".repeat(5000)), Value::Null];
    assert_eq!(
        s.insert(&mut t, "t", &huge).unwrap_err().kind,
        StoreErrorKind::Limit
    );
    let id = s.insert(&mut t, "t", &row(1, "ok")).unwrap();
    assert_eq!(
        s.execute_ddl("DROP TABLE t").unwrap_err().kind,
        StoreErrorKind::State,
        "DDL waits for transactions"
    );
    s.commit(t).unwrap();
    let other = Store::open(SimDevice::new(), small()).unwrap().0;
    let mut t = s.begin().unwrap();
    assert_eq!(
        other
            .update(&mut t, "t", id, &row(2, "x"))
            .unwrap_err()
            .kind,
        StoreErrorKind::WrongStore
    );
    s.abort(t).unwrap();
    for _ in 0..30 {
        let mut t = s.begin().unwrap();
        s.insert(&mut t, "t", &row(1, &"q".repeat(1000))).unwrap();
        s.commit(t).unwrap();
    }
    assert_eq!(s.stats().pinned, 0);

    // Bit rot in a page that no logged image covers is Corrupt, not a
    // silent repair, and leaves no frame pinned.
    let mut dev = s.close().unwrap();
    dev.corrupt(FileId::Data, 5 * 16384 + 100, 0x40);
    let (s, _) = Store::open(dev, small()).unwrap();
    let t = s.begin().unwrap();
    let err = s.scan(&t, "t").unwrap_err();
    assert_eq!(err.kind, StoreErrorKind::Corrupt);
    assert_eq!(s.stats().pinned, 0);
    assert!(s.poisoned());
    assert_eq!(s.begin().unwrap_err().kind, StoreErrorKind::State);
    std::mem::forget(t);

    // A corrupt log header region is refused instead of reinitialized.
    let mut dev = SimDevice::new();
    let (mut s, _) = Store::open(&mut dev, small()).unwrap();
    s.execute_ddl(DDL).unwrap();
    drop(s);
    for at in 0..128 {
        dev.corrupt(FileId::Wal, at, 0xff);
    }
    assert_eq!(
        Store::open(&mut dev, small()).unwrap_err().kind,
        StoreErrorKind::Corrupt
    );
}

#[test]
fn dropped_transactions_are_counted_and_limits_hold() {
    let s = store(StoreLimits {
        max_active_txns: 2,
        max_txn_writes: 3,
        ..small()
    });
    let mut t = s.begin().unwrap();
    for i in 0..3 {
        s.insert(&mut t, "t", &row(i, "w")).unwrap();
    }
    assert_eq!(
        s.insert(&mut t, "t", &row(9, "w")).unwrap_err().kind,
        StoreErrorKind::Limit
    );
    let u = s.begin().unwrap();
    assert_eq!(s.begin().unwrap_err().kind, StoreErrorKind::Limit);
    drop(u);
    assert_eq!(s.leaked_txns(), 1);
    s.commit(t).unwrap();
    assert!(
        StoreLimits {
            btree_fanout: 2,
            ..small()
        }
        .pool_frames
            > 0
    );
    assert_eq!(
        Store::open(
            SimDevice::new(),
            StoreLimits {
                btree_fanout: 2,
                ..small()
            }
        )
        .unwrap_err()
        .kind,
        StoreErrorKind::Limit
    );
}

#[test]
fn file_device_store_round_trips() {
    let dir = std::env::temp_dir().join(format!(
        "vesper-quarry-store-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    {
        let (mut s, _) = Store::open(FsDevice::open(&dir).unwrap(), small()).unwrap();
        s.execute_ddl(DDL).unwrap();
        let mut t = s.begin().unwrap();
        for i in 0..50 {
            s.insert(&mut t, "t", &row(i, "fs")).unwrap();
        }
        s.commit(t).unwrap();
        // Crash: no close.
        drop(s.into_device());
    }
    let (s, rep) = Store::open(FsDevice::open(&dir).unwrap(), small()).unwrap();
    assert!(rep.groups >= 2);
    let t = s.begin().unwrap();
    assert_eq!(s.scan(&t, "t").unwrap().len(), 50);
    s.commit(t).unwrap();
    drop(s);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A vacuum that collects deleted rows' tombstones and old values runs in
/// several bounded groups. A crash at every device operation of it, under
/// each crash choice, recovers the same logical contents: no deleted row
/// comes back from a committed prefix of the groups.
#[test]
fn vacuum_crashes_never_resurrect_deleted_rows() {
    let limits = StoreLimits {
        max_group_pages: 8,
        ..small()
    };
    // Built with the default group size; only the vacuum runs with 8 pages.
    let s = store(small());
    let mut t = s.begin().unwrap();
    let ids: Vec<u64> = (0..12)
        .map(|i| s.insert(&mut t, "t", &row(i, "orig")).unwrap())
        .collect();
    s.commit(t).unwrap();
    // An open snapshot keeps the old values and the tombstones past commit
    // time, so the vacuum below collects them.
    let old = s.begin().unwrap();
    let mut t = s.begin().unwrap();
    for (i, &id) in ids.iter().enumerate() {
        if i % 2 == 0 {
            s.update(&mut t, "t", id, &row(i as u64, "new")).unwrap();
        } else {
            s.delete(&mut t, "t", id).unwrap();
        }
    }
    s.commit(t).unwrap();
    s.abort(old).unwrap();
    let r = s.begin().unwrap();
    let expect = s.scan(&r, "t").unwrap();
    s.abort(r).unwrap();
    assert_eq!(expect.len(), 6);
    let base = s.into_device();
    let start = base.stats().ops;
    let (mut s, _) = Store::open(base.clone(), limits).unwrap();
    let opened = s.device().stats().ops - start;
    let rep = s.vacuum().unwrap();
    assert!(rep.groups >= 3 && rep.versions_removed >= 12, "{rep:?}");
    let total = s.device().stats().ops - start;
    let mut recoveries = 0;
    for n in opened..total {
        let mut dev = base.clone();
        dev.crash_after(n);
        let (mut s, _) = Store::open(dev, limits).unwrap();
        assert!(s.vacuum().is_err(), "crash point {n} is inside the vacuum");
        let crashed = s.into_device();
        for choice in [
            CrashChoice::LoseUnsynced,
            CrashChoice::KeepUnsynced,
            CrashChoice::Subset { seed: n },
        ] {
            let (mut s, _) = Store::open(crashed.crash(choice), limits).unwrap();
            let r = s.begin().unwrap();
            assert_eq!(
                s.scan(&r, "t").unwrap(),
                expect,
                "crash at vacuum operation {n}, {choice:?}"
            );
            s.abort(r).unwrap();
            s.check().unwrap();
            recoveries += 1;
        }
    }
    assert!(recoveries >= 30, "{recoveries} recoveries");
}

/// `close` during a running fuzzy checkpoint keeps a deferred commit whose
/// pages the checkpoint already wrote back.
#[test]
fn close_keeps_deferred_commits_during_a_running_checkpoint() {
    // A pool large enough that no eviction flushes the log on the way.
    let limits = StoreLimits {
        btree_fanout: 3,
        leaf_max_versions: 4,
        ..StoreLimits::default()
    };
    let mut s = store(limits);
    let mut t = s.begin().unwrap();
    let ids: Vec<u64> = (0..40)
        .map(|i| s.insert(&mut t, "t", &row(i, "orig")).unwrap())
        .collect();
    s.commit(t).unwrap();
    s.checkpoint_begin().unwrap();
    s.checkpoint_step(3).unwrap();
    let mut t = s.begin().unwrap();
    s.update(&mut t, "t", ids[1], &row(1, "late")).unwrap();
    assert!(!s.commit_deferred(t).unwrap().durable);
    let dev = s.close().unwrap();
    let (s, _) = Store::open(dev.crash(CrashChoice::LoseUnsynced), limits).unwrap();
    let r = s.begin().unwrap();
    assert_eq!(s.get(&r, "t", ids[1]).unwrap(), Some(row(1, "late")));
    assert_eq!(s.scan(&r, "t").unwrap().len(), 40);
    s.abort(r).unwrap();
}
