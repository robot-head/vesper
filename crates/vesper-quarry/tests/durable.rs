use std::collections::BTreeMap;
use vesper_quarry::device::{CrashChoice, Device, FileId, FsDevice, SimDevice};
use vesper_quarry::durable::{Db, DbErrorKind, DbLimits, RecordId, RecoveryReport, Txn, MAX_VALUE};
use vesper_quarry::page::PAGE_SIZE;
use vesper_quarry::wal::{TailDefect, HEADER_BYTES};

fn limits(frames: usize) -> DbLimits {
    DbLimits {
        pool_frames: frames,
        ..DbLimits::default()
    }
}
fn contents<D: Device>(db: &mut Db<D>) -> BTreeMap<RecordId, Vec<u8>> {
    db.scan().unwrap().into_iter().collect()
}

#[test]
fn committed_changes_survive_reopen_and_aborts_roll_back() {
    let mut dev = SimDevice::new();
    let (mut db, report) = Db::open(&mut dev, limits(4)).unwrap();
    assert!(report.created);
    let mut t = db.begin().unwrap();
    let a = db.insert(&mut t, b"alpha").unwrap();
    let b = db.insert(&mut t, b"beta").unwrap();
    assert_eq!(db.get(Some(&t), a).unwrap().as_deref(), Some(&b"alpha"[..]));
    assert_eq!(db.get(None, a).unwrap_err().kind, DbErrorKind::Conflict);
    db.commit(t).unwrap();

    let mut t = db.begin().unwrap();
    db.update(&mut t, a, b"ALPHA, much longer than before")
        .unwrap();
    db.delete(&mut t, b).unwrap();
    let c = db.insert(&mut t, b"gamma").unwrap();
    db.abort(t).unwrap();
    assert_eq!(db.get(None, a).unwrap().as_deref(), Some(&b"alpha"[..]));
    assert_eq!(db.get(None, b).unwrap().as_deref(), Some(&b"beta"[..]));
    assert_eq!(
        db.get(None, c).unwrap(),
        None,
        "aborted insert is a tombstone"
    );

    let mut t = db.begin().unwrap();
    db.delete(&mut t, b).unwrap();
    assert_eq!(
        db.delete(&mut t, b).unwrap_err().kind,
        DbErrorKind::NotFound
    );
    db.commit(t).unwrap();
    assert_eq!(db.stats().pinned, 0);
    assert_eq!(db.leaked_txns(), 0);
    db.close().unwrap();

    let (mut db, report) = Db::open(&mut dev, limits(4)).unwrap();
    assert!(!report.created);
    assert_eq!(
        report.records, 0,
        "close checkpointed and truncated the log"
    );
    assert_eq!(
        contents(&mut db),
        BTreeMap::from([(a, b"alpha".to_vec())]),
        "only the committed state remains"
    );
}

#[test]
fn recovery_redoes_committed_and_undoes_uncommitted_after_crash() {
    let mut dev = SimDevice::new();
    let (mut db, _) = Db::open(&mut dev, limits(2)).unwrap();
    let mut keep = Vec::new();
    let mut t = db.begin().unwrap();
    for i in 0..40u8 {
        keep.push(db.insert(&mut t, &vec![i; 1500]).unwrap());
    }
    db.commit(t).unwrap();
    let mut loser = db.begin().unwrap();
    for id in &keep[..10] {
        db.update(&mut loser, *id, b"uncommitted").unwrap();
    }
    db.insert(&mut loser, b"uncommitted insert").unwrap();
    // Another transaction's commit syncs the shared log, loser records too.
    let mut other = db.begin().unwrap();
    let survivor = db.insert(&mut other, b"other").unwrap();
    db.commit(other).unwrap();
    // With two frames the loser's dirty pages were stolen (written) already.
    assert!(db.stats().page_writes > 0);
    let evictions = db.stats().evictions;
    assert!(evictions > 0);
    std::mem::forget(loser); // simulate the process dying with it active
    drop(db);
    let mut crashed = dev.crash(CrashChoice::KeepUnsynced);
    let (mut db, report) = Db::open(&mut crashed, limits(2)).unwrap();
    assert_eq!(report.committed.len(), 2);
    assert_eq!(report.rolled_back.len(), 1);
    assert_eq!(report.compensations, 11);
    let got = contents(&mut db);
    assert_eq!(got.len(), 41);
    assert_eq!(got[&survivor], b"other");
    for (i, id) in keep.iter().enumerate() {
        assert_eq!(got[id], vec![i as u8; 1500]);
    }
}

#[test]
fn a_dropped_transaction_is_counted_and_blocks_checkpoint() {
    let mut dev = SimDevice::new();
    let (mut db, _) = Db::open(&mut dev, limits(2)).unwrap();
    let mut t = db.begin().unwrap();
    db.insert(&mut t, b"x").unwrap();
    drop(t);
    assert_eq!(db.leaked_txns(), 1);
    let mut other = db.begin().unwrap();
    db.insert(&mut other, b"y").unwrap();
    db.commit(other).unwrap();
    assert_eq!(db.checkpoint().unwrap_err().kind, DbErrorKind::State);
    assert_eq!(db.scan().unwrap_err().kind, DbErrorKind::Conflict);
    drop(db);
    let (mut db, report) = Db::open(&mut dev, limits(2)).unwrap();
    assert_eq!(report.rolled_back, vec![1]);
    let left: Vec<Vec<u8>> = contents(&mut db).into_values().collect();
    assert_eq!(left, vec![b"y".to_vec()]);
}

#[test]
fn page_locks_conflict_without_waiting_and_handles_are_checked() {
    let mut dev = SimDevice::new();
    let (mut db, _) = Db::open(&mut dev, limits(4)).unwrap();
    let mut a = db.begin().unwrap();
    let mut b = db.begin().unwrap();
    let ra = db.insert(&mut a, b"a").unwrap();
    // b cannot use a's page; it gets a fresh one.
    let rb = db.insert(&mut b, b"b").unwrap();
    assert_ne!(ra.page, rb.page);
    assert_eq!(
        db.update(&mut b, ra, b"z").unwrap_err().kind,
        DbErrorKind::Conflict
    );
    assert_eq!(
        db.get(Some(&b), ra).unwrap_err().kind,
        DbErrorKind::Conflict
    );
    db.commit(a).unwrap();
    db.update(&mut b, ra, b"z").unwrap();
    db.commit(b).unwrap();
    assert_eq!(db.get(None, ra).unwrap().as_deref(), Some(&b"z"[..]));

    let mut other_dev = SimDevice::new();
    let (mut other, _) = Db::open(&mut other_dev, limits(2)).unwrap();
    let mut foreign: Txn = other.begin().unwrap();
    assert_eq!(
        db.insert(&mut foreign, b"q").unwrap_err().kind,
        DbErrorKind::WrongStore
    );
    other.abort(foreign).unwrap();
    let mut t = db.begin().unwrap();
    assert_eq!(
        db.insert(&mut t, &vec![0; MAX_VALUE + 1]).unwrap_err().kind,
        DbErrorKind::Limit
    );
    let big = db.insert(&mut t, &vec![7; MAX_VALUE]).unwrap();
    assert_eq!(db.get(Some(&t), big).unwrap().unwrap().len(), MAX_VALUE);
    db.commit(t).unwrap();
    assert_eq!(db.stats().pinned, 0);
}

#[test]
fn log_limit_reserves_room_for_rollback() {
    let mut dev = SimDevice::new();
    let small = DbLimits {
        pool_frames: 4,
        max_wal_bytes: 64 << 10,
        ..DbLimits::default()
    };
    let (mut db, _) = Db::open(&mut dev, small).unwrap();
    let mut t = db.begin().unwrap();
    let mut n = 0;
    let err = loop {
        match db.insert(&mut t, &[1; 2000]) {
            Ok(_) => n += 1,
            Err(e) => break e,
        }
    };
    assert_eq!(err.kind, DbErrorKind::LogFull);
    assert!(n > 10);
    // The reservation guarantees the rollback fits.
    db.abort(t).unwrap();
    assert!(db.stats().wal_bytes <= 64 << 10);
    db.checkpoint().unwrap();
    assert_eq!(db.stats().wal_bytes, 0);
    let mut t = db.begin().unwrap();
    db.insert(&mut t, &[2; 2000]).unwrap();
    db.commit(t).unwrap();
}

#[test]
fn page_checksum_corruption_is_rejected_and_pins_are_conserved() {
    let mut dev = SimDevice::new();
    let (mut db, _) = Db::open(&mut dev, limits(2)).unwrap();
    let mut t = db.begin().unwrap();
    for i in 0..30u8 {
        db.insert(&mut t, &vec![i; 1000]).unwrap();
    }
    db.commit(t).unwrap();
    db.close().unwrap();
    // One flipped bit in page 1's record area, after a clean shutdown.
    dev.corrupt(FileId::Data, PAGE_SIZE as u64 + 9000, 0x10);
    let (mut db, report) = Db::open(&mut dev, limits(2)).unwrap();
    assert_eq!(report.records, 0);
    let err = db.scan().unwrap_err();
    assert_eq!(err.kind, DbErrorKind::Corrupt);
    assert!(err.message.contains("page 1"), "{}", err.message);
    assert_eq!(db.stats().pinned, 0);
    assert!(db.poisoned());
    assert_eq!(db.begin().unwrap_err().kind, DbErrorKind::State);
}

#[test]
fn corrupt_page_without_a_logged_image_fails_recovery() {
    let mut dev = SimDevice::new();
    let (mut db, _) = Db::open(&mut dev, limits(2)).unwrap();
    let mut t = db.begin().unwrap();
    let id = db.insert(&mut t, b"v1").unwrap();
    db.commit(t).unwrap();
    db.checkpoint().unwrap();
    let mut t = db.begin().unwrap();
    db.update(&mut t, id, b"v2").unwrap();
    db.commit(t).unwrap();
    drop(db);
    // The page image logged after the checkpoint repairs a torn page ...
    let mut torn = dev.crash(CrashChoice::LoseUnsynced);
    torn.corrupt(FileId::Data, 100, 0xff);
    let (mut db, report) = Db::open(&mut torn, limits(2)).unwrap();
    assert_eq!(report.images_restored, 1);
    assert_eq!(db.get(None, id).unwrap().as_deref(), Some(&b"v2"[..]));
    drop(db);
    // ... but rot in a page the log no longer covers is reported, not hidden.
    torn.corrupt(FileId::Data, 100, 0xff);
    let (mut db, report) = Db::open(&mut torn, limits(2)).unwrap();
    assert_eq!(report.records, 0);
    assert_eq!(db.get(None, id).unwrap_err().kind, DbErrorKind::Corrupt);
    assert_eq!(db.stats().pinned, 0);
}

#[test]
fn torn_log_tail_is_cut_and_reported() {
    let mut dev = SimDevice::new();
    let (mut db, _) = Db::open(&mut dev, limits(4)).unwrap();
    let mut t = db.begin().unwrap();
    let id = db.insert(&mut t, b"durable").unwrap();
    db.commit(t).unwrap();
    let len = dev_len(&mut db);
    let mut t = db.begin().unwrap();
    db.insert(&mut t, b"never acknowledged").unwrap();
    db.commit(t).unwrap();
    drop(db);
    let full = dev.len(FileId::Wal).unwrap();
    assert!(full > len);
    // Cut the second commit's frames in the middle.
    let mut torn = dev.crash(CrashChoice::LoseUnsynced);
    torn.set_len(FileId::Wal, len + (full - len) / 2).unwrap();
    torn.sync(FileId::Wal).unwrap();
    let (mut db, report) = Db::open(&mut torn, limits(4)).unwrap();
    assert!(matches!(
        report.tail,
        Some(TailDefect::TornPayload | TailDefect::TornHeader)
    ));
    assert_eq!(
        contents(&mut db),
        BTreeMap::from([(id, b"durable".to_vec())])
    );
    drop(db);
    assert_eq!(torn.len(FileId::Wal).unwrap(), HEADER_BYTES);
}
fn dev_len(db: &mut Db<&mut SimDevice>) -> u64 {
    (**db.device()).clone().len(FileId::Wal).unwrap()
}

#[test]
fn destroyed_log_headers_with_data_present_are_corruption() {
    let mut dev = SimDevice::new();
    let (mut db, _) = Db::open(&mut dev, limits(2)).unwrap();
    let mut t = db.begin().unwrap();
    db.insert(&mut t, b"x").unwrap();
    db.commit(t).unwrap();
    db.close().unwrap();
    dev.corrupt(FileId::Wal, 3, 1);
    dev.corrupt(FileId::Wal, 35, 1);
    let err = Db::open(&mut dev, limits(2)).unwrap_err();
    assert_eq!(err.kind, DbErrorKind::Corrupt);
}

#[test]
fn real_files_round_trip() {
    // Bazel provides TEST_TMPDIR; Cargo runs use the OS temp directory.
    let tmp = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, Into::into);
    let dir = tmp.join(format!(
        "vesper-quarry-durable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut ids = Vec::new();
    {
        let (mut db, report) = Db::open(FsDevice::open(&dir).unwrap(), limits(2)).unwrap();
        assert!(report.created);
        let mut t = db.begin().unwrap();
        for i in 0..20u8 {
            ids.push(db.insert(&mut t, &vec![i; 3000]).unwrap());
        }
        db.commit(t).unwrap();
        let mut t = db.begin().unwrap();
        db.update(&mut t, ids[3], b"three").unwrap();
        db.commit(t).unwrap();
        let mut t = db.begin().unwrap();
        db.update(&mut t, ids[4], b"lost").unwrap();
        let mut other = db.begin().unwrap();
        db.update(&mut other, ids[19], b"nineteen").unwrap();
        db.commit(other).unwrap();
        // No close: the process "dies" with t active and the log not truncated.
        std::mem::forget(t);
    }
    let (mut db, report) = Db::open(FsDevice::open(&dir).unwrap(), limits(2)).unwrap();
    assert_eq!(report.committed.len(), 3);
    assert_eq!(report.rolled_back.len(), 1);
    let got = contents(&mut db);
    assert_eq!(got[&ids[3]], b"three");
    assert_eq!(got[&ids[19]], b"nineteen");
    assert_eq!(got[&ids[4]], vec![4; 3000]);
    db.close().unwrap();
    assert_eq!(
        std::fs::metadata(dir.join("quarry.wal")).unwrap().len(),
        HEADER_BYTES
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- crash-point enumeration --------------------------------------------

type Model = BTreeMap<u64, Vec<u8>>;

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
fn value(key: u64, version: u64, len: usize) -> Vec<u8> {
    let mut v = key.to_le_bytes().to_vec();
    v.extend((0..len).map(|i| (key as usize * 31 + version as usize * 7 + i) as u8));
    v
}
fn decode(scan: Vec<(RecordId, Vec<u8>)>) -> Model {
    scan.into_iter()
        .map(|(_, v)| (u64::from_le_bytes(v[..8].try_into().unwrap()), v))
        .collect()
}

struct Open {
    txn: Txn,
    writes: BTreeMap<u64, Option<(RecordId, Vec<u8>)>>,
}
/// States after 0, 1, 2, ... committed transactions, and whether the last
/// commit was in flight (attempted but not acknowledged) at the crash.
struct Outcome {
    states: Vec<Model>,
    in_doubt: Option<Model>,
}

/// A deterministic workload: up to two interleaved transactions doing
/// inserts, updates and deletes of 1..3 KiB records, commits, aborts and
/// checkpoints over a three-frame pool (so dirty uncommitted pages are
/// stolen). It stops at the first crash error.
fn workload(dev: &mut SimDevice, seed: u64, steps: usize) -> Outcome {
    let mut rng = Rng(seed);
    let mut out = Outcome {
        states: vec![Model::new()],
        in_doubt: None,
    };
    let Ok((mut db, _)) = Db::open(&mut *dev, limits(3)) else {
        return out;
    };
    let mut committed: BTreeMap<u64, (RecordId, Vec<u8>)> = BTreeMap::new();
    let mut open: [Option<Open>; 2] = [None, None];
    let mut next_key = 1u64;
    let crashed = |e: &vesper_quarry::durable::DbError| e.kind == DbErrorKind::Crashed;
    macro_rules! check {
        ($r:expr) => {
            match $r {
                Ok(x) => Some(x),
                // Open transaction owners are dropped (counted as leaked).
                Err(e) if crashed(&e) => return out,
                // Lock conflicts and an update too large for its page are skipped.
                Err(e) if matches!(e.kind, DbErrorKind::Conflict | DbErrorKind::Full) => None,
                Err(e) => panic!("unexpected {e}"),
            }
        };
    }
    for step in 0..steps {
        let s = (step + rng.below(2) as usize) % 2;
        let Some(cur) = open[s].as_mut() else {
            if rng.below(8) == 0 && open.iter().all(Option::is_none) {
                check!(db.checkpoint());
            } else if let Some(txn) = check!(db.begin()) {
                open[s] = Some(Open {
                    txn,
                    writes: BTreeMap::new(),
                });
            }
            continue;
        };
        let visible = |key: u64| -> Option<RecordId> {
            match cur.writes.get(&key) {
                Some(Some((id, _))) => Some(*id),
                Some(None) => None,
                None => committed.get(&key).map(|(id, _)| *id),
            }
        };
        match rng.below(10) {
            0..=3 => {
                let key = next_key;
                next_key += 1;
                let v = value(key, 0, 1 + rng.below(3000) as usize);
                if let Some(id) = check!(db.insert(&mut cur.txn, &v)) {
                    cur.writes.insert(key, Some((id, v)));
                }
            }
            4..=6 => {
                let keys: Vec<u64> = committed
                    .keys()
                    .chain(cur.writes.keys())
                    .copied()
                    .filter(|&k| visible(k).is_some())
                    .collect();
                if keys.is_empty() {
                    continue;
                }
                let key = keys[rng.below(keys.len() as u64) as usize];
                let id = visible(key).unwrap();
                if rng.below(3) == 0 {
                    if check!(db.delete(&mut cur.txn, id)).is_some() {
                        cur.writes.insert(key, None);
                    }
                } else {
                    let v = value(key, step as u64, 1 + rng.below(2500) as usize);
                    if check!(db.update(&mut cur.txn, id, &v)).is_some() {
                        cur.writes.insert(key, Some((id, v)));
                    }
                }
            }
            7..=8 => {
                let Open { txn, writes } = open[s].take().unwrap();
                let mut next = committed.clone();
                for (k, w) in writes {
                    match w {
                        Some(x) => next.insert(k, x),
                        None => next.remove(&k),
                    };
                }
                let model: Model = next.iter().map(|(k, (_, v))| (*k, v.clone())).collect();
                match db.commit(txn) {
                    Ok(_) => {
                        committed = next;
                        out.states.push(model);
                    }
                    Err(e) if crashed(&e) => {
                        out.in_doubt = Some(model);
                        return out;
                    }
                    Err(e) => panic!("commit failed: {e}"),
                }
            }
            _ => {
                let Open { txn, .. } = open[s].take().unwrap();
                check!(db.abort(txn));
            }
        }
    }
    for t in open.into_iter().flatten() {
        check!(db.abort(t.txn));
    }
    out
}

fn verify(crashed: &SimDevice, out: &Outcome, label: &str) -> RecoveryReport {
    let mut dev = crashed.clone();
    let (mut db, report) =
        Db::open(&mut dev, limits(3)).unwrap_or_else(|e| panic!("{label}: recovery failed: {e}"));
    let got = decode(db.scan().unwrap());
    let acked = out.states.last().unwrap();
    let ok = &got == acked || out.in_doubt.as_ref() == Some(&got);
    assert!(
        ok,
        "{label}: recovered {} records, acknowledged prefix has {} (report {report:?})",
        got.len(),
        acked.len()
    );
    assert_eq!(db.stats().pinned, 0);
    assert_eq!(db.stats().active_txns, 0);
    assert_eq!(db.stats().wal_bytes, 0, "recovery ends with a checkpoint");
    report
}

fn crash_points(seed: u64, steps: usize) -> (u64, usize) {
    let mut full = SimDevice::new();
    let out = workload(&mut full, seed, steps);
    assert!(
        out.states.len() > 3,
        "workload commits several transactions"
    );
    eprintln!("seed {seed}: {} commits", out.states.len() - 1);
    let total = full.stats().ops;
    let mut checked = 0;
    let mut seen = [0usize; 5];
    for k in 0..=total {
        let mut dev = SimDevice::new();
        dev.crash_after(k);
        let out = workload(&mut dev, seed, steps);
        let choices = [
            CrashChoice::LoseUnsynced,
            CrashChoice::KeepUnsynced,
            CrashChoice::Subset { seed: k },
            CrashChoice::Subset { seed: !k },
        ];
        for choice in choices {
            let r = verify(
                &dev.crash(choice),
                &out,
                &format!("seed {seed} op {k} {choice:?}"),
            );
            checked += 1;
            for (n, hit) in seen.iter_mut().zip([
                r.images_restored > 0,
                r.redone > 0,
                !r.rolled_back.is_empty(),
                r.compensations > 0,
                r.tail.is_some(),
            ]) {
                *n += usize::from(hit);
            }
        }
    }
    // The enumeration exercised torn-page repair, redo, loser rollback
    // (including resumed rollbacks) and torn log tails.
    assert!(
        seen.iter().all(|&n| n > 0),
        "seed {seed}: coverage {seen:?}"
    );
    eprintln!("seed {seed}: [images, redo, losers, compensations, torn tails] = {seen:?}");
    (total, checked)
}

#[test]
fn every_crash_point_recovers_a_committed_prefix() {
    for seed in [1, 2] {
        let (total, checked) = crash_points(seed, 240);
        assert!(total > 100, "seed {seed}: only {total} device operations");
        assert_eq!(checked as u64, (total + 1) * 4);
        eprintln!("seed {seed}: {total} crash points, {checked} recoveries checked");
    }
}

#[test]
fn crashes_during_recovery_recover_the_same_prefix() {
    let seed = 3;
    let mut full = SimDevice::new();
    let complete = workload(&mut full, seed, 240);
    let total = full.stats().ops;
    // A few crash points spread over the workload, each with torn writes.
    for k in [total / 3, total / 2, total - 2] {
        let mut dev = SimDevice::new();
        dev.crash_after(k);
        let out = workload(&mut dev, seed, 240);
        let crashed = dev.crash(CrashChoice::Subset { seed: k });
        let mut probe = crashed.clone();
        drop(Db::open(&mut probe, limits(3)).unwrap());
        let recovery_ops = probe.stats().ops;
        assert!(recovery_ops > 0);
        for j in 0..recovery_ops {
            let mut again = crashed.clone();
            again.crash_after(j);
            assert_eq!(
                Db::open(&mut again, limits(3)).unwrap_err().kind,
                DbErrorKind::Crashed
            );
            for choice in [CrashChoice::KeepUnsynced, CrashChoice::Subset { seed: j }] {
                verify(
                    &again.crash(choice),
                    &out,
                    &format!("op {k}, recovery op {j} {choice:?}"),
                );
            }
        }
    }
    assert!(complete.states.len() > 3);
}
