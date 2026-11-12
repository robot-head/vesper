use std::{
    alloc::Layout,
    cell::{Cell, RefCell},
    ptr::NonNull,
    rc::Rc,
};
use vesper_alloc::{
    ChunkBacking, SLAB_BYTES, SLAB_SIZE_CLASSES, ShardSlab, SlabConfig, SlabError, SystemBacking,
};

#[derive(Default)]
struct Ledger {
    allocations: Cell<usize>,
    releases: Cell<usize>,
    refuse: Cell<bool>,
}
#[derive(Clone, Default)]
struct Counting(Rc<Ledger>);
// SAFETY: forwards exact allocations/layouts to SystemBacking.
unsafe impl ChunkBacking for Counting {
    fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        if self.0.refuse.get() {
            return None;
        }
        let ptr = SystemBacking.allocate(layout)?;
        self.0.allocations.set(self.0.allocations.get() + 1);
        Some(ptr)
    }
    unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) {
        self.0.releases.set(self.0.releases.get() + 1);
        // SAFETY: delegated ownership and exact layout from pool.
        unsafe { SystemBacking.deallocate(ptr, layout) };
    }
}
fn config(max_slabs: usize) -> SlabConfig {
    SlabConfig {
        max_slabs,
        max_live_blocks: max_slabs * 8192,
    }
}
fn pool(max_slabs: usize) -> (ShardSlab<Counting>, Rc<Ledger>) {
    let backing = Counting::default();
    let log = backing.0.clone();
    (
        ShardSlab::with_backing(config(max_slabs), backing).unwrap(),
        log,
    )
}
#[test]
fn all_classes_and_alignments_are_real_initialized_disjoint_allocations() {
    assert_eq!(SLAB_SIZE_CLASSES.len(), 48);
    for class in SLAB_SIZE_CLASSES {
        let (pool, log) = pool(1);
        let mut blocks = vec![];
        for align in (0..=14)
            .map(|shift| 1usize << shift)
            .filter(|a| class.is_multiple_of(*a))
        {
            let block = pool.allocate(class, align).unwrap();
            assert_eq!(block.as_ptr().addr() % align, 0);
            assert_eq!(block.capacity(), class);
            assert!(block.iter().all(|byte| *byte == 0));
            assert!(
                blocks
                    .iter()
                    .all(|old: &vesper_alloc::SlabBlock<Counting>| old.as_ptr() != block.as_ptr())
            );
            blocks.push(block);
            // Large classes have at least four slots. Free each before next
            // alignment so this test checks all supported alignments in one slab.
            if blocks.len() == 4 {
                blocks.clear();
            }
        }
        drop(blocks);
        assert_eq!(log.allocations.get(), 1);
        pool.close().unwrap();
        assert_eq!(log.releases.get(), 1);
    }
}
#[test]
fn fits_request_and_alignment_without_fallback_or_hiding_padding() {
    let pool = ShardSlab::new(config(3)).unwrap();
    let a = pool.allocate(17, 16).unwrap();
    assert_eq!(a.capacity(), 32);
    assert_eq!(a.len(), 17);
    let b = pool.allocate(3, 4096).unwrap();
    assert_eq!(b.capacity(), 4096);
    assert_eq!(b.as_ptr().addr() % 4096, 0);
    let z = pool.allocate(0, 1).unwrap();
    assert_eq!(z.len(), 0);
    assert_eq!(z.capacity(), 8);
    assert_eq!(
        pool.allocate(1, 0).unwrap_err(),
        SlabError::InvalidAlignment { align: 0 }
    );
    assert_eq!(
        pool.allocate(1, 3).unwrap_err(),
        SlabError::InvalidAlignment { align: 3 }
    );
    assert!(matches!(
        pool.allocate(16385, 8),
        Err(SlabError::UnsupportedLayout { .. })
    ));
    assert!(matches!(
        pool.allocate(1, 32768),
        Err(SlabError::UnsupportedLayout { .. })
    ));
    assert_eq!(
        pool.allocate(usize::MAX, 8).unwrap_err(),
        SlabError::SizeOverflow
    );
}
#[test]
fn bounded_capacity_full_slab_reuse_and_stale_generation() {
    let (pool, log) = pool(1);
    let mut blocks = vec![];
    for _ in 0..4 {
        blocks.push(pool.allocate(16384, 8).unwrap());
    }
    let before = pool.stats().unwrap();
    assert_eq!(before.reserved_bytes, SLAB_BYTES);
    assert_eq!(before.live_blocks, 4);
    assert_eq!(
        pool.allocate(16384, 8).unwrap_err(),
        SlabError::CapacityExceeded { max_slabs: 1 }
    );
    let released = blocks.pop().unwrap();
    let old_key = released.key();
    let old_ptr = released.as_ptr();
    pool.free(released).unwrap();
    assert_eq!(pool.validate(old_key), Err(SlabError::Stale));
    let new = pool.allocate(16384, 8).unwrap();
    assert_eq!(new.as_ptr(), old_ptr);
    assert_ne!(new.key().generation(), old_key.generation());
    assert_eq!(pool.validate(old_key), Err(SlabError::Stale));
    assert_eq!(pool.validate(new.key()), Ok(()));
    assert!(new.iter().all(|x| *x == 0));
    assert_eq!(log.allocations.get(), 1);
    drop(new);
    drop(blocks);
    pool.close().unwrap();
    assert_eq!(log.releases.get(), 1);
}
#[test]
fn live_limit_and_failed_provisioning_are_failure_atomic() {
    let backing = Counting::default();
    let log = backing.0.clone();
    let pool = ShardSlab::with_backing(
        SlabConfig {
            max_slabs: 2,
            max_live_blocks: 1,
        },
        backing,
    )
    .unwrap();
    let owned = pool.allocate_copy(b"owner", 8).unwrap();
    let before = pool.stats().unwrap();
    assert_eq!(
        pool.allocate(8, 8).unwrap_err(),
        SlabError::LiveLimit { limit: 1 }
    );
    assert_eq!(pool.stats().unwrap(), before);
    assert_eq!(&*owned, b"owner");
    drop(owned);
    log.refuse.set(true);
    let before = pool.stats().unwrap();
    assert_eq!(pool.allocate(1000, 8).unwrap_err(), SlabError::OutOfMemory);
    assert_eq!(pool.stats().unwrap(), before);
    log.refuse.set(false);
    assert!(pool.allocate(1000, 8).is_ok());
    pool.close().unwrap();
    assert_eq!(log.allocations.get(), log.releases.get());
}
#[test]
fn foreign_free_returns_intact_owner_and_origin_keys_never_authorize_access() {
    let (a, _) = pool(1);
    let (b, _) = pool(1);
    let owner = a.allocate_copy(b"still mine", 8).unwrap();
    let key = owner.key();
    let rejected = b.free(owner).unwrap_err();
    assert_eq!(
        rejected.error,
        SlabError::Foreign {
            expected: b.id(),
            found: a.id()
        }
    );
    assert_eq!(&*rejected.owner, b"still mine");
    assert_eq!(a.validate(key), Ok(()));
    assert!(matches!(b.validate(key), Err(SlabError::Foreign { .. })));
    a.free(rejected.owner).unwrap();
    assert_eq!(a.validate(key), Err(SlabError::Stale));
}
#[test]
fn owner_survives_pool_drop_and_last_block_returns_each_backing_once() {
    let (pool, log) = pool(2);
    let a = pool.allocate_copy(b"alive", 8).unwrap();
    let mut b = pool.allocate_copy(b"also alive", 64).unwrap();
    assert_eq!(log.allocations.get(), 2);
    drop(pool);
    assert_eq!(log.releases.get(), 0);
    assert_eq!(&*a, b"alive");
    b[0] = b'A';
    assert_eq!(&*b, b"Also alive");
    drop(a);
    assert_eq!(log.releases.get(), 0);
    drop(b);
    assert_eq!(log.releases.get(), 2);
}
#[test]
fn close_returns_pool_until_live_blocks_resolve() {
    let (pool, log) = pool(1);
    let owner = pool.allocate(8, 8).unwrap();
    let pending = pool.close().unwrap_err();
    assert_eq!(pending.error, SlabError::LiveBlocks { count: 1 });
    assert_eq!(log.releases.get(), 0);
    pending.pool.free(owner).unwrap();
    let stats = pending.pool.close().unwrap();
    assert_eq!(stats.live_blocks, 0);
    assert_eq!(log.releases.get(), 1);
}
#[test]
fn resize_preserves_data_zeroes_extension_and_returns_owner_on_failure() {
    let (pool, _) = pool(2);
    let block = pool.allocate_copy(b"abc", 8).unwrap();
    let original_key = block.key();
    let block = pool.resize(block, 7, 8).unwrap();
    assert_eq!(block.key(), original_key);
    assert_eq!(&*block, b"abc\0\0\0\0");
    let block = pool.resize(block, 2, 8).unwrap();
    assert_eq!(&*block, b"ab");
    let block = pool.resize(block, 9, 8).unwrap();
    assert_ne!(block.key(), original_key);
    assert_eq!(pool.validate(original_key), Err(SlabError::Stale));
    assert_eq!(&block[..2], b"ab");
    assert!(block[2..].iter().all(|b| *b == 0));
    let rejected = pool.resize(block, 1000, 8).unwrap_err();
    assert_eq!(rejected.error, SlabError::CapacityExceeded { max_slabs: 2 });
    assert_eq!(&rejected.owner[..2], b"ab");
    let rejected = pool.resize(rejected.owner, 8, 3).unwrap_err();
    assert_eq!(rejected.error, SlabError::InvalidAlignment { align: 3 });
    pool.free(rejected.owner).unwrap();
}
#[test]
fn local_transfer_failure_returns_source_and_success_changes_origin() {
    let (from, _) = pool(1);
    let (to, _) = pool(1);
    let block = from.allocate_copy(b"transfer", 8).unwrap();
    let old = block.key();
    let occupied = to.allocate(64, 8).unwrap();
    let failed = to.transfer_from(block).unwrap_err();
    assert_eq!(failed.error, SlabError::CapacityExceeded { max_slabs: 1 });
    assert_eq!(&*failed.owner, b"transfer");
    assert_eq!(from.validate(old), Ok(()));
    drop(occupied);
    drop(to); // slabs stay class-specific until pool destruction
    let (to, _) = pool(1);
    let new = to.transfer_from(failed.owner).unwrap();
    assert_eq!(&*new, b"transfer");
    assert_eq!(new.key().pool(), to.id());
    assert_eq!(from.validate(old), Err(SlabError::Stale));
}
#[test]
fn configuration_limits_are_explicit_and_zero_budget_is_useful() {
    assert!(matches!(
        ShardSlab::new(SlabConfig {
            max_slabs: 4097,
            max_live_blocks: 0
        }),
        Err(SlabError::InvalidConfig(_))
    ));
    assert!(matches!(
        ShardSlab::new(SlabConfig {
            max_slabs: 1,
            max_live_blocks: 8193
        }),
        Err(SlabError::InvalidConfig(_))
    ));
    let pool = ShardSlab::new(config(0)).unwrap();
    assert_eq!(
        pool.allocate(0, 1).unwrap_err(),
        SlabError::LiveLimit { limit: 0 }
    );
    pool.close().unwrap();
}
type Callback = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;
#[derive(Clone, Default)]
struct Reentrant {
    callback: Callback,
}
// SAFETY: delegates memory ownership and exact layouts to SystemBacking.
unsafe impl ChunkBacking for Reentrant {
    fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let callback = self.callback.borrow_mut().take();
        if let Some(callback) = callback {
            callback();
        }
        SystemBacking.allocate(layout)
    }
    unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: same originating allocation/layout.
        unsafe { SystemBacking.deallocate(ptr, layout) };
    }
}
#[test]
fn reentrant_backing_cannot_panic_drop_or_consume_owner_on_busy_free() {
    let backing = Reentrant::default();
    let callback = backing.callback.clone();
    let pool = Rc::new(ShardSlab::with_backing(config(2), backing).unwrap());
    let block = pool.allocate(8, 8).unwrap();
    let old = block.key();
    let nested = pool.clone();
    let called = Rc::new(Cell::new(false));
    let result = called.clone();
    *callback.borrow_mut() = Some(Box::new(move || {
        assert_eq!(nested.allocate(8, 8).unwrap_err(), SlabError::Busy);
        let error = nested.free(block).unwrap_err();
        assert_eq!(error.error, SlabError::Busy);
        drop(error.owner); // deferred while provisioning holds the state borrow
        result.set(true);
    }));
    let new = pool.allocate(64, 8).unwrap();
    assert!(called.get());
    assert_eq!(pool.validate(old), Err(SlabError::Stale));
    assert_eq!(pool.stats().unwrap().live_blocks, 1);
    drop(new);
    assert_eq!(pool.stats().unwrap().live_blocks, 0);
}
#[test]
fn deterministic_reuse_model_keeps_live_payloads_disjoint() {
    let pool = ShardSlab::new(SlabConfig {
        max_slabs: 4,
        max_live_blocks: 128,
    })
    .unwrap();
    let mut live = vec![];
    let mut seed = 7u64;
    for turn in 0..2000u64 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        if seed & 3 == 0 && !live.is_empty() {
            let at = seed as usize % live.len();
            drop(live.swap_remove(at));
        } else if live.len() < 128
            && let Ok(mut block) = pool.allocate(16, 8)
        {
            block[..8].copy_from_slice(&turn.to_le_bytes());
            live.push((turn, block));
        }
        for (expected, block) in &live {
            assert_eq!(
                u64::from_le_bytes(block[..8].try_into().unwrap()),
                *expected
            );
        }
        assert_eq!(pool.stats().unwrap().live_blocks, live.len());
    }
    drop(live);
    pool.close().unwrap();
}
