//! Independent checked-handle and backing lifecycle acceptance for tiled rows.
use std::alloc::Layout;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ptr::NonNull;
use std::rc::Rc;
use vesper_alloc::{
    AllocError, ChunkBacking, HandleError, Region, RegionConfig, RegionError, RegionTiled2,
    SystemBacking, TiledError,
};

#[derive(Default)]
struct Ledger {
    attempts: Cell<usize>,
    allocations: Cell<usize>,
    deallocations: Cell<usize>,
    fail: Cell<bool>,
    live: RefCell<BTreeMap<usize, Layout>>,
}

#[derive(Clone, Default)]
struct Counting(Rc<Ledger>);

// SAFETY: successful requests delegate the exact Layout to SystemBacking and
// retain its exclusive allocation. The ledger checks that each deallocation
// consumes exactly one live pointer with precisely the original Layout.
unsafe impl ChunkBacking for Counting {
    fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        self.0.attempts.set(self.0.attempts.get() + 1);
        if self.0.fail.get() {
            return None;
        }
        let ptr = SystemBacking.allocate(layout)?;
        assert!(
            self.0
                .live
                .borrow_mut()
                .insert(ptr.as_ptr() as usize, layout)
                .is_none()
        );
        self.0.allocations.set(self.0.allocations.get() + 1);
        Some(ptr)
    }

    unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) {
        assert_eq!(
            self.0.live.borrow_mut().remove(&(ptr.as_ptr() as usize)),
            Some(layout)
        );
        self.0.deallocations.set(self.0.deallocations.get() + 1);
        // SAFETY: the checked ledger entry came from the matching successful
        // allocation above and has just been removed, preventing double free.
        unsafe { SystemBacking.deallocate(ptr, layout) };
    }
}

fn config() -> RegionConfig {
    RegionConfig {
        initial_chunk_bytes: 1024,
        max_chunk_bytes: 4096,
        max_reserved_bytes: 64 * 1024,
        max_align: 64,
        max_allocations: 128,
        max_checkpoints: 16,
        retained_bytes: 0,
    }
}

fn counting(cfg: RegionConfig) -> (Region<Counting>, Rc<Ledger>) {
    let backing = Counting::default();
    let ledger = Rc::clone(&backing.0);
    (Region::with_backing(cfg, backing).unwrap(), ledger)
}

type Rows = RegionTiled2<u32, i64, 8>;

fn pair(i: u32) -> (u32, i64) {
    (i * 3 + 1, -i64::from(i) * 11 - 7)
}

fn append(r: &mut Region<Counting>, rows: &mut Rows, range: std::ops::Range<u32>) {
    for i in range {
        let (a, b) = pair(i);
        r.tiled2_push(rows, a, b).unwrap();
    }
}

fn actual(r: &Region<Counting>, rows: &Rows) -> Vec<(u32, i64)> {
    (0..r.tiled2_len(rows).unwrap())
        .map(|i| r.tiled2_get(rows, i).unwrap().unwrap())
        .collect()
}

fn invalid_everywhere(r: &mut Region<Counting>, rows: &mut Rows, error: HandleError) {
    let before = r.stats();
    assert_eq!(r.tiled2_len(rows), Err(error));
    assert_eq!(r.stats(), before);
    assert_eq!(r.tiled2_is_empty(rows), Err(error));
    assert_eq!(r.stats(), before);
    assert_eq!(r.tiled2_capacity(rows), Err(error));
    assert_eq!(r.stats(), before);
    assert_eq!(r.tiled2_tile_count(rows), Err(error));
    assert_eq!(r.stats(), before);
    for index in [0, usize::MAX] {
        assert_eq!(r.tiled2_get(rows, index), Err(error));
        assert_eq!(r.stats(), before);
        assert_eq!(r.tiled2_columns(rows, index), Err(error));
        assert_eq!(r.stats(), before);
        assert_eq!(r.tiled2_columns_mut(rows, index), Err(error));
        assert_eq!(r.stats(), before);
    }
    for additional in [0, usize::MAX] {
        assert_eq!(
            r.tiled2_reserve(rows, additional),
            Err(RegionError::Handle(error))
        );
        assert_eq!(r.stats(), before);
    }
    assert_eq!(r.tiled2_pop(rows), Err(error));
    assert_eq!(r.stats(), before);
    let failure = r.tiled2_push(rows, 41, -97).unwrap_err();
    assert_eq!(failure.value, (41, -97));
    assert_eq!(failure.error, RegionError::Handle(error));
    assert_eq!(r.stats(), before);
}

#[test]
fn premark_partial_tile_mutation_survives_scratch_rewind() {
    let (mut r, _) = counting(config());
    let mut rows = r.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
    append(&mut r, &mut rows, 0..3);
    let address = r.tiled2_columns(&rows, 0).unwrap().unwrap().0.as_ptr();
    let mark = r.checkpoint().unwrap();
    let scratch = r.alloc_bytes(513, 16).unwrap();
    append(&mut r, &mut rows, 3..7);
    let (a, b) = r.tiled2_columns_mut(&rows, 0).unwrap().unwrap();
    a[1] = 999;
    b[1] = -1234;
    r.rewind(mark).unwrap();
    assert_eq!(r.bytes(&scratch), Err(HandleError::Stale));
    let mut expected: Vec<_> = (0..7).map(pair).collect();
    expected[1] = (999, -1234);
    assert_eq!(actual(&r, &rows), expected);
    assert_eq!(r.tiled2_tile_count(&rows), Ok(1));
    assert_eq!(
        r.tiled2_columns(&rows, 0).unwrap().unwrap().0.as_ptr(),
        address
    );
}

#[test]
fn premark_capacity_can_publish_new_live_tiles_and_survive_repeated_rewind() {
    let (mut r, _) = counting(config());
    let mut rows = r.tiled2_with_capacity::<u32, i64, 8>(24).unwrap();
    append(&mut r, &mut rows, 0..2);
    for end in [9, 17, 24] {
        let mark = r.checkpoint().unwrap();
        let scratch = r.alloc_bytes(400, 8).unwrap();
        let start = r.tiled2_len(&rows).unwrap() as u32;
        append(&mut r, &mut rows, start..end);
        r.rewind(mark).unwrap();
        assert_eq!(r.bytes(&scratch), Err(HandleError::Stale));
        assert_eq!(actual(&r, &rows), (0..end).map(pair).collect::<Vec<_>>());
        assert_eq!(r.tiled2_tile_count(&rows), Ok((end as usize).div_ceil(8)));
    }
    assert_eq!(r.tiled2_capacity(&rows), Ok(24));
}

#[test]
fn protected_growth_preserves_tuple_rows_identity_and_all_stats() {
    let (mut r, ledger) = counting(config());
    let mut rows = r.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
    append(&mut r, &mut rows, 0..8);
    let mark = r.checkpoint().unwrap();
    let before = r.stats();
    let attempts = ledger.attempts.get();
    let address = r.tiled2_columns(&rows, 0).unwrap().unwrap().0.as_ptr();
    let failure = r.tiled2_push(&mut rows, 987, -654).unwrap_err();
    assert_eq!(
        (failure.value, failure.error),
        ((987, -654), RegionError::ProtectedByWatermark)
    );
    assert_eq!(
        r.tiled2_reserve(&mut rows, 1),
        Err(RegionError::ProtectedByWatermark)
    );
    assert_eq!(r.stats(), before);
    assert_eq!(ledger.attempts.get(), attempts);
    assert_eq!(rows.region(), r.id());
    assert_eq!(
        r.tiled2_columns(&rows, 0).unwrap().unwrap().0.as_ptr(),
        address
    );
    assert_eq!(actual(&r, &rows), (0..8).map(pair).collect::<Vec<_>>());
    r.commit(mark).unwrap();
    r.tiled2_push(&mut rows, 987, -654).unwrap();
    assert_eq!(r.tiled2_get(&rows, 8), Ok(Some((987, -654))));
}

#[test]
fn postmark_allocation_is_stale_after_same_slot_and_bytes_are_reused() {
    let (mut r, _) = counting(config());
    let mark = r.checkpoint().unwrap();
    let mut old = r.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
    append(&mut r, &mut old, 0..2);
    let address = r.tiled2_columns(&old, 0).unwrap().unwrap().0.as_ptr();
    r.rewind(mark).unwrap();
    let mut new = r.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
    append(&mut r, &mut new, 7..10);
    assert_eq!(
        r.tiled2_columns(&new, 0).unwrap().unwrap().0.as_ptr(),
        address
    );
    invalid_everywhere(&mut r, &mut old, HandleError::Stale);
    let before = r.stats();
    let failure = r.tiled2_release(old).unwrap_err();
    assert_eq!(failure.error, RegionError::Handle(HandleError::Stale));
    assert_eq!(failure.owner.region(), r.id());
    assert_eq!(r.stats(), before);
    let mut returned = failure.owner;
    invalid_everywhere(&mut r, &mut returned, HandleError::Stale);
    assert_eq!(actual(&r, &new), (7..10).map(pair).collect::<Vec<_>>());
}

#[test]
fn relocated_postmark_owner_is_invalidated_by_covering_rewind() {
    let (mut r, _) = counting(config());
    let mark = r.checkpoint().unwrap();
    let mut rows = r.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
    append(&mut r, &mut rows, 0..8);
    let address = r.tiled2_columns(&rows, 0).unwrap().unwrap().0.as_ptr();
    let blocker = r.alloc_bytes(1, 1).unwrap();
    append(&mut r, &mut rows, 8..9);
    assert_ne!(
        r.tiled2_columns(&rows, 0).unwrap().unwrap().0.as_ptr(),
        address
    );
    assert_eq!(actual(&r, &rows), (0..9).map(pair).collect::<Vec<_>>());
    r.rewind(mark).unwrap();
    assert_eq!(r.bytes(&blocker), Err(HandleError::Stale));
    invalid_everywhere(&mut r, &mut rows, HandleError::Stale);
}

#[test]
fn foreign_operations_including_noops_return_the_unconsumed_owner() {
    let (mut a, _) = counting(config());
    let (mut b, _) = counting(config());
    let mut rows = a.tiled2_new::<u32, i64, 8>().unwrap();
    append(&mut a, &mut rows, 0..2);
    let foreign = HandleError::CrossRegion {
        expected: b.id(),
        found: a.id(),
    };
    invalid_everywhere(&mut b, &mut rows, foreign);
    let before = b.stats();
    let failure = b.tiled2_release(rows).unwrap_err();
    assert_eq!(failure.error, RegionError::Handle(foreign));
    assert_eq!(b.stats(), before);
    let mut returned = failure.owner;
    append(&mut a, &mut returned, 2..5);
    assert_eq!(actual(&a, &returned), (0..5).map(pair).collect::<Vec<_>>());
    a.tiled2_release(returned).unwrap();
}

#[test]
fn reset_invalidates_even_zero_capacity_owner_without_serial_revival() {
    let (mut r, ledger) = counting(config());
    let mut empty = r.tiled2_new::<u32, i64, 8>().unwrap();
    let mut rows = r.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
    append(&mut r, &mut rows, 0..3);
    r.reset().unwrap();
    assert!(ledger.live.borrow().is_empty());
    let fresh = r.tiled2_new::<u32, i64, 8>().unwrap();
    invalid_everywhere(&mut r, &mut empty, HandleError::Stale);
    invalid_everywhere(&mut r, &mut rows, HandleError::Stale);
    assert_eq!(r.tiled2_len(&fresh), Ok(0));
}

#[test]
fn zero_capacity_noops_are_valid_but_still_consume_a_tracked_slot() {
    let (mut r, _) = counting(config());
    let before = r.stats();
    let mut rows = r.tiled2_new::<u32, i64, 8>().unwrap();
    assert_eq!(
        r.stats().tracked_allocations,
        before.tracked_allocations + 1
    );
    assert_eq!(r.stats().used_bytes, before.used_bytes);
    let allocated = r.stats();
    assert_eq!(r.tiled2_len(&rows), Ok(0));
    assert_eq!(r.tiled2_capacity(&rows), Ok(0));
    assert_eq!(r.tiled2_tile_count(&rows), Ok(0));
    assert_eq!(r.tiled2_is_empty(&rows), Ok(true));
    assert_eq!(r.tiled2_get(&rows, usize::MAX), Ok(None));
    assert_eq!(r.tiled2_columns(&rows, usize::MAX), Ok(None));
    assert_eq!(r.tiled2_columns_mut(&rows, usize::MAX), Ok(None));
    assert_eq!(r.tiled2_pop(&mut rows), Ok(None));
    r.tiled2_reserve(&mut rows, 0).unwrap();
    assert_eq!(r.stats(), allocated);
    r.tiled2_release(rows).unwrap();
    assert_eq!(r.stats().tracked_allocations, before.tracked_allocations);
    assert_eq!(r.stats().used_bytes, before.used_bytes);
    // A released tail relinquishes its slot and bytes, while chunks remain
    // region-owned until normal reset/drop reclamation.
    assert_eq!(r.stats().reserved_bytes, allocated.reserved_bytes);
    assert_eq!(r.stats().chunks, allocated.chunks);
}

#[test]
fn reserve_overflow_and_constructor_overflow_do_not_allocate_or_mutate() {
    let (mut r, ledger) = counting(config());
    let mut rows = r.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
    append(&mut r, &mut rows, 0..1);
    let before = r.stats();
    let attempts = ledger.attempts.get();
    assert_eq!(
        r.tiled2_reserve(&mut rows, usize::MAX),
        Err(RegionError::Alloc(AllocError::SizeOverflow))
    );
    assert!(matches!(
        r.tiled2_with_capacity::<u32, i64, 8>(usize::MAX),
        Err(TiledError::Region(RegionError::Alloc(
            AllocError::SizeOverflow
        )))
    ));
    assert_eq!(r.stats(), before);
    assert_eq!(ledger.attempts.get(), attempts);
    assert_eq!(actual(&r, &rows), [pair(0)]);
}

fn one_tile_config() -> RegionConfig {
    // For u32/i64 at width8, two 64-byte columns plus the rounded header
    // occupy 192 bytes. A full tile exhausts this finite initial chunk.
    RegionConfig {
        initial_chunk_bytes: 192,
        max_chunk_bytes: 192,
        ..config()
    }
}

#[test]
fn backing_refusal_preserves_full_collection_and_original_pair() {
    let (mut r, ledger) = counting(one_tile_config());
    let mut rows = r.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
    append(&mut r, &mut rows, 0..8);
    ledger.fail.set(true);
    let before = r.stats();
    let attempts = ledger.attempts.get();
    let failure = r.tiled2_push(&mut rows, 123, -456).unwrap_err();
    assert_eq!(failure.value, (123, -456));
    assert!(matches!(
        failure.error,
        RegionError::Alloc(AllocError::OutOfMemory { .. })
    ));
    assert!(ledger.attempts.get() > attempts);
    assert_eq!(r.stats(), before);
    assert_eq!(actual(&r, &rows), (0..8).map(pair).collect::<Vec<_>>());
    ledger.fail.set(false);
    r.tiled2_push(&mut rows, 123, -456).unwrap();
}

#[test]
fn finite_reservation_and_allocation_table_bounds_preserve_existing_rows() {
    for allocation_limit in [false, true] {
        let mut cfg = one_tile_config();
        if allocation_limit {
            cfg.max_allocations = 1;
        } else {
            cfg.max_reserved_bytes = 192;
        }
        let (mut r, ledger) = counting(cfg);
        let mut rows = r.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
        append(&mut r, &mut rows, 0..8);
        let before = r.stats();
        let attempts = ledger.attempts.get();
        let failure = r.tiled2_push(&mut rows, 5, -9).unwrap_err();
        assert_eq!(failure.value, (5, -9));
        if allocation_limit {
            assert_eq!(
                failure.error,
                RegionError::Alloc(AllocError::TooManyAllocations { limit: 1 })
            );
        } else {
            assert!(matches!(
                failure.error,
                RegionError::Alloc(AllocError::CapacityExceeded { limit: 192, .. })
            ));
        }
        assert_eq!(r.stats(), before);
        assert_eq!(ledger.attempts.get(), attempts);
        assert_eq!(actual(&r, &rows), (0..8).map(pair).collect::<Vec<_>>());
    }
}

#[test]
fn alignment_limit_rejects_constructor_before_backing_or_slot_publication() {
    let (mut r, ledger) = counting(RegionConfig {
        max_align: 32,
        ..config()
    });
    let before = r.stats();
    for capacity in [0, 8] {
        assert!(matches!(
            r.tiled2_with_capacity::<u32, i64, 8>(capacity),
            Err(TiledError::Region(RegionError::Alloc(
                AllocError::AlignmentTooLarge { align: 64, max: 32 }
            )))
        ));
        assert_eq!(r.stats(), before);
        assert_eq!(ledger.attempts.get(), 0);
    }
}

#[test]
fn release_moves_owner_and_reclaims_tail_without_releasing_backing_early() {
    let (mut r, ledger) = counting(config());
    let before = r.stats();
    let mut rows = r.tiled2_with_capacity::<u32, i64, 8>(16).unwrap();
    append(&mut r, &mut rows, 0..11);
    let moved = rows;
    r.tiled2_release(moved).unwrap();
    assert_eq!(r.stats().tracked_allocations, before.tracked_allocations);
    assert_eq!(r.stats().used_bytes, before.used_bytes);
    assert_eq!(ledger.deallocations.get(), 0);
    r.reset().unwrap();
    assert!(ledger.live.borrow().is_empty());
    assert_eq!(ledger.allocations.get(), ledger.deallocations.get());
}

#[test]
#[allow(clippy::forget_non_drop)] // Explicitly exercise the contract's forgotten-owner case.
fn forgotten_owner_is_reclaimed_exactly_once_by_region_reset_or_drop() {
    for reset in [false, true] {
        let (mut r, ledger) = counting(config());
        let mut rows = r.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
        append(&mut r, &mut rows, 0..40);
        std::mem::forget(rows);
        assert!(!ledger.live.borrow().is_empty());
        assert_eq!(ledger.deallocations.get(), 0);
        if reset {
            r.reset().unwrap();
            assert_eq!(r.stats().tracked_allocations, 0);
            assert!(ledger.live.borrow().is_empty());
        }
        drop(r);
        assert!(ledger.live.borrow().is_empty());
        assert_eq!(ledger.allocations.get(), ledger.deallocations.get());
        assert!(ledger.allocations.get() > 0);
    }
}
