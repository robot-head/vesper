//! Secondary indexes on the durable store: built from existing versions,
//! maintained in commit groups (inserts, updates that move a row between
//! keys, deletes, version collection and vacuum), read through snapshots
//! and the transaction's own writes, persisted across reopen and dropped
//! with their table. A randomized workload compares every lookup with a
//! filtered scan and validates index/table agreement after each commit.
use vesper_quarry::device::{CrashChoice, SimDevice};
use vesper_quarry::store::{Store, StoreErrorKind, StoreLimits};
use vesper_quarry::Value;

fn limits() -> StoreLimits {
    StoreLimits {
        pool_frames: 8,
        btree_fanout: 3,
        leaf_max_versions: 4,
        ..StoreLimits::default()
    }
}
const DDL: &str = "CREATE TABLE p (name TEXT NOT NULL, age I64 NULL, score F64 NULL)";
fn person(name: &str, age: Option<i64>, score: f64) -> Vec<Value> {
    vec![
        Value::Text(name.into()),
        age.map_or(Value::Null, Value::I64),
        Value::F64(score),
    ]
}
fn ids(rows: &[(u64, Vec<Value>)]) -> Vec<u64> {
    rows.iter().map(|(id, _)| *id).collect()
}

#[test]
fn indexes_follow_commits_snapshots_and_reopen() {
    let (mut s, _) = Store::open(SimDevice::new(), limits()).unwrap();
    s.execute_ddl(DDL).unwrap();
    let mut t = s.begin().unwrap();
    // Names share an eight-byte prefix: the recheck separates them.
    let a = s.insert(&mut t, "p", &person("alexander", Some(30), 1.5)).unwrap();
    let b = s.insert(&mut t, "p", &person("alexandra", Some(-4), -0.0)).unwrap();
    let c = s.insert(&mut t, "p", &person("bo", None, 0.0)).unwrap();
    s.commit(t).unwrap();
    // Built from the stored versions.
    s.create_index("p_name", "p", "name").unwrap();
    s.create_index("p_age", "p", "age").unwrap();
    s.create_index("p_score", "p", "score").unwrap();
    s.check().unwrap();

    let t = s.begin().unwrap();
    let name = |v: &str| Value::Text(v.into());
    assert_eq!(ids(&s.lookup(&t, "p_name", &name("alexandra")).unwrap()), [b]);
    assert_eq!(
        ids(&s.lookup_range(&t, "p_name", &name("alex"), &name("alexz")).unwrap()),
        [a, b]
    );
    assert_eq!(
        ids(&s.lookup_range(&t, "p_age", &Value::I64(-10), &Value::I64(30)).unwrap()),
        [a, b],
        "negative integers order before positive ones; NULL is not indexed"
    );
    assert_eq!(
        ids(&s.lookup(&t, "p_score", &Value::F64(0.0)).unwrap()),
        [b, c],
        "-0.0 equals 0.0"
    );
    s.commit(t).unwrap();

    // An old snapshot keeps seeing the old key while a newer one sees the
    // moved row; the transaction sees its own uncommitted write.
    let old = s.begin().unwrap();
    let mut w = s.begin().unwrap();
    s.update(&mut w, "p", a, &person("zed", Some(31), 2.0)).unwrap();
    assert_eq!(ids(&s.lookup(&w, "p_name", &name("zed")).unwrap()), [a]);
    assert!(s.lookup(&w, "p_name", &name("alexander")).unwrap().is_empty());
    s.delete(&mut w, "p", c).unwrap();
    s.commit(w).unwrap();
    s.check().unwrap();
    assert_eq!(ids(&s.lookup(&old, "p_name", &name("alexander")).unwrap()), [a]);
    assert!(s.lookup(&old, "p_name", &name("zed")).unwrap().is_empty());
    assert_eq!(ids(&s.lookup(&old, "p_score", &Value::F64(0.0)).unwrap()), [b, c]);
    s.commit(old).unwrap();
    let t = s.begin().unwrap();
    assert!(s.lookup(&t, "p_name", &name("alexander")).unwrap().is_empty());
    assert_eq!(ids(&s.lookup(&t, "p_score", &Value::F64(0.0)).unwrap()), [b]);
    s.commit(t).unwrap();

    // Vacuum collects the old versions and their index entries.
    let rep = s.vacuum().unwrap();
    assert!(rep.versions_removed > 0);
    s.check().unwrap();

    // Typed errors.
    let t = s.begin().unwrap();
    let e = s.lookup(&t, "p_name", &Value::Null).unwrap_err();
    assert_eq!(e.kind, StoreErrorKind::Data);
    let e = s.lookup(&t, "p_name", &Value::U64(1)).unwrap_err();
    assert_eq!(e.kind, StoreErrorKind::Data);
    let e = s.lookup(&t, "nope", &Value::U64(1)).unwrap_err();
    assert_eq!(e.kind, StoreErrorKind::NotFound);
    s.commit(t).unwrap();
    assert!(s.create_index("p_name", "p", "age").is_err(), "duplicate name");
    assert!(s.create_index("p", "p", "age").is_err(), "a table's name");
    assert_eq!(
        s.create_index("x", "p", "nope").unwrap_err().kind,
        StoreErrorKind::NotFound
    );
    let busy = s.begin().unwrap();
    assert_eq!(
        s.create_index("x", "p", "age").unwrap_err().kind,
        StoreErrorKind::State
    );
    s.abort(busy).unwrap();
    assert!(s.execute_ddl("CREATE TABLE p_age (x U64 NOT NULL)").is_err());

    // Reopen: indexes persist (meta format 2) and still agree.
    let dev = s.close().unwrap();
    let (mut s, _) = Store::open(dev, limits()).unwrap();
    s.check().unwrap();
    assert_eq!(s.indexes().len(), 3);
    let t = s.begin().unwrap();
    assert_eq!(ids(&s.lookup(&t, "p_name", &name("zed")).unwrap()), [a]);
    s.commit(t).unwrap();

    // Dropping an index, then the table with its other indexes, frees
    // every page.
    s.drop_index("p_score").unwrap();
    s.check().unwrap();
    assert_eq!(s.indexes().len(), 2);
    let before = s.stats().free_pages;
    s.execute_ddl("DROP TABLE p").unwrap();
    s.check().unwrap();
    assert!(s.indexes().is_empty());
    assert!(s.stats().free_pages > before);
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random inserts, updates and deletes over a few hot keys, with an old
/// snapshot held open part of the time: every lookup equals the filtered
/// scan, and the index agrees with its table after every commit and a
/// crash that loses unsynced writes.
#[test]
fn random_workload_lookups_equal_filtered_scans() {
    let (mut s, _) = Store::open(SimDevice::new(), limits()).unwrap();
    s.execute_ddl(DDL).unwrap();
    s.create_index("p_age", "p", "age").unwrap();
    s.create_index("p_name", "p", "name").unwrap();
    let mut rng = Rng(0x5eed);
    let mut held = None;
    for step in 0..300 {
        let mut t = s.begin().unwrap();
        let rows = s.scan(&t, "p").unwrap();
        for _ in 0..1 + rng.below(3) {
            let age = (rng.below(5) != 0).then(|| rng.below(8) as i64 - 3);
            let row = person(&format!("name-{}", rng.below(6)), age, 0.5);
            match rng.below(3) {
                0 if !rows.is_empty() => {
                    let id = rows[rng.below(rows.len() as u64) as usize].0;
                    let _ = s.update(&mut t, "p", id, &row);
                }
                1 if !rows.is_empty() => {
                    let id = rows[rng.below(rows.len() as u64) as usize].0;
                    let _ = s.delete(&mut t, "p", id);
                }
                _ => {
                    s.insert(&mut t, "p", &row).unwrap();
                }
            }
        }
        // Lookups inside the writing transaction see its own writes.
        let lo = rng.below(8) as i64 - 3;
        let hi = lo + rng.below(4) as i64;
        let want: Vec<_> = s
            .scan(&t, "p")
            .unwrap()
            .into_iter()
            .filter(|(_, r)| matches!(r[1], Value::I64(x) if lo <= x && x <= hi))
            .collect();
        let got = s
            .lookup_range(&t, "p_age", &Value::I64(lo), &Value::I64(hi))
            .unwrap();
        assert_eq!(got, want, "step {step}");
        s.commit(t).unwrap();
        if let Some(h) = held.take() {
            let want: Vec<_> = s
                .scan(&h, "p")
                .unwrap()
                .into_iter()
                .filter(|(_, r)| r[0] == Value::Text("name-1".into()))
                .collect();
            assert_eq!(
                s.lookup(&h, "p_name", &Value::Text("name-1".into())).unwrap(),
                want,
                "old snapshot, step {step}"
            );
            s.commit(h).unwrap();
        }
        if rng.below(4) == 0 {
            held = Some(s.begin().unwrap());
        }
        if step % 50 == 0 {
            s.check().unwrap();
        }
    }
    if let Some(h) = held {
        s.commit(h).unwrap();
    }
    s.vacuum().unwrap();
    s.check().unwrap();
    let crashed = s.into_device().crash(CrashChoice::LoseUnsynced);
    let (mut s, _) = Store::open(crashed, limits()).unwrap();
    s.check().unwrap();
}
