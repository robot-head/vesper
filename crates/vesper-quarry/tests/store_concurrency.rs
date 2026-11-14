//! Stress harness for concurrency inside one `Store`: reader threads walk
//! the latch-coupled B+Tree while writer threads commit groups that split,
//! merge and rebalance leaves (fanout 3, four versions per leaf) and evict
//! frames (eight-frame pool). Every reader's snapshot must equal the state
//! the writer recorded for that commit timestamp, the conserved total must
//! hold in every snapshot, and conflicting writers must never lose an
//! update. This is a randomized stress test over real threads, not a
//! model check of every interleaving (`loom` is not a dependency).
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vesper_quarry::device::SimDevice;
use vesper_quarry::store::{Store, StoreErrorKind, StoreLimits};
use vesper_quarry::Value;

const TOTAL: u64 = 1_000_000;

fn limits() -> StoreLimits {
    StoreLimits {
        pool_frames: 8,
        btree_fanout: 3,
        leaf_max_versions: 4,
        checkpoint_step_pages: 2,
        log_region_bytes: 2 << 20,
        ..StoreLimits::default()
    }
}
fn open() -> Store<SimDevice> {
    let (mut s, _) = Store::open(SimDevice::new(), limits()).unwrap();
    s.execute_ddl("CREATE TABLE acct (owner U64 NOT NULL, balance U64 NOT NULL, note TEXT NULL)")
        .unwrap();
    s
}
fn acct(owner: u64, balance: u64) -> Vec<Value> {
    vec![
        Value::U64(owner),
        Value::U64(balance),
        Value::Text(format!("owner {owner}")),
    ]
}
fn balance(row: &[Value]) -> u64 {
    match row[1] {
        Value::U64(b) => b,
        ref v => panic!("balance {v:?}"),
    }
}
/// xorshift64*: deterministic per thread.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}
type State = BTreeMap<u64, Vec<Value>>;

/// One writer thread transfers, inserts and deletes (conserving the total)
/// while four readers check every snapshot against the recorded history.
#[test]
fn readers_see_exact_snapshots_while_a_writer_splits_and_merges() {
    let s = open();
    let mut state: State = BTreeMap::new();
    let mut t = s.begin().unwrap();
    for i in 0..40 {
        let row = acct(i, TOTAL / 40);
        let id = s.insert(&mut t, "acct", &row).unwrap();
        state.insert(id, row);
    }
    let ts0 = s.commit(t).unwrap().ts;
    let history: Mutex<BTreeMap<u64, State>> = Mutex::new(BTreeMap::from([(ts0, state.clone())]));
    let done = AtomicBool::new(false);
    let checked = AtomicU64::new(0);
    let deadline = Instant::now() + Duration::from_secs(6);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            // Readers stop even if this thread panics.
            struct Done<'a>(&'a AtomicBool);
            impl Drop for Done<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _done = Done(&done);
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
            let mut commits = 0;
            let mut deferred = false;
            while commits < 400 && Instant::now() < deadline {
                // A deferred commit is invisible to new snapshots until
                // the log is synced; the writer's next transaction must
                // see it (first committer wins otherwise).
                if deferred {
                    s.flush().unwrap();
                }
                let mut t = s.begin().unwrap();
                let ids: Vec<u64> = state.keys().copied().collect();
                let pick = |rng: &mut Rng| ids[rng.below(ids.len() as u64) as usize];
                match rng.below(10) {
                    // Insert: a new account funded by an existing one.
                    0..=2 if state.len() < 120 => {
                        let from = pick(&mut rng);
                        let fb = balance(&state[&from]);
                        let amount = fb / 2;
                        let mut f = state[&from].clone();
                        f[1] = Value::U64(fb - amount);
                        s.update(&mut t, "acct", from, &f).unwrap();
                        let row = acct(rng.below(1 << 20), amount);
                        let id = s.insert(&mut t, "acct", &row).unwrap();
                        state.insert(from, f);
                        state.insert(id, row);
                    }
                    // Delete: an account's balance moves to another.
                    3..=5 if state.len() > 8 => {
                        let gone = pick(&mut rng);
                        let mut to = pick(&mut rng);
                        while to == gone {
                            to = pick(&mut rng);
                        }
                        let mut r = state[&to].clone();
                        r[1] = Value::U64(balance(&r) + balance(&state[&gone]));
                        s.delete(&mut t, "acct", gone).unwrap();
                        s.update(&mut t, "acct", to, &r).unwrap();
                        state.remove(&gone);
                        state.insert(to, r);
                    }
                    // Transfer.
                    _ => {
                        let (a, b) = (pick(&mut rng), pick(&mut rng));
                        if a == b {
                            s.abort(t).unwrap();
                            continue;
                        }
                        let amount = rng.below(balance(&state[&a]) + 1);
                        let mut ra = state[&a].clone();
                        let mut rb = state[&b].clone();
                        ra[1] = Value::U64(balance(&ra) - amount);
                        rb[1] = Value::U64(balance(&rb) + amount);
                        s.update(&mut t, "acct", a, &ra).unwrap();
                        s.update(&mut t, "acct", b, &rb).unwrap();
                        state.insert(a, ra);
                        state.insert(b, rb);
                    }
                }
                // Deferred commits make visibility lag installation.
                deferred = rng.below(3) == 0;
                let c = if deferred {
                    s.commit_deferred(t).unwrap()
                } else {
                    s.commit(t).unwrap()
                };
                history.lock().unwrap().insert(c.ts, state.clone());
                commits += 1;
            }
            s.flush().unwrap();
        });
        for r in 0..4u64 {
            let (s, history, done, checked) = (&s, &history, &done, &checked);
            scope.spawn(move || {
                let mut rng = Rng(0x1234_5678 + r);
                while !done.load(Ordering::SeqCst) {
                    let t = s.begin().unwrap();
                    let rows = s.scan(&t, "acct").unwrap();
                    let total: u64 = rows.iter().map(|(_, row)| balance(row)).sum();
                    assert_eq!(
                        total,
                        TOTAL,
                        "snapshot {} conserves the total",
                        t.snapshot()
                    );
                    // Point reads through the same tree agree with the scan.
                    for _ in 0..4 {
                        let (id, row) = &rows[rng.below(rows.len() as u64) as usize];
                        assert_eq!(s.get(&t, "acct", *id).unwrap().as_ref(), Some(row));
                    }
                    let snap = t.snapshot();
                    s.commit(t).unwrap();
                    // The writer records a commit right after it returns.
                    let expected = loop {
                        if let Some(st) = history.lock().unwrap().get(&snap) {
                            break st.clone();
                        }
                        std::thread::yield_now();
                    };
                    let got: State = rows.into_iter().collect();
                    assert_eq!(got, expected, "snapshot {snap} equals the committed state");
                    checked.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    let mut s = s;
    let shapes = s.check().unwrap();
    let st = s.stats();
    assert_eq!(st.pinned, 0);
    assert_eq!(st.leaked_pins, 0);
    assert!(
        checked.load(Ordering::Relaxed) > 0,
        "readers checked snapshots"
    );
    assert!(
        st.evictions > 0,
        "the eight-frame pool evicted while readers ran"
    );
    assert!(
        shapes["acct"].height >= 3,
        "a deep tree: {:?}",
        shapes["acct"]
    );
    eprintln!(
        "{} reader snapshots checked; {:?}; {} evictions, {} checkpoints",
        checked.load(Ordering::Relaxed),
        shapes["acct"],
        st.evictions,
        st.checkpoints_done
    );
}

/// Writers on several threads race on the same rows: no-wait locks and
/// first-committer-wins turn every race into a typed `Conflict`, retried
/// by the caller, and no transfer is lost.
#[test]
fn conflicting_writer_threads_never_lose_an_update() {
    let s = open();
    let mut t = s.begin().unwrap();
    let ids: Vec<u64> = (0..6)
        .map(|i| s.insert(&mut t, "acct", &acct(i, 1000)).unwrap())
        .collect();
    s.commit(t).unwrap();
    let conflicts = AtomicU64::new(0);
    let applied = AtomicU64::new(0);
    std::thread::scope(|scope| {
        for w in 0..4u64 {
            let (s, ids, conflicts, applied) = (&s, &ids, &conflicts, &applied);
            scope.spawn(move || {
                let mut rng = Rng(0xabcd + w);
                let mut done = 0;
                while done < 60 {
                    let a = ids[rng.below(6) as usize];
                    let mut b = ids[rng.below(6) as usize];
                    if a == b {
                        b = ids[(ids.iter().position(|&x| x == a).unwrap() + 1) % 6];
                    }
                    let mut t = s.begin().unwrap();
                    let r = (|| {
                        let mut ra = s.get(&t, "acct", a)?.expect("row");
                        let mut rb = s.get(&t, "acct", b)?.expect("row");
                        let amount = balance(&ra).min(7);
                        ra[1] = Value::U64(balance(&ra) - amount);
                        rb[1] = Value::U64(balance(&rb) + amount);
                        s.update(&mut t, "acct", a, &ra)?;
                        s.update(&mut t, "acct", b, &rb)?;
                        Ok(())
                    })();
                    match r {
                        Ok(()) => {
                            s.commit(t).unwrap();
                            applied.fetch_add(1, Ordering::Relaxed);
                            done += 1;
                        }
                        Err(e) => {
                            let e: vesper_quarry::store::StoreError = e;
                            assert_eq!(e.kind, StoreErrorKind::Conflict, "{e}");
                            conflicts.fetch_add(1, Ordering::Relaxed);
                            s.abort(t).unwrap();
                        }
                    }
                }
            });
        }
    });
    let t = s.begin().unwrap();
    let total: u64 = s
        .scan(&t, "acct")
        .unwrap()
        .iter()
        .map(|(_, r)| balance(r))
        .sum();
    s.commit(t).unwrap();
    assert_eq!(total, 6000, "no lost update");
    assert_eq!(applied.load(Ordering::Relaxed), 240);
    let st = s.stats();
    assert_eq!((st.locks, st.active_txns, st.pinned), (0, 0, 0));
    eprintln!("{} conflicts retried", conflicts.load(Ordering::Relaxed));
}
