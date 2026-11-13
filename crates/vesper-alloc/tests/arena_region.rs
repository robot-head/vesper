//! Independent public-API lifecycle and ownership tests for affine slot arenas.
use std::{
    alloc::Layout,
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ptr::NonNull,
    rc::Rc,
};
use vesper_alloc::{
    AllocError, ArenaError, ArenaKey, ChunkBacking, HARD_MAX_ARENA_ENTRY_BYTES, HandleError,
    Region, RegionConfig, RegionError, RegionSlotArena, SystemBacking,
};

#[derive(Default)]
struct Ledger {
    attempts: Cell<usize>,
    allocated: Cell<usize>,
    deallocated: Cell<usize>,
    refuse: Cell<bool>,
    live: RefCell<BTreeMap<usize, Layout>>,
}

struct Counting(Rc<Ledger>);

// SAFETY: successful requests delegate to SystemBacking. The live map checks
// exclusive ownership and the exact matching layout on each deallocation.
unsafe impl ChunkBacking for Counting {
    fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        self.0.attempts.set(self.0.attempts.get() + 1);
        if self.0.refuse.get() {
            return None;
        }
        let pointer = SystemBacking.allocate(layout)?;
        assert!(
            self.0
                .live
                .borrow_mut()
                .insert(pointer.as_ptr().addr(), layout)
                .is_none()
        );
        self.0.allocated.set(self.0.allocated.get() + 1);
        Some(pointer)
    }

    unsafe fn deallocate(&mut self, pointer: NonNull<u8>, layout: Layout) {
        assert_eq!(
            self.0.live.borrow_mut().remove(&pointer.as_ptr().addr()),
            Some(layout)
        );
        self.0.deallocated.set(self.0.deallocated.get() + 1);
        // SAFETY: this exact live pointer/layout came from SystemBacking above
        // and was just removed, so no subsequent deallocation may reuse it.
        unsafe { SystemBacking.deallocate(pointer, layout) }
    }
}

type Log = Rc<RefCell<Vec<u32>>>;

#[derive(Debug)]
struct Payload {
    id: u32,
    bytes: Box<[u8]>,
    log: Log,
}

impl Drop for Payload {
    fn drop(&mut self) {
        self.log.borrow_mut().push(self.id);
    }
}

fn payload(id: u32, log: &Log) -> Payload {
    Payload {
        id,
        bytes: vec![id as u8; 17].into_boxed_slice(),
        log: log.clone(),
    }
}

fn dropped(log: &Log) -> Vec<u32> {
    let mut ids = log.borrow().clone();
    ids.sort_unstable();
    ids
}

fn config() -> RegionConfig {
    RegionConfig {
        initial_chunk_bytes: 128,
        max_chunk_bytes: 512,
        max_reserved_bytes: 1024 * 1024,
        max_align: 4096,
        max_allocations: 4096,
        max_checkpoints: 16,
        retained_bytes: 0,
    }
}

fn counting(config: RegionConfig) -> (Region<Counting>, Rc<Ledger>) {
    let ledger = Rc::new(Ledger::default());
    let region = Region::with_backing(config, Counting(ledger.clone())).unwrap();
    (region, ledger)
}

fn assert_invalid_owner<R: ChunkBacking>(
    region: &mut Region<R>,
    owner: &mut RegionSlotArena<Payload>,
    key: ArenaKey<Payload>,
    expected: ArenaError,
) {
    let before = region.stats();
    assert_eq!(region.arena_id(owner).unwrap_err(), expected);
    assert_eq!(region.arena_len(owner).unwrap_err(), expected);
    assert_eq!(region.arena_is_empty(owner).unwrap_err(), expected);
    assert_eq!(region.arena_capacity(owner).unwrap_err(), expected);
    assert_eq!(region.arena_reserve(owner, 0).unwrap_err(), expected);
    assert_eq!(region.arena_get(owner, key).unwrap_err(), expected);
    assert_eq!(region.arena_get_mut(owner, key).unwrap_err(), expected);
    assert_eq!(
        region.arena_get_disjoint_mut(owner, key, key).unwrap_err(),
        expected
    );
    assert_eq!(region.arena_remove(owner, key).unwrap_err(), expected);
    let log = Log::default();
    let original = payload(777, &log);
    let pointer = original.bytes.as_ptr();
    let error = region.arena_insert(owner, original).unwrap_err();
    assert_eq!(error.error, expected);
    assert_eq!(error.value.bytes.as_ptr(), pointer);
    assert!(log.borrow().is_empty());
    drop(error.value);
    assert_eq!(dropped(&log), [777]);
    assert_eq!(region.stats(), before);
}

#[test]
fn removed_payload_remains_caller_owned_through_release_and_region_drop() {
    let log = Log::default();
    let (mut region, ledger) = counting(config());
    let mut arena = region.arena_with_capacity(3).unwrap();
    let first = region.arena_insert(&mut arena, payload(1, &log)).unwrap();
    region.arena_insert(&mut arena, payload(2, &log)).unwrap();
    region.arena_insert(&mut arena, payload(3, &log)).unwrap();
    let pointer = region.arena_get(&arena, first).unwrap().bytes.as_ptr();
    let removed = region.arena_remove(&mut arena, first).unwrap();
    assert_eq!(removed.bytes.as_ptr(), pointer);
    assert!(log.borrow().is_empty());
    region.arena_release(arena).unwrap();
    assert_eq!(dropped(&log), [2, 3]);
    assert_eq!(removed.id, 1);
    drop(region);
    assert_eq!(dropped(&log), [2, 3]);
    assert!(ledger.live.borrow().is_empty());
    assert_eq!(ledger.allocated.get(), ledger.deallocated.get());
    drop(removed);
    assert_eq!(dropped(&log), [1, 2, 3]);
}

#[test]
#[expect(
    clippy::forget_non_drop,
    reason = "Exercise documented forgotten-owner reclamation even though the current handle has no Drop glue"
)]
fn reset_finalizes_dropped_and_forgotten_owners_without_owning_removed_values() {
    let log = Log::default();
    let (mut region, ledger) = counting(config());
    {
        let mut dropped_owner = region.arena_new().unwrap();
        region
            .arena_insert(&mut dropped_owner, payload(1, &log))
            .unwrap();
    }
    let mut forgotten = region.arena_new().unwrap();
    region
        .arena_insert(&mut forgotten, payload(2, &log))
        .unwrap();
    std::mem::forget(forgotten);
    let mut survivor = region.arena_new().unwrap();
    let removed_key = region
        .arena_insert(&mut survivor, payload(3, &log))
        .unwrap();
    let live_key = region
        .arena_insert(&mut survivor, payload(4, &log))
        .unwrap();
    let removed = region.arena_remove(&mut survivor, removed_key).unwrap();
    assert!(log.borrow().is_empty());
    region.reset().unwrap();
    assert_eq!(dropped(&log), [1, 2, 4]);
    assert_eq!(region.stats().reserved_bytes, 0);
    assert_eq!(region.stats().used_bytes, 0);
    assert_invalid_owner(
        &mut region,
        &mut survivor,
        live_key,
        ArenaError::Region(RegionError::Handle(HandleError::Stale)),
    );
    let before = region.stats();
    let error = region.arena_release(survivor).unwrap_err();
    assert_eq!(error.error, RegionError::Handle(HandleError::Stale));
    assert_eq!(region.stats(), before);
    let _returned_stale_owner = error.owner;
    drop(region);
    assert!(ledger.live.borrow().is_empty());
    assert_eq!(ledger.allocated.get(), ledger.deallocated.get());
    assert_eq!(removed.id, 3);
    drop(removed);
    assert_eq!(dropped(&log), [1, 2, 3, 4]);
}

#[test]
#[expect(
    clippy::forget_non_drop,
    reason = "Exercise region-drop reclamation after explicitly forgetting the move-only owner"
)]
fn relocation_preserves_affine_payloads_and_region_drop_finalizes_once() {
    let log = Log::default();
    let (mut region, ledger) = counting(config());
    let mut arena = region.arena_with_capacity(1).unwrap();
    let first = region.arena_insert(&mut arena, payload(1, &log)).unwrap();
    let before = region.arena_get(&arena, first).unwrap() as *const Payload;
    region.alloc_bytes(4096, 64).unwrap();
    region.arena_reserve(&mut arena, 64).unwrap();
    let after = region.arena_get(&arena, first).unwrap() as *const Payload;
    assert_ne!(
        before, after,
        "live blocker forces actual buffer relocation"
    );
    for id in 2..=65 {
        region.arena_insert(&mut arena, payload(id, &log)).unwrap();
    }
    assert!(
        log.borrow().is_empty(),
        "relocation must move rather than finalize values"
    );
    let removed = region.arena_remove(&mut arena, first).unwrap();
    std::mem::forget(arena);
    drop(region);
    assert_eq!(dropped(&log), (2..=65).collect::<Vec<_>>());
    assert!(ledger.live.borrow().is_empty());
    assert_eq!(ledger.allocated.get(), ledger.deallocated.get());
    drop(removed);
    assert_eq!(dropped(&log), (1..=65).collect::<Vec<_>>());
}

#[test]
fn failed_backing_growth_returns_original_value_without_consuming_generation() {
    let log = Log::default();
    let (mut region, ledger) = counting(config());
    let mut arena = region.arena_with_capacity(2).unwrap();
    let capacity = region.arena_capacity(&arena).unwrap();
    let mut keys = Vec::new();
    for id in 0..capacity {
        keys.push(
            region
                .arena_insert(&mut arena, payload(id as u32, &log))
                .unwrap(),
        );
    }
    region.alloc_bytes(4096, 64).unwrap();
    let identity = region.arena_id(&arena).unwrap();
    let before = region.stats();
    let attempts = ledger.attempts.get();
    ledger.refuse.set(true);
    assert!(matches!(
        region.arena_reserve(&mut arena, 1),
        Err(ArenaError::Region(RegionError::Alloc(
            AllocError::OutOfMemory { .. }
        )))
    ));
    assert_eq!(region.stats(), before);
    let input = payload(900, &log);
    let pointer = input.bytes.as_ptr();
    let error = region.arena_insert(&mut arena, input).unwrap_err();
    assert!(matches!(
        error.error,
        ArenaError::Region(RegionError::Alloc(AllocError::OutOfMemory { .. }))
    ));
    assert_eq!(error.value.id, 900);
    assert_eq!(error.value.bytes.as_ptr(), pointer);
    assert_eq!(region.stats(), before);
    assert!(
        ledger.attempts.get() > attempts,
        "failure actually reached the backing"
    );
    assert_eq!(region.arena_id(&arena), Ok(identity));
    assert_eq!(region.arena_len(&arena), Ok(capacity));
    assert!(log.borrow().is_empty());
    for (id, key) in keys.iter().enumerate() {
        assert_eq!(region.arena_get(&arena, *key).unwrap().id, id as u32);
    }
    ledger.refuse.set(false);
    let retried = region.arena_insert(&mut arena, error.value).unwrap();
    assert_eq!(retried.generation(), keys.last().unwrap().generation() + 1);
    assert_eq!(
        region.arena_get(&arena, retried).unwrap().bytes.as_ptr(),
        pointer
    );
    drop(region);
    let mut expected = (0..capacity as u32).collect::<Vec<_>>();
    expected.push(900);
    assert_eq!(dropped(&log), expected);
    assert!(ledger.live.borrow().is_empty());
}

#[test]
fn constructor_layout_capacity_alignment_and_allocation_limits_are_preflighted() {
    let (mut region, ledger) = counting(config());
    let before = region.stats();
    assert!(matches!(
        region.arena_with_capacity::<u64>(usize::MAX),
        Err(ArenaError::Region(RegionError::Alloc(
            AllocError::SizeOverflow
        )))
    ));
    type Oversized = [u8; HARD_MAX_ARENA_ENTRY_BYTES + 1];
    assert_eq!(
        region.arena_new::<Oversized>().unwrap_err(),
        ArenaError::UnsupportedEntryLayout
    );
    assert_eq!(region.stats(), before);
    assert_eq!(ledger.attempts.get(), 0);

    let (mut limited, ledger) = counting(RegionConfig {
        max_reserved_bytes: 128,
        ..config()
    });
    let before = limited.stats();
    assert!(matches!(
        limited.arena_with_capacity::<u64>(1024),
        Err(ArenaError::Region(RegionError::Alloc(
            AllocError::CapacityExceeded { .. }
        )))
    ));
    assert_eq!(limited.stats(), before);
    assert_eq!(ledger.attempts.get(), 0);

    #[repr(align(128))]
    struct Aligned(u8);
    let (mut narrow, ledger) = counting(RegionConfig {
        max_align: 64,
        ..config()
    });
    let before = narrow.stats();
    assert_eq!(
        narrow.arena_new::<Aligned>().unwrap_err(),
        ArenaError::Region(RegionError::Alloc(AllocError::AlignmentTooLarge {
            align: 128,
            max: 64
        }))
    );
    assert_eq!(narrow.stats(), before);
    assert_eq!(ledger.attempts.get(), 0);
    let mut aligned = region.arena_new().unwrap();
    let key = region.arena_insert(&mut aligned, Aligned(7)).unwrap();
    let value = region.arena_get(&aligned, key).unwrap();
    assert_eq!((value as *const Aligned).addr() % 128, 0);
    assert_eq!(value.0, 7);

    let (mut no_slots, ledger) = counting(RegionConfig {
        max_allocations: 0,
        ..config()
    });
    let before = no_slots.stats();
    assert_eq!(
        no_slots.arena_new::<u8>().unwrap_err(),
        ArenaError::Region(RegionError::Alloc(AllocError::TooManyAllocations {
            limit: 0
        }))
    );
    assert_eq!(no_slots.stats(), before);
    assert_eq!(ledger.attempts.get(), 0);

    let (mut refused, ledger) = counting(config());
    ledger.refuse.set(true);
    let before = refused.stats();
    assert!(matches!(
        refused.arena_new::<Payload>(),
        Err(ArenaError::Region(RegionError::Alloc(
            AllocError::OutOfMemory { .. }
        )))
    ));
    assert_eq!(refused.stats(), before);
    assert_eq!(ledger.attempts.get(), 1);
    assert_eq!(ledger.allocated.get(), 0);
}

#[test]
fn overflow_and_tracked_slot_growth_failures_preserve_arena_and_input() {
    let log = Log::default();
    let (mut region, _) = counting(RegionConfig {
        max_allocations: 2,
        ..config()
    });
    let mut arena = region.arena_with_capacity(1).unwrap();
    let key = region.arena_insert(&mut arena, payload(1, &log)).unwrap();
    let before = region.stats();
    assert_eq!(
        region.arena_reserve(&mut arena, usize::MAX),
        Err(ArenaError::Region(RegionError::Alloc(
            AllocError::SizeOverflow
        )))
    );
    assert_eq!(region.stats(), before);
    region.alloc_bytes(4096, 64).unwrap();
    let before = region.stats();
    let error = region
        .arena_insert(&mut arena, payload(2, &log))
        .unwrap_err();
    assert_eq!(
        error.error,
        ArenaError::Region(RegionError::Alloc(AllocError::TooManyAllocations {
            limit: 2
        }))
    );
    assert_eq!(region.stats(), before);
    assert_eq!(region.arena_get(&arena, key).unwrap().id, 1);
    assert!(log.borrow().is_empty());
    drop(error.value);
    region.arena_release(arena).unwrap();
    assert_eq!(dropped(&log), [1, 2]);
}

#[test]
fn premark_removal_reuse_and_preallocated_append_survive_scratch_rewind() {
    let log = Log::default();
    let (mut region, _) = counting(config());
    let mut arena = region.arena_with_capacity(4).unwrap();
    let removed_key = region.arena_insert(&mut arena, payload(1, &log)).unwrap();
    let retained_key = region.arena_insert(&mut arena, payload(2, &log)).unwrap();
    let mark = region.checkpoint().unwrap();
    let removed = region.arena_remove(&mut arena, removed_key).unwrap();
    let reused = region.arena_insert(&mut arena, payload(3, &log)).unwrap();
    let appended = region.arena_insert(&mut arena, payload(4, &log)).unwrap();
    assert_eq!(reused.slot(), removed_key.slot());
    assert_ne!(reused.generation(), removed_key.generation());
    assert_ne!(appended.slot(), retained_key.slot());
    region.arena_get_mut(&arena, retained_key).unwrap().bytes[0] = 99;
    let mut scratch = region.arena_new().unwrap();
    region.arena_insert(&mut scratch, payload(9, &log)).unwrap();
    region.rewind(mark).unwrap();
    assert_eq!(region.arena_len(&arena), Ok(3));
    assert_eq!(region.arena_get(&arena, reused).unwrap().id, 3);
    assert_eq!(region.arena_get(&arena, appended).unwrap().id, 4);
    assert_eq!(region.arena_get(&arena, retained_key).unwrap().bytes[0], 99);
    assert_eq!(
        region.arena_get(&arena, removed_key).unwrap_err(),
        ArenaError::StaleKey
    );
    assert_eq!(dropped(&log), [9]);
    region.arena_release(arena).unwrap();
    assert_eq!(dropped(&log), [2, 3, 4, 9]);
    assert_eq!(removed.id, 1);
    drop(removed);
    assert_eq!(dropped(&log), [1, 2, 3, 4, 9]);
}

#[test]
fn protected_premark_growth_returns_original_input_and_preserves_stats() {
    let log = Log::default();
    let (mut region, _) = counting(config());
    let mut arena = region.arena_with_capacity(1).unwrap();
    let key = region.arena_insert(&mut arena, payload(1, &log)).unwrap();
    let mark = region.checkpoint().unwrap();
    let before = region.stats();
    assert_eq!(
        region.arena_reserve(&mut arena, 1),
        Err(ArenaError::Region(RegionError::ProtectedByWatermark))
    );
    let input = payload(2, &log);
    let pointer = input.bytes.as_ptr();
    let error = region.arena_insert(&mut arena, input).unwrap_err();
    assert_eq!(
        error.error,
        ArenaError::Region(RegionError::ProtectedByWatermark)
    );
    assert_eq!(error.value.bytes.as_ptr(), pointer);
    assert_eq!(region.stats(), before);
    assert_eq!(region.arena_get(&arena, key).unwrap().id, 1);
    assert!(log.borrow().is_empty());
    region.rewind(mark).unwrap();
    let inserted = region.arena_insert(&mut arena, error.value).unwrap();
    assert_eq!(inserted.generation(), key.generation() + 1);
    region.arena_release(arena).unwrap();
    assert_eq!(dropped(&log), [1, 2]);
}

#[test]
fn postmark_relocation_is_stale_after_rewind_and_reused_bytes_do_not_revive_it() {
    let log = Log::default();
    let (mut region, _) = counting(config());
    let mark = region.checkpoint().unwrap();
    let mut old = region.arena_with_capacity(1).unwrap();
    let key = region.arena_insert(&mut old, payload(1, &log)).unwrap();
    let identity = region.arena_id(&old).unwrap();
    let before = region.arena_get(&old, key).unwrap() as *const Payload;
    region.alloc_bytes(4096, 64).unwrap();
    region.arena_reserve(&mut old, 64).unwrap();
    assert_ne!(
        region.arena_get(&old, key).unwrap() as *const Payload,
        before
    );
    region.arena_insert(&mut old, payload(2, &log)).unwrap();
    assert!(log.borrow().is_empty());
    region.rewind(mark).unwrap();
    assert_eq!(dropped(&log), [1, 2]);
    let stale = ArenaError::Region(RegionError::Handle(HandleError::Stale));
    assert_invalid_owner(&mut region, &mut old, key, stale);
    let mut fresh = region.arena_with_capacity(1).unwrap();
    let fresh_key = region.arena_insert(&mut fresh, payload(3, &log)).unwrap();
    assert_eq!(key.slot(), fresh_key.slot());
    assert_ne!(identity, region.arena_id(&fresh).unwrap());
    assert!(matches!(
        region.arena_get(&fresh, key),
        Err(ArenaError::ForeignArena { .. })
    ));
    assert_invalid_owner(&mut region, &mut old, key, stale);
    let before = region.stats();
    let error = region.arena_release(old).unwrap_err();
    assert_eq!(error.error, RegionError::Handle(HandleError::Stale));
    assert_eq!(region.stats(), before);
    assert_eq!(error.owner.region(), region.id());
    region.arena_release(fresh).unwrap();
    assert_eq!(dropped(&log), [1, 2, 3]);
}

#[test]
fn foreign_owner_noops_and_consuming_release_return_checked_failures() {
    let log = Log::default();
    let (mut origin, _) = counting(config());
    let (mut foreign, _) = counting(config());
    let mut arena = origin.arena_with_capacity(1).unwrap();
    let key = origin.arena_insert(&mut arena, payload(1, &log)).unwrap();
    let identity = origin.arena_id(&arena).unwrap();
    let expected = HandleError::CrossRegion {
        expected: foreign.id(),
        found: origin.id(),
    };
    assert_invalid_owner(
        &mut foreign,
        &mut arena,
        key,
        ArenaError::Region(RegionError::Handle(expected)),
    );
    let before = foreign.stats();
    let error = foreign.arena_release(arena).unwrap_err();
    assert_eq!(error.error, RegionError::Handle(expected));
    assert_eq!(foreign.stats(), before);
    let arena = error.owner;
    assert_eq!(origin.arena_id(&arena), Ok(identity));
    assert_eq!(origin.arena_get(&arena, key).unwrap().id, 1);
    assert!(log.borrow().is_empty());
    origin.arena_release(arena).unwrap();
    assert_eq!(dropped(&log), [1]);
}

#[test]
fn zst_drop_values_are_logical_values_and_removed_ones_remain_caller_owned() {
    thread_local! { static DROPS: Cell<usize> = const { Cell::new(0) }; }
    struct Unit;
    impl Drop for Unit {
        fn drop(&mut self) {
            DROPS.with(|n| n.set(n.get() + 1));
        }
    }
    DROPS.with(|n| n.set(0));
    let (mut region, _) = counting(config());
    let mut arena = region.arena_new().unwrap();
    assert!(
        region.stats().used_bytes > 0,
        "even a ZST arena owns a resident header"
    );
    let first = region.arena_insert(&mut arena, Unit).unwrap();
    for _ in 1..32 {
        region.arena_insert(&mut arena, Unit).unwrap();
    }
    let removed = region.arena_remove(&mut arena, first).unwrap();
    assert_eq!(region.arena_len(&arena), Ok(31));
    assert_eq!(DROPS.with(Cell::get), 0);
    region.reset().unwrap();
    assert_eq!(DROPS.with(Cell::get), 31);
    drop(region);
    assert_eq!(DROPS.with(Cell::get), 31);
    drop(removed);
    assert_eq!(DROPS.with(Cell::get), 32);
}
