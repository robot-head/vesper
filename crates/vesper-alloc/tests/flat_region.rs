//! Independent affine ownership, bounded backing and watermark lifecycle checks.
use std::{
    alloc::Layout,
    cell::{Cell, RefCell},
    hash::{Hash, Hasher},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
    rc::Rc,
};
use vesper_alloc::{
    AllocError, ChunkBacking, FlatConfig, FlatError, HandleError, Region, RegionConfig,
    RegionError, RegionFlatTable, SystemBacking,
};

#[derive(Default)]
struct Ledger {
    attempts: Cell<usize>,
    allocated: Cell<usize>,
    deallocated: Cell<usize>,
    live: Cell<usize>,
    bytes: Cell<usize>,
    fail: Cell<bool>,
}
struct Counting(Rc<Ledger>);
// SAFETY: successful allocation/deallocation forwards the identical pointer and
// Layout to SystemBacking. The wrapper adds accounting and a None refusal only.
unsafe impl ChunkBacking for Counting {
    fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        self.0.attempts.set(self.0.attempts.get() + 1);
        if self.0.fail.get() {
            return None;
        }
        let pointer = SystemBacking.allocate(layout)?;
        self.0.allocated.set(self.0.allocated.get() + 1);
        self.0.live.set(self.0.live.get() + 1);
        self.0.bytes.set(self.0.bytes.get() + layout.size());
        Some(pointer)
    }
    unsafe fn deallocate(&mut self, pointer: NonNull<u8>, layout: Layout) {
        self.0.deallocated.set(self.0.deallocated.get() + 1);
        self.0.live.set(self.0.live.get() - 1);
        self.0.bytes.set(self.0.bytes.get() - layout.size());
        // SAFETY: ChunkBacking's caller returns its live allocation with the
        // original Layout, and this wrapper never frees it elsewhere.
        unsafe { SystemBacking.deallocate(pointer, layout) };
    }
}
fn config(limit: usize) -> RegionConfig {
    RegionConfig {
        initial_chunk_bytes: 64,
        max_chunk_bytes: 64,
        max_reserved_bytes: limit,
        max_align: 4096,
        max_allocations: 1024,
        max_checkpoints: 16,
        retained_bytes: 0,
    }
}
fn region() -> (Region<Counting>, Rc<Ledger>) {
    let ledger = Rc::new(Ledger::default());
    (
        Region::with_backing(config(8 * 1024 * 1024), Counting(ledger.clone())).unwrap(),
        ledger,
    )
}
fn failed<T, E>(result: Result<T, E>) -> E {
    match result {
        Err(error) => error,
        Ok(_) => panic!("unexpected success"),
    }
}
type Log = Rc<RefCell<Vec<u64>>>;
struct Token {
    id: u64,
    log: Log,
}
impl Drop for Token {
    fn drop(&mut self) {
        self.log.borrow_mut().push(self.id);
    }
}
fn token(id: u64, log: &Log) -> Token {
    Token {
        id,
        log: log.clone(),
    }
}
fn sorted(log: &Log) -> Vec<u64> {
    let mut values = log.borrow().clone();
    values.sort_unstable();
    values
}

struct Key {
    number: u64,
    owned: Option<Token>,
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.number == other.number
    }
}
impl Eq for Key {}
impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.number.hash(state)
    }
}

#[test]
fn replacement_and_removal_transfer_actual_affine_pairs_without_early_drops() {
    let (mut r, ledger) = region();
    let log = Log::default();
    let mut table = r.flat_new::<Key, Token>(FlatConfig::default()).unwrap();
    assert!(
        r.flat_insert(
            &mut table,
            Key {
                number: 7,
                owned: Some(token(1, &log))
            },
            token(2, &log)
        )
        .unwrap()
        .is_none()
    );
    let previous = r
        .flat_insert(
            &mut table,
            Key {
                number: 7,
                owned: Some(token(3, &log)),
            },
            token(4, &log),
        )
        .unwrap()
        .unwrap();
    assert_eq!(previous.0.owned.as_ref().unwrap().id, 1);
    assert_eq!(previous.1.id, 2);
    assert!(log.borrow().is_empty());
    assert_eq!(r.flat_len(&table), Ok(1));
    drop(previous);
    assert_eq!(sorted(&log), [1, 2]);
    let removed = r
        .flat_remove(
            &table,
            &Key {
                number: 7,
                owned: None,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(removed.0.owned.as_ref().unwrap().id, 3);
    assert_eq!(removed.1.id, 4);
    assert_eq!(r.flat_len(&table), Ok(0));
    assert_eq!(sorted(&log), [1, 2]);
    r.flat_release(table).unwrap();
    assert_eq!(sorted(&log), [1, 2]);
    drop(removed);
    assert_eq!(sorted(&log), [1, 2, 3, 4]);
    drop(r);
    assert_eq!(ledger.live.get(), 0);
    assert_eq!(ledger.bytes.get(), 0);
    assert_eq!(ledger.allocated.get(), ledger.deallocated.get());
}

#[test]
fn failed_backing_growth_returns_incoming_value_and_keeps_rows_owner_and_stats() {
    let (mut r, ledger) = region();
    let log = Log::default();
    let mut table = r
        .flat_new::<u64, Token>(FlatConfig { max_groups: 4 })
        .unwrap();
    for id in 0..14 {
        assert!(
            r.flat_insert(&mut table, id, token(id, &log))
                .unwrap()
                .is_none()
        );
    }
    let before = r.stats();
    let address = r
        .flat_control_group(&table, 0)
        .unwrap()
        .unwrap()
        .as_ptr()
        .addr();
    let region_id = table.region();
    ledger.fail.set(true);
    let refusal = failed(r.flat_insert(&mut table, 14, token(99, &log)));
    assert!(matches!(
        refusal.error,
        FlatError::Region(RegionError::Alloc(AllocError::OutOfMemory { .. }))
    ));
    assert_eq!(refusal.value.0, 14);
    assert_eq!(refusal.value.1.id, 99);
    assert!(Rc::ptr_eq(&refusal.value.1.log, &log));
    assert_eq!(r.stats(), before);
    assert_eq!(table.region(), region_id);
    assert_eq!(
        r.flat_control_group(&table, 0)
            .unwrap()
            .unwrap()
            .as_ptr()
            .addr(),
        address
    );
    assert!(log.borrow().is_empty());
    assert_eq!(r.flat_len(&table), Ok(14));
    for id in 0..14 {
        assert_eq!(r.flat_get(&table, &id).unwrap().unwrap().id, id);
    }
    assert!(matches!(
        failed(r.flat_reserve(&mut table, 1)),
        FlatError::Region(RegionError::Alloc(AllocError::OutOfMemory { .. }))
    ));
    assert_eq!(r.stats(), before);
    ledger.fail.set(false);
    let (key, value) = refusal.value;
    assert!(r.flat_insert(&mut table, key, value).unwrap().is_none());
    assert_eq!(r.flat_group_count(&table), Ok(2));
    assert_eq!(r.flat_capacity(&table), Ok(28));
    assert_eq!(r.flat_len(&table), Ok(15));
    assert!(log.borrow().is_empty());
    drop(r);
    let mut expected: Vec<_> = (0..14).collect();
    expected.push(99);
    assert_eq!(sorted(&log), expected);
    assert_eq!(ledger.live.get(), 0);
    assert_eq!(ledger.allocated.get(), ledger.deallocated.get());
}

#[test]
fn finite_region_reservation_refusal_and_constructor_refusal_leave_stats_unchanged() {
    let ledger = Rc::new(Ledger::default());
    let mut r = Region::with_backing(config(2048), Counting(ledger.clone())).unwrap();
    ledger.fail.set(true);
    let before = r.stats();
    assert!(matches!(
        failed(r.flat_new::<u64, [u8; 32]>(FlatConfig::default())),
        FlatError::Region(RegionError::Alloc(AllocError::OutOfMemory { .. }))
    ));
    assert_eq!(r.stats(), before);
    ledger.fail.set(false);
    let mut table = r
        .flat_new::<u64, [u8; 32]>(FlatConfig { max_groups: 4 })
        .unwrap();
    for id in 0..14 {
        r.flat_insert(&mut table, id, [id as u8; 32]).unwrap();
    }
    let before = r.stats();
    let attempts = ledger.attempts.get();
    assert!(matches!(
        failed(r.flat_reserve(&mut table, 1)),
        FlatError::Region(RegionError::Alloc(AllocError::CapacityExceeded { .. }))
    ));
    assert_eq!(r.stats(), before);
    assert_eq!(ledger.attempts.get(), attempts);
    assert_eq!(r.flat_len(&table), Ok(14));
    for id in 0..14 {
        assert_eq!(r.flat_get(&table, &id), Ok(Some(&[id as u8; 32])));
    }
}

#[test]
fn premark_in_capacity_mutations_survive_rewind_and_protected_growth_is_atomic() {
    let (mut r, _) = region();
    let mut table = r
        .flat_with_capacity::<u64, u64>(14, FlatConfig { max_groups: 4 })
        .unwrap();
    for id in 0..7 {
        r.flat_insert(&mut table, id, id).unwrap();
    }
    let mark = r.checkpoint().unwrap();
    let scratch = r.alloc_bytes(64, 16).unwrap();
    *r.flat_get_mut(&table, &1).unwrap().unwrap() = 101;
    assert_eq!(r.flat_insert(&mut table, 2, 202).unwrap(), Some((2, 2)));
    assert_eq!(r.flat_remove(&table, &3).unwrap(), Some((3, 3)));
    for id in 7..15 {
        r.flat_insert(&mut table, id, id).unwrap();
    }
    assert_eq!(r.flat_len(&table), Ok(14));
    let before = r.stats();
    let refusal = failed(r.flat_insert(&mut table, 15, 1515));
    assert_eq!(refusal.value, (15, 1515));
    assert_eq!(
        refusal.error,
        FlatError::Region(RegionError::ProtectedByWatermark)
    );
    assert_eq!(
        r.flat_reserve(&mut table, 1),
        Err(FlatError::Region(RegionError::ProtectedByWatermark))
    );
    assert_eq!(r.stats(), before);
    r.flat_reserve(&mut table, 0).unwrap();
    assert_eq!(r.stats(), before);
    assert_eq!(r.flat_insert(&mut table, 0, 100).unwrap(), Some((0, 0)));
    r.rewind(mark).unwrap();
    assert_eq!(r.bytes(&scratch), Err(HandleError::Stale));
    assert_eq!(r.flat_len(&table), Ok(14));
    assert_eq!(r.flat_get(&table, &0), Ok(Some(&100)));
    assert_eq!(r.flat_get(&table, &1), Ok(Some(&101)));
    assert_eq!(r.flat_get(&table, &2), Ok(Some(&202)));
    assert_eq!(r.flat_get(&table, &3), Ok(None));
    r.flat_insert(&mut table, 15, 1515).unwrap();
    assert_eq!(r.flat_group_count(&table), Ok(2));
}

#[test]
fn protected_affine_replacement_keeps_transfers_and_drop_counts_exact() {
    let (mut r, _) = region();
    let log = Log::default();
    let mut table = r.flat_new::<u64, Token>(FlatConfig::default()).unwrap();
    for id in 0..14 {
        r.flat_insert(&mut table, id, token(id, &log)).unwrap();
    }
    let mark = r.checkpoint().unwrap();
    let before = r.stats();
    let refusal = failed(r.flat_insert(&mut table, 14, token(99, &log)));
    assert_eq!(
        refusal.error,
        FlatError::Region(RegionError::ProtectedByWatermark)
    );
    assert_eq!(r.stats(), before);
    assert!(log.borrow().is_empty());
    let replaced = r
        .flat_insert(&mut table, 0, token(100, &log))
        .unwrap()
        .unwrap();
    assert_eq!(replaced.1.id, 0);
    assert!(log.borrow().is_empty());
    drop(replaced);
    drop(refusal.value);
    r.rewind(mark).unwrap();
    assert_eq!(r.flat_get(&table, &0).unwrap().unwrap().id, 100);
    drop(r);
    let mut expected: Vec<_> = (0..14).collect();
    expected.extend([99, 100]);
    assert_eq!(sorted(&log), expected);
}

struct TrapKey {
    number: u64,
    callbacks: Rc<Cell<usize>>,
    panic: Rc<Cell<bool>>,
}
impl Hash for TrapKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.callbacks.set(self.callbacks.get() + 1);
        assert!(!self.panic.get(), "invalid owner hashed");
        self.number.hash(state)
    }
}
impl PartialEq for TrapKey {
    fn eq(&self, other: &Self) -> bool {
        self.callbacks.set(self.callbacks.get() + 1);
        assert!(!self.panic.get(), "invalid owner compared");
        self.number == other.number
    }
}
impl Eq for TrapKey {}
fn invalid_operations(
    r: &mut Region<Counting>,
    table: &mut RegionFlatTable<TrapKey, usize>,
    query: &TrapKey,
    expected: HandleError,
) {
    assert_eq!(r.flat_len(table), Err(expected));
    assert_eq!(r.flat_is_empty(table), Err(expected));
    assert_eq!(r.flat_capacity(table), Err(expected));
    assert_eq!(r.flat_group_count(table), Err(expected));
    assert_eq!(failed(r.flat_get(table, query)), expected);
    assert_eq!(failed(r.flat_get_mut(table, query)), expected);
    assert_eq!(
        failed(r.flat_get_disjoint_mut(table, [query, query])),
        expected
    );
    assert_eq!(failed(r.flat_remove(table, query)), expected);
    assert_eq!(r.flat_clear(table), Err(expected));
    assert_eq!(failed(r.flat_iter(table)), expected);
    assert_eq!(failed(r.flat_control_group(table, usize::MAX)), expected);
    assert_eq!(
        r.flat_reserve(table, 0),
        Err(FlatError::Region(RegionError::Handle(expected)))
    );
    let incoming = TrapKey {
        number: 7,
        callbacks: query.callbacks.clone(),
        panic: query.panic.clone(),
    };
    let refusal = failed(r.flat_insert(table, incoming, 77));
    assert_eq!(
        refusal.error,
        FlatError::Region(RegionError::Handle(expected))
    );
    assert_eq!(refusal.value.0.number, 7);
    assert_eq!(refusal.value.1, 77);
}

#[test]
fn cross_region_and_reset_stale_noops_validate_before_every_hash_or_equality_callback() {
    let (mut a, _) = region();
    let (mut b, _) = region();
    let callbacks = Rc::new(Cell::new(0));
    let panic = Rc::new(Cell::new(true));
    let query = TrapKey {
        number: 7,
        callbacks: callbacks.clone(),
        panic: panic.clone(),
    };
    let mut table = a.flat_new::<TrapKey, usize>(FlatConfig::default()).unwrap();
    let expected = HandleError::CrossRegion {
        expected: b.id(),
        found: a.id(),
    };
    let before = b.stats();
    invalid_operations(&mut b, &mut table, &query, expected);
    assert_eq!(b.stats(), before);
    assert_eq!(callbacks.get(), 0);
    a.reset().unwrap();
    let before = a.stats();
    invalid_operations(&mut a, &mut table, &query, HandleError::Stale);
    assert_eq!(a.stats(), before);
    assert_eq!(callbacks.get(), 0);
    let fresh = a.flat_new::<TrapKey, usize>(FlatConfig::default()).unwrap();
    assert_eq!(a.flat_len(&fresh), Ok(0));
    invalid_operations(&mut a, &mut table, &query, HandleError::Stale);
    assert_eq!(callbacks.get(), 0);
}

#[test]
fn postmark_relocation_and_same_byte_reuse_do_not_revive_old_owners() {
    let (mut r, _) = region();
    let mark = r.checkpoint().unwrap();
    let mut table = r.flat_new::<u64, u64>(FlatConfig::default()).unwrap();
    let address = r
        .flat_control_group(&table, 0)
        .unwrap()
        .unwrap()
        .as_ptr()
        .addr();
    r.rewind(mark).unwrap();
    let new = r.flat_new::<u64, u64>(FlatConfig::default()).unwrap();
    assert_eq!(
        r.flat_control_group(&new, 0)
            .unwrap()
            .unwrap()
            .as_ptr()
            .addr(),
        address
    );
    assert_eq!(r.flat_len(&table), Err(HandleError::Stale));
    assert_eq!(
        r.flat_reserve(&mut table, 0),
        Err(FlatError::Region(RegionError::Handle(HandleError::Stale)))
    );
    let mark = r.checkpoint().unwrap();
    let mut relocated = r.flat_new::<u64, u64>(FlatConfig::default()).unwrap();
    for id in 0..80 {
        r.flat_insert(&mut relocated, id, id).unwrap();
    }
    assert_eq!(r.flat_group_count(&relocated), Ok(8));
    r.rewind(mark).unwrap();
    assert_eq!(r.flat_len(&relocated), Err(HandleError::Stale));
    assert_eq!(r.flat_len(&new), Ok(0));
}

#[test]
fn release_failure_returns_owner_and_forgotten_or_dropped_handles_still_finalize_once() {
    let log = Log::default();
    let (mut a, ledger) = region();
    let (mut b, _) = region();
    let mut table = a.flat_new::<u64, Token>(FlatConfig::default()).unwrap();
    a.flat_insert(&mut table, 1, token(1, &log)).unwrap();
    let failure = failed(b.flat_release(table));
    assert!(matches!(
        failure.error,
        RegionError::Handle(HandleError::CrossRegion { .. })
    ));
    let table = failure.owner;
    assert!(log.borrow().is_empty());
    assert_eq!(a.flat_len(&table), Ok(1));
    a.flat_release(table).unwrap();
    assert_eq!(sorted(&log), [1]);
    for (id, forget) in [(2, false), (3, true)] {
        let mut table = a.flat_new::<u64, Token>(FlatConfig::default()).unwrap();
        a.flat_insert(&mut table, id, token(id, &log)).unwrap();
        if forget {
            // Suppress owner destruction explicitly, including if its current
            // plain-handle representation later acquires a Drop implementation.
            let _forgotten = std::mem::ManuallyDrop::new(table);
        } else {
            let _dropped_at_scope_exit = table;
        }
    }
    assert_eq!(sorted(&log), [1]);
    a.reset().unwrap();
    assert_eq!(sorted(&log), [1, 2, 3]);
    assert_eq!(ledger.live.get(), 0);
    let mark = a.checkpoint().unwrap();
    let mut table = a.flat_new::<u64, Token>(FlatConfig::default()).unwrap();
    for id in 10..74 {
        a.flat_insert(&mut table, id, token(id, &log)).unwrap();
    }
    let _forgotten = std::mem::ManuallyDrop::new(table);
    a.rewind(mark).unwrap();
    let mut expected = vec![1, 2, 3];
    expected.extend(10..74);
    assert_eq!(sorted(&log), expected);
    drop(a);
    assert_eq!(sorted(&log), expected);
    assert_eq!(ledger.live.get(), 0);
    assert_eq!(ledger.allocated.get(), ledger.deallocated.get());
}

struct PanicValue {
    id: u64,
    log: Log,
    once: Rc<Cell<bool>>,
}
impl Drop for PanicValue {
    fn drop(&mut self) {
        self.log.borrow_mut().push(self.id);
        assert!(
            !self.once.replace(false),
            "injected single destructor panic"
        )
    }
}

#[test]
fn one_clear_destructor_panic_keeps_surviving_probe_chains_and_all_drops_exact() {
    let (mut r, _) = region();
    let log = Log::default();
    let once = Rc::new(Cell::new(false));
    let mut table = r
        .flat_new::<u64, PanicValue>(FlatConfig::default())
        .unwrap();
    for id in 0..8 {
        r.flat_insert(
            &mut table,
            id,
            PanicValue {
                id,
                log: log.clone(),
                once: once.clone(),
            },
        )
        .unwrap();
    }
    once.set(true);
    assert!(catch_unwind(AssertUnwindSafe(|| r.flat_clear(&table).unwrap())).is_err());
    assert_eq!(r.flat_len(&table), Ok(7));
    assert_eq!(log.borrow().len(), 1);
    let removed = log.borrow()[0];
    for id in 0..8 {
        assert_eq!(
            r.flat_get(&table, &id).unwrap().map(|v| v.id),
            if id == removed { None } else { Some(id) }
        );
    }
    r.flat_insert(
        &mut table,
        99,
        PanicValue {
            id: 99,
            log: log.clone(),
            once: once.clone(),
        },
    )
    .unwrap();
    r.flat_clear(&table).unwrap();
    assert_eq!(r.flat_len(&table), Ok(0));
    r.flat_release(table).unwrap();
    drop(r);
    let mut expected: Vec<_> = (0..8).collect();
    expected.push(99);
    assert_eq!(sorted(&log), expected);
}
