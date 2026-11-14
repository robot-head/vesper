//! Serializable snapshot isolation: the classic write-skew anomalies that
//! snapshot isolation permits are rejected with a typed `Serialization`
//! error for serializable transactions: on-call doctors (row reads and
//! table scans), the phantom variant through an index range, and Fekete,
//! O'Neil and O'Neil's read-only anomaly. Disjoint serializable work
//! commits, mixed levels keep SI semantics, and a threaded stress test
//! keeps an invariant that write skew would break.
use std::sync::atomic::{AtomicU64, Ordering};
use vesper_quarry::device::SimDevice;
use vesper_quarry::store::{Isolation, Store, StoreError, StoreErrorKind, StoreLimits, Txn};
use vesper_quarry::Value;

fn open(ddl: &str) -> Store<SimDevice> {
    let (mut s, _) = Store::open(
        SimDevice::new(),
        StoreLimits {
            btree_fanout: 3,
            leaf_max_versions: 4,
            ..StoreLimits::default()
        },
    )
    .unwrap();
    s.execute_ddl(ddl).unwrap();
    s
}
const DOCTORS: &str = "CREATE TABLE doc (name TEXT NOT NULL, on_call BOOL NOT NULL)";
fn doc(name: &str, on: bool) -> Vec<Value> {
    vec![Value::Text(name.into()), Value::Bool(on)]
}
fn doctors() -> (Store<SimDevice>, u64, u64) {
    let s = open(DOCTORS);
    let mut t = s.begin().unwrap();
    let alice = s.insert(&mut t, "doc", &doc("alice", true)).unwrap();
    let bob = s.insert(&mut t, "doc", &doc("bob", true)).unwrap();
    s.commit(t).unwrap();
    (s, alice, bob)
}
fn ser(s: &Store<SimDevice>) -> Txn {
    s.begin_with(Isolation::Serializable).unwrap()
}
fn on_call_by_get(s: &Store<SimDevice>, t: &Txn, ids: [u64; 2]) -> usize {
    ids.iter()
        .filter(|&&id| s.get(t, "doc", id).unwrap().unwrap()[1] == Value::Bool(true))
        .count()
}
fn on_call_by_scan(s: &Store<SimDevice>, t: &Txn) -> usize {
    s.scan(t, "doc")
        .unwrap()
        .iter()
        .filter(|(_, r)| r[1] == Value::Bool(true))
        .count()
}
fn is_serialization(e: &StoreError) -> bool {
    e.kind == StoreErrorKind::Serialization
}

#[test]
fn write_skew_on_call_doctors_is_rejected() {
    for scan in [false, true] {
        let (s, alice, bob) = doctors();
        let count = |t: &Txn| {
            if scan {
                on_call_by_scan(&s, t)
            } else {
                on_call_by_get(&s, t, [alice, bob])
            }
        };
        // Snapshot isolation: both see two doctors on call, each takes a
        // different one off, both commit: nobody is on call.
        let mut t1 = s.begin().unwrap();
        let mut t2 = s.begin().unwrap();
        assert_eq!((count(&t1), count(&t2)), (2, 2));
        s.update(&mut t1, "doc", alice, &doc("alice", false)).unwrap();
        s.update(&mut t2, "doc", bob, &doc("bob", false)).unwrap();
        s.commit(t1).unwrap();
        s.commit(t2).unwrap();
        let t = s.begin().unwrap();
        assert_eq!(count(&t), 0, "snapshot isolation permits write skew");
        s.commit(t).unwrap();

        // Serializable: the second committer is the pivot and aborts.
        let (s, alice, bob) = doctors();
        let count = |t: &Txn| {
            if scan {
                on_call_by_scan(&s, t)
            } else {
                on_call_by_get(&s, t, [alice, bob])
            }
        };
        let mut t1 = ser(&s);
        let mut t2 = ser(&s);
        assert_eq!((count(&t1), count(&t2)), (2, 2));
        s.update(&mut t1, "doc", alice, &doc("alice", false)).unwrap();
        s.update(&mut t2, "doc", bob, &doc("bob", false)).unwrap();
        s.commit(t1).unwrap();
        let e = s.commit(t2).unwrap_err();
        assert!(is_serialization(&e), "scan={scan}: {e}");
        let t = s.begin().unwrap();
        assert_eq!(count(&t), 1, "one doctor stays on call");
        s.commit(t).unwrap();
        let st = s.stats();
        assert_eq!((st.locks, st.active_txns, st.ssi_retained), (0, 0, 0));
    }
}

#[test]
fn phantom_write_skew_through_an_index_range_is_rejected() {
    let mut s = open("CREATE TABLE booking (room U64 NOT NULL, hour U64 NOT NULL)");
    s.create_index("booking_hour", "booking", "hour").unwrap();
    // Each transaction books room 1 at 10:00 only if no booking exists
    // between 9 and 11; the predicate reads an empty range (a phantom).
    let book = |s: &Store<SimDevice>, t: &mut Txn| {
        let busy = s
            .lookup_range(t, "booking_hour", &Value::U64(9), &Value::U64(11))
            .unwrap();
        assert!(busy.is_empty());
        s.insert(t, "booking", &[Value::U64(1), Value::U64(10)])
            .unwrap();
    };
    let mut t1 = ser(&s);
    let mut t2 = ser(&s);
    book(&s, &mut t1);
    book(&s, &mut t2);
    s.commit(t1).unwrap();
    assert!(is_serialization(&s.commit(t2).unwrap_err()));
    // A booking outside the range does not conflict.
    let mut t3 = ser(&s);
    let mut t4 = ser(&s);
    let n = s
        .lookup_range(&t3, "booking_hour", &Value::U64(14), &Value::U64(15))
        .unwrap();
    assert!(n.is_empty());
    s.insert(&mut t3, "booking", &[Value::U64(2), Value::U64(14)])
        .unwrap();
    let busy = s
        .lookup_range(&t4, "booking_hour", &Value::U64(20), &Value::U64(21))
        .unwrap();
    assert!(busy.is_empty());
    s.insert(&mut t4, "booking", &[Value::U64(3), Value::U64(20)])
        .unwrap();
    s.commit(t3).unwrap();
    s.commit(t4).unwrap();
}

/// Fekete, O'Neil and O'Neil (SIGMOD Record 2004): T2 withdraws from
/// checking with a penalty when checking + savings would go negative, T1
/// deposits into savings, and a read-only T3 sees T1's deposit but not
/// T2's withdrawal: no serial order explains T3's view. T3 ->rw T2 ->rw T1
/// makes T2 a pivot.
#[test]
fn read_only_anomaly_is_rejected() {
    let s = open("CREATE TABLE acct (kind TEXT NOT NULL, bal I64 NOT NULL)");
    let mut t = s.begin().unwrap();
    let x = s
        .insert(&mut t, "acct", &[Value::Text("checking".into()), Value::I64(0)])
        .unwrap();
    let y = s
        .insert(&mut t, "acct", &[Value::Text("savings".into()), Value::I64(0)])
        .unwrap();
    s.commit(t).unwrap();
    let bal = |t: &Txn, id| match s.get(t, "acct", id).unwrap().unwrap()[1] {
        Value::I64(v) => v,
        _ => unreachable!(),
    };
    let mut t2 = ser(&s);
    assert_eq!(bal(&t2, x) + bal(&t2, y), 0);
    let mut t1 = ser(&s);
    let y1 = bal(&t1, y);
    s.update(&mut t1, "acct", y, &[Value::Text("savings".into()), Value::I64(y1 + 20)])
        .unwrap();
    s.commit(t1).unwrap();
    let t3 = ser(&s);
    assert_eq!((bal(&t3, x), bal(&t3, y)), (0, 20));
    s.commit(t3).unwrap();
    // T2 withdraws 10 and, seeing a total of 0, charges a penalty of 1.
    s.update(&mut t2, "acct", x, &[Value::Text("checking".into()), Value::I64(-11)])
        .unwrap();
    assert!(is_serialization(&s.commit(t2).unwrap_err()));
}

#[test]
fn disjoint_serializable_work_commits_and_levels_mix() {
    let (s, alice, bob) = doctors();
    // Each reads and writes only its own row: no dangerous structure.
    let mut t1 = ser(&s);
    let mut t2 = ser(&s);
    assert!(s.get(&t1, "doc", alice).unwrap().is_some());
    assert!(s.get(&t2, "doc", bob).unwrap().is_some());
    s.update(&mut t1, "doc", alice, &doc("alice", false)).unwrap();
    s.update(&mut t2, "doc", bob, &doc("bob", false)).unwrap();
    s.commit(t1).unwrap();
    s.commit(t2).unwrap();
    // A snapshot-isolation reader does not take part in the graph.
    let (s, alice, bob) = doctors();
    let mut t1 = s.begin().unwrap();
    let mut t2 = ser(&s);
    assert_eq!(on_call_by_get(&s, &t1, [alice, bob]), 2);
    assert_eq!(on_call_by_get(&s, &t2, [alice, bob]), 2);
    s.update(&mut t1, "doc", alice, &doc("alice", false)).unwrap();
    s.update(&mut t2, "doc", bob, &doc("bob", false)).unwrap();
    s.commit(t1).unwrap();
    s.commit(t2).unwrap();
    // A plain write-write conflict stays a Conflict.
    let (s, alice, _) = doctors();
    let mut t1 = ser(&s);
    let mut t2 = ser(&s);
    s.update(&mut t1, "doc", alice, &doc("alice", false)).unwrap();
    let e = s.update(&mut t2, "doc", alice, &doc("alice", false)).unwrap_err();
    assert_eq!(e.kind, StoreErrorKind::Conflict);
    s.commit(t1).unwrap();
    s.abort(t2).unwrap();
    assert_eq!(s.stats().ssi_retained, 0);
}

/// Four threads keep "at least one doctor on call" while taking doctors
/// off and putting them back, each deciding from a scan. Write skew would
/// break the invariant; serializable transactions retry on Conflict and
/// Serialization errors and every committed snapshot keeps it.
#[test]
fn threads_keep_an_invariant_that_write_skew_breaks() {
    let s = open(DOCTORS);
    let mut t = s.begin().unwrap();
    for i in 0..3 {
        s.insert(&mut t, "doc", &doc(&format!("d{i}"), true)).unwrap();
    }
    s.commit(t).unwrap();
    let (committed, retried) = (AtomicU64::new(0), AtomicU64::new(0));
    std::thread::scope(|scope| {
        for w in 0..4u64 {
            let (s, committed, retried) = (&s, &committed, &retried);
            scope.spawn(move || {
                let mut x = 0x9e37_79b9 ^ w;
                let mut done = 0;
                while done < 40 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let mut t = s.begin_with(Isolation::Serializable).unwrap();
                    let r = (|| -> Result<(), StoreError> {
                        let rows = s.scan(&t, "doc")?;
                        let on: Vec<_> = rows.iter().filter(|(_, r)| r[1] == Value::Bool(true)).collect();
                        assert!(!on.is_empty(), "invariant in a snapshot");
                        let (id, row) = &rows[(x % rows.len() as u64) as usize];
                        let flip = row[1] == Value::Bool(false) || on.len() >= 2;
                        if flip {
                            let mut r = row.clone();
                            r[1] = Value::Bool(row[1] != Value::Bool(true));
                            s.update(&mut t, "doc", *id, &r)?;
                        }
                        Ok(())
                    })();
                    let r = match r {
                        Ok(()) => s.commit(t).map(drop),
                        Err(e) => {
                            s.abort(t).unwrap();
                            Err(e)
                        }
                    };
                    match r {
                        Ok(()) => {
                            committed.fetch_add(1, Ordering::Relaxed);
                            done += 1;
                        }
                        Err(e) => {
                            assert!(
                                matches!(
                                    e.kind,
                                    StoreErrorKind::Conflict | StoreErrorKind::Serialization
                                ),
                                "{e}"
                            );
                            retried.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });
    let t = s.begin().unwrap();
    assert!(on_call_by_scan(&s, &t) >= 1);
    s.commit(t).unwrap();
    assert_eq!(committed.load(Ordering::Relaxed), 160);
    let st = s.stats();
    assert_eq!((st.locks, st.active_txns, st.ssi_retained), (0, 0, 0));
    eprintln!("{} retried", retried.load(Ordering::Relaxed));
}
