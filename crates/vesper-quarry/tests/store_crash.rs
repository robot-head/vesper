//! Crash recovery of the table store under the `SimDevice` crash model.
//!
//! A seeded workload (two interleaved transactions, inserts/updates/deletes
//! of rows up to 1.2 KiB over two tables with a secondary index each, synced and deferred commits,
//! aborts, write conflicts, fuzzy checkpoints started and stepped while
//! commits continue, vacuum, a small pool forcing evictions, a fanout-3
//! B+Tree forcing splits and leaf removal, and small log regions forcing
//! region switches) is rerun crashing at every device operation, and at
//! every WAL record boundary of every log write. Each crash is recovered
//! and must equal the committed state at some timestamp between the last
//! acknowledged-durable commit and the last appended one, with valid trees
//! and an exact page partition, indexes that hold exactly their tables'
//! value prefixes, and index lookups equal to filtered scans.
use std::collections::BTreeMap;
use vesper_quarry::device::{CrashChoice, FileId, SimDevice};
use vesper_quarry::store::{Store, StoreErrorKind, StoreLimits, StoreRecovery, Txn};
use vesper_quarry::Value;

type State = BTreeMap<(String, u64), Vec<Value>>;

fn limits() -> StoreLimits {
    StoreLimits {
        pool_frames: 6,
        log_region_bytes: 512 << 10,
        btree_fanout: 3,
        leaf_max_versions: 4,
        checkpoint_step_pages: 2,
        max_group_pages: 256,
        ..StoreLimits::default()
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// What the workload observed before it stopped.
#[derive(Clone, Debug, Default)]
struct Observed {
    /// Committed logical state after each commit timestamp (0: empty).
    states: BTreeMap<u64, State>,
    /// Newest timestamp acknowledged durable.
    acked: u64,
    /// Newest timestamp appended to the log.
    appended: u64,
}

fn random_row(rng: &mut Rng, table: &str) -> Vec<Value> {
    if table == "a" {
        let len = [0usize, 5, 40, 300, 1200][rng.below(5) as usize];
        let text: String = (0..len)
            .map(|i| (b'a' + ((i as u64 + rng.0) % 26) as u8) as char)
            .collect();
        vec![
            Value::U64(rng.next()),
            if rng.below(4) == 0 {
                Value::Null
            } else {
                Value::Text(text)
            },
            if rng.below(3) == 0 {
                Value::Null
            } else {
                Value::F64((rng.below(1_000_000) as f64) / 7.0)
            },
        ]
    } else {
        vec![
            Value::I64(rng.next() as i64),
            if rng.below(3) == 0 {
                Value::Null
            } else {
                Value::Bool(rng.below(2) == 1)
            },
        ]
    }
}

struct Active {
    txn: Txn,
    view: State,
    writes: State,
    deletes: Vec<(String, u64)>,
}

/// Runs the workload; stops at the first `Crashed`/`Io` error.
fn workload(dev: &mut SimDevice, seed: u64, steps: usize, obs: &mut Observed) {
    obs.states.insert(0, State::new());
    let Ok((mut s, _)) = Store::open(&mut *dev, limits()) else {
        return;
    };
    if s.execute_ddl(
        "CREATE TABLE a (k U64 NOT NULL, v TEXT NULL, f F64 NULL); \
             CREATE TABLE b (k I64 NOT NULL, flag BOOL NULL)",
    )
    .is_err()
    {
        return;
    }
    // Secondary indexes are maintained in the same commit groups.
    if s.create_index("a_v", "a", "v").is_err() || s.create_index("b_k", "b", "k").is_err() {
        return;
    }
    let mut rng = Rng(seed);
    let mut active: Vec<Active> = Vec::new();
    let latest = |obs: &Observed| obs.states.values().next_back().cloned().unwrap_or_default();
    for _ in 0..steps {
        let r: Result<(), vesper_quarry::store::StoreError> = (|| {
            match rng.below(100) {
                0..=14 if active.len() < 2 => {
                    let txn = s.begin()?;
                    let view = obs.states[&txn.snapshot()].clone();
                    active.push(Active {
                        txn,
                        view,
                        writes: State::new(),
                        deletes: Vec::new(),
                    });
                }
                0..=49 if !active.is_empty() => {
                    let i = rng.below(active.len() as u64) as usize;
                    let a = &mut active[i];
                    let table = if rng.below(3) == 0 { "b" } else { "a" };
                    let visible: Vec<u64> = a
                        .view
                        .keys()
                        .filter(|(t, _)| t == table)
                        .map(|(_, r)| *r)
                        .collect();
                    let op = rng.below(10);
                    if op < 5 || visible.is_empty() {
                        let row = random_row(&mut rng, table);
                        let id = s.insert(&mut a.txn, table, &row)?;
                        a.view.insert((table.into(), id), row.clone());
                        a.writes.insert((table.into(), id), row);
                    } else {
                        let id = visible[rng.below(visible.len() as u64) as usize];
                        let key = (table.to_string(), id);
                        if op < 8 {
                            let row = random_row(&mut rng, table);
                            match s.update(&mut a.txn, table, id, &row) {
                                Ok(()) => {
                                    a.view.insert(key.clone(), row.clone());
                                    a.writes.insert(key, row);
                                }
                                Err(e) if e.kind == StoreErrorKind::Conflict => {}
                                Err(e) => return Err(e),
                            }
                        } else {
                            match s.delete(&mut a.txn, table, id) {
                                Ok(()) => {
                                    a.view.remove(&key);
                                    a.writes.remove(&key);
                                    a.deletes.push(key);
                                }
                                Err(e) if e.kind == StoreErrorKind::Conflict => {}
                                Err(e) => return Err(e),
                            }
                        }
                    }
                }
                50..=69 if !active.is_empty() => {
                    let a = active.remove(rng.below(active.len() as u64) as usize);
                    let deferred = rng.below(3) == 0;
                    let empty = a.writes.is_empty() && a.deletes.is_empty();
                    let mut next = latest(obs);
                    for k in &a.deletes {
                        next.remove(k);
                    }
                    next.extend(a.writes);
                    let r = if deferred {
                        s.commit_deferred(a.txn)
                    } else {
                        s.commit(a.txn)
                    };
                    let c = match r {
                        Ok(c) => c,
                        Err(e) => {
                            // In flight: it may or may not survive.
                            if !empty {
                                obs.appended += 1;
                                obs.states.insert(obs.appended, next);
                            }
                            return Err(e);
                        }
                    };
                    if !empty {
                        assert_eq!(c.ts, obs.appended + 1);
                        obs.states.insert(c.ts, next);
                        obs.appended = c.ts;
                    }
                    if c.durable && !empty {
                        obs.acked = c.ts;
                    }
                }
                70..=74 if !active.is_empty() => {
                    let a = active.remove(0);
                    s.abort(a.txn)?;
                }
                75..=79 => {
                    s.flush()?;
                    obs.acked = obs.appended;
                }
                80..=86 => {
                    if s.stats().checkpoint_running {
                        s.checkpoint_step(1 + rng.below(3) as usize)?;
                    } else {
                        s.checkpoint_begin()?;
                    }
                }
                87..=89 => {
                    s.vacuum()?;
                    // Vacuum flushes the log.
                    obs.acked = obs.appended;
                }
                _ => {}
            }
            Ok(())
        })();
        if let Err(e) = r {
            assert!(
                matches!(e.kind, StoreErrorKind::Crashed),
                "unexpected workload error {e}"
            );
            for a in active {
                std::mem::forget(a.txn);
            }
            return;
        }
    }
    for a in active.drain(..) {
        let _ = s.abort(a.txn);
    }
    assert_eq!(s.stats().pinned, 0);
}

fn contents(s: &mut Store<&mut SimDevice>) -> State {
    let t = s.begin().unwrap();
    let mut out = State::new();
    for table in ["a", "b"] {
        if s.columns(table).is_err() {
            continue;
        }
        for (id, row) in s.scan(&t, table).unwrap() {
            out.insert((table.to_string(), id), row);
        }
    }
    s.commit(t).unwrap();
    out
}

/// Recovers `medium` and checks it against `obs`. Returns the report.
fn recover_and_check(medium: &mut SimDevice, obs: &Observed, what: &str) -> StoreRecovery {
    let (mut s, report) = Store::open(&mut *medium, limits())
        .unwrap_or_else(|e| panic!("{what}: recovery failed: {e}"));
    s.check()
        .unwrap_or_else(|e| panic!("{what}: invalid store: {e}"));
    let ts = s.stats().last_ts;
    assert!(
        ts >= obs.acked && ts <= obs.appended.max(obs.acked),
        "{what}: recovered ts {ts} outside [{}, {}]",
        obs.acked,
        obs.appended
    );
    let got = contents(&mut s);
    assert_eq!(&got, &obs.states[&ts], "{what}: contents at ts {ts}");
    // Index lookups over the whole domain equal filtered scans.
    let t = s.begin().unwrap();
    let ix = s.indexes();
    for (name, table, col, lo, hi) in [
        (
            "a_v",
            "a",
            1,
            Value::Text(String::new()),
            Value::Text("{".into()),
        ),
        ("b_k", "b", 0, Value::I64(i64::MIN), Value::I64(i64::MAX)),
    ] {
        if !ix.contains_key(name) {
            continue;
        }
        let want: Vec<_> = s
            .scan(&t, table)
            .unwrap()
            .into_iter()
            .filter(|(_, r)| r[col] != Value::Null)
            .collect();
        assert_eq!(
            s.lookup_range(&t, name, &lo, &hi).unwrap(),
            want,
            "{what}: {name}"
        );
    }
    s.commit(t).unwrap();
    assert_eq!(s.stats().pinned, 0);
    drop(s);
    report
}

#[derive(Default)]
struct Coverage {
    points: u64,
    recoveries: u64,
    boundaries: u64,
    images: u64,
    redone: u64,
    incomplete: u64,
    previous_region: u64,
    tails: u64,
}
impl Coverage {
    fn add(&mut self, r: &StoreRecovery) {
        self.recoveries += 1;
        self.images += u64::from(r.images_restored > 0);
        self.redone += u64::from(r.redone > 0);
        self.incomplete += u64::from(r.incomplete > 0);
        self.previous_region += u64::from(r.previous_region);
        self.tails += u64::from(r.tail.is_some());
    }
}

/// Frame boundaries inside a log write (which always starts at a frame).
fn frame_boundaries(data: &[u8]) -> Vec<usize> {
    let mut out = vec![0];
    let mut at = 0;
    while at + 16 <= data.len() {
        let len = u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        at += 16 + len;
        if at > data.len() {
            break;
        }
        out.push(at);
    }
    out
}

fn enumerate(seed: u64, steps: usize, cov: &mut Coverage) {
    let mut full = SimDevice::new();
    let mut obs = Observed::default();
    workload(&mut full, seed, steps, &mut obs);
    let total = full.stats().ops;
    eprintln!(
        "seed {seed}: {} ops, appended ts {}, acked {}",
        total, obs.appended, obs.acked
    );
    assert!(obs.appended > 5, "seed {seed} committed too little");
    for n in 0..total {
        let mut dev = SimDevice::new();
        dev.crash_after(n);
        let mut obs = Observed::default();
        workload(&mut dev, seed, steps, &mut obs);
        cov.points += 1;
        for (i, choice) in [
            CrashChoice::LoseUnsynced,
            CrashChoice::KeepUnsynced,
            CrashChoice::Subset { seed: n },
            CrashChoice::Subset { seed: n ^ 0xabcdef },
        ]
        .into_iter()
        .enumerate()
        {
            let mut medium = dev.crash(choice);
            let r = recover_and_check(&mut medium, &obs, &format!("seed {seed} op {n} choice {i}"));
            cov.add(&r);
        }
        if let Some((_, data)) = dev.last_pending_write(FileId::Wal) {
            if data.len() >= 16 {
                for b in frame_boundaries(data) {
                    let mut medium = dev.crash_tearing(FileId::Wal, b);
                    let r = recover_and_check(
                        &mut medium,
                        &obs,
                        &format!("seed {seed} op {n} WAL cut at frame boundary {b}"),
                    );
                    cov.add(&r);
                    cov.boundaries += 1;
                }
            }
        }
    }
}

#[test]
fn every_crash_point_and_wal_record_boundary_recovers_a_committed_prefix() {
    let mut cov = Coverage::default();
    for seed in [11, 12] {
        enumerate(seed, 300, &mut cov);
    }
    eprintln!(
        "crash points {}, recoveries {}, WAL frame boundaries {}, with images restored {}, \
         redo {}, incomplete groups {}, previous region {}, log tails {}",
        cov.points,
        cov.recoveries,
        cov.boundaries,
        cov.images,
        cov.redone,
        cov.incomplete,
        cov.previous_region,
        cov.tails
    );
    assert!(cov.boundaries > 50);
    assert!(cov.images > 0 && cov.redone > 0 && cov.incomplete > 0 && cov.previous_region > 0);
}

#[test]
fn crashes_during_recovery_recover_the_same_state() {
    let seed = 13;
    let mut full = SimDevice::new();
    let mut obs = Observed::default();
    workload(&mut full, seed, 60, &mut obs);
    let total = full.stats().ops;
    let mut checked = 0;
    for n in [total / 3, total / 2, total - 2] {
        let mut dev = SimDevice::new();
        dev.crash_after(n);
        let mut obs = Observed::default();
        workload(&mut dev, seed, 60, &mut obs);
        let crashed = dev.crash(CrashChoice::KeepUnsynced);
        // Count recovery's own device operations, then crash at each.
        let mut probe = crashed.clone();
        let before = probe.stats().ops;
        drop(Store::open(&mut probe, limits()).unwrap());
        let ops = probe.stats().ops - before;
        let mut reference = crashed.clone();
        let (mut s, _) = Store::open(&mut reference, limits()).unwrap();
        let expect = contents(&mut s);
        drop(s);
        for k in 0..ops {
            for choice in [CrashChoice::LoseUnsynced, CrashChoice::Subset { seed: k }] {
                let mut medium = crashed.clone();
                medium.crash_after(k);
                assert!(Store::open(&mut medium, limits()).is_err());
                let mut again = medium.crash(choice);
                let (mut s, _) = Store::open(&mut again, limits())
                    .unwrap_or_else(|e| panic!("op {n} recovery crash {k}: {e}"));
                s.check().unwrap();
                assert_eq!(contents(&mut s), expect, "op {n} recovery crash {k}");
                checked += 1;
            }
        }
    }
    assert!(checked > 20);
}

#[test]
fn creation_crashes_at_every_operation_leave_a_usable_store() {
    for n in 0..40 {
        let mut dev = SimDevice::new();
        dev.crash_after(n);
        let mut obs = Observed::default();
        workload(&mut dev, 99, 3, &mut obs);
        for choice in [
            CrashChoice::LoseUnsynced,
            CrashChoice::KeepUnsynced,
            CrashChoice::Subset { seed: n },
        ] {
            let mut medium = dev.crash(choice);
            let (mut s, _) =
                Store::open(&mut medium, limits()).unwrap_or_else(|e| panic!("op {n}: {e}"));
            s.check().unwrap();
            // Either both tables exist (the DDL group survived) or none.
            let a = s.columns("a").is_ok();
            assert_eq!(a, s.columns("b").is_ok(), "op {n}: DDL group is atomic");
            if !a {
                s.execute_ddl("CREATE TABLE a (k U64 NOT NULL, v TEXT NULL, f F64 NULL)")
                    .unwrap();
            }
            let mut t = s.begin().unwrap();
            s.insert(&mut t, "a", &[Value::U64(1), Value::Null, Value::Null])
                .unwrap();
            s.commit(t).unwrap();
        }
    }
}
