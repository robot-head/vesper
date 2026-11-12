//! Region-backed vectors and strings: bounds, growth, finalization, handle
//! validity and UTF-8 preservation.
use std::alloc::Layout;
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::rc::Rc;
use vesper_alloc::{
    AllocError, ChunkBacking, HandleError, Region, RegionConfig, RegionError, RegionVec,
    RewindError, StringError, SystemBacking,
};

// ----- fixtures ---------------------------------------------------------------

#[derive(Default)]
struct Ledger {
    allocations: Cell<usize>,
    live: Cell<usize>,
    fail_after: Cell<Option<usize>>,
}

#[derive(Clone, Default)]
struct Counting(Rc<Ledger>);

unsafe impl ChunkBacking for Counting {
    fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let l = &self.0;
        if l.fail_after.get().is_some_and(|n| l.allocations.get() >= n) {
            return None;
        }
        let ptr = SystemBacking.allocate(layout)?;
        l.allocations.set(l.allocations.get() + 1);
        l.live.set(l.live.get() + 1);
        Some(ptr)
    }

    unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) {
        self.0.live.set(self.0.live.get() - 1);
        unsafe { SystemBacking.deallocate(ptr, layout) }
    }
}

type Log = Rc<RefCell<Vec<u32>>>;

/// Non-`Copy` payload that records its id when dropped.
#[derive(Debug)]
struct Tracked {
    id: u32,
    log: Log,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.log.borrow_mut().push(self.id);
    }
}

fn t(id: u32, log: &Log) -> Tracked {
    Tracked {
        id,
        log: log.clone(),
    }
}

fn ids(v: &[Tracked]) -> Vec<u32> {
    v.iter().map(|x| x.id).collect()
}

fn sorted(log: &Log) -> Vec<u32> {
    let mut v = log.borrow().clone();
    v.sort_unstable();
    v
}

fn small(max_reserved: usize) -> RegionConfig {
    RegionConfig {
        initial_chunk_bytes: 64,
        max_chunk_bytes: 256,
        max_reserved_bytes: max_reserved,
        max_align: 4096,
        max_allocations: 1 << 16,
        max_checkpoints: 16,
        retained_bytes: 64,
    }
}

fn counting(config: RegionConfig) -> (Region<Counting>, Rc<Ledger>) {
    let backing = Counting::default();
    let ledger = backing.0.clone();
    (Region::with_backing(config, backing).unwrap(), ledger)
}

fn addr<T>(r: &Region<Counting>, v: &RegionVec<T>) -> usize {
    r.vec_as_slice(v).unwrap().as_ptr().addr()
}

// ----- construction, capacity and checked arithmetic ---------------------------

#[test]
fn empty_vectors_have_zero_capacity_and_views() {
    let (mut r, _) = counting(small(1024));
    let mut v = r.vec_new::<u64>().unwrap();
    assert_eq!(r.vec_len(&v), Ok(0));
    assert_eq!(r.vec_capacity(&v), Ok(0));
    assert_eq!(r.vec_is_empty(&v), Ok(true));
    assert!(r.vec_as_slice(&v).unwrap().is_empty());
    assert_eq!(r.vec_get(&v, 0), Ok(None));
    assert_eq!(r.vec_pop(&mut v), Ok(None));
    assert_eq!(r.stats().used_bytes, 0);
    r.vec_push(&mut v, 7).unwrap();
    assert_eq!(r.vec_capacity(&v), Ok(4), "amortized minimum capacity");
    assert_eq!(r.vec_as_slice(&v).unwrap(), [7]);
}

#[test]
fn capacity_arithmetic_is_checked() {
    let (mut r, _) = counting(small(1024));
    let before = r.stats();
    assert_eq!(
        r.vec_with_capacity::<u64>(usize::MAX).unwrap_err(),
        AllocError::SizeOverflow
    );
    assert_eq!(
        r.vec_with_capacity::<u64>(isize::MAX as usize / 8 + 1)
            .unwrap_err(),
        AllocError::SizeOverflow
    );
    assert!(matches!(
        r.vec_with_capacity::<u64>(isize::MAX as usize / 8),
        Err(AllocError::CapacityExceeded { .. })
    ));
    assert!(matches!(
        r.string_with_capacity(isize::MAX as usize + 1),
        Err(AllocError::SizeOverflow)
    ));
    assert_eq!(r.stats(), before, "failed construction changes nothing");

    let mut v = r.vec_with_capacity::<u32>(2).unwrap();
    r.vec_push(&mut v, 1).unwrap();
    assert_eq!(
        r.vec_reserve(&mut v, usize::MAX),
        Err(RegionError::Alloc(AllocError::SizeOverflow))
    );
    assert_eq!(
        r.vec_reserve_exact(&mut v, isize::MAX as usize),
        Err(RegionError::Alloc(AllocError::SizeOverflow))
    );
    assert_eq!(r.vec_as_slice(&v).unwrap(), [1]);
    assert_eq!(r.vec_capacity(&v), Ok(2));
}

#[test]
fn exact_and_maximum_capacities_fit_the_reservation() {
    // 256 bytes of reservation hold exactly 32 u64s in one dedicated chunk.
    let cfg = RegionConfig {
        initial_chunk_bytes: 256,
        max_chunk_bytes: 256,
        max_reserved_bytes: 256,
        retained_bytes: 256,
        ..small(256)
    };
    let (mut r, _) = counting(cfg);
    let mut v = r.vec_with_capacity::<u64>(32).unwrap();
    for i in 0..32 {
        r.vec_push(&mut v, i).unwrap();
    }
    let err = r.vec_push(&mut v, 32).unwrap_err();
    assert_eq!(err.value, 32);
    assert!(matches!(
        err.error,
        RegionError::Alloc(AllocError::CapacityExceeded { .. })
    ));
    assert_eq!(r.vec_len(&v), Ok(32));
    assert_eq!(r.vec_as_slice(&v).unwrap(), (0..32).collect::<Vec<_>>());
}

#[test]
fn zero_sized_elements_have_unbounded_capacity_and_still_drop() {
    thread_local!(static DROPS: Cell<usize> = const { Cell::new(0) });
    struct Unit;
    impl Drop for Unit {
        fn drop(&mut self) {
            DROPS.with(|d| d.set(d.get() + 1));
        }
    }
    let (mut r, _) = counting(small(1024));
    let mut v = r.vec_with_capacity::<Unit>(0).unwrap();
    assert_eq!(r.vec_capacity(&v), Ok(usize::MAX));
    for _ in 0..1000 {
        r.vec_push(&mut v, Unit).unwrap();
    }
    assert_eq!(r.stats().used_bytes, 0, "no element bytes");
    assert_eq!(r.vec_len(&v), Ok(1000));
    assert_eq!(
        r.vec_reserve(&mut v, usize::MAX),
        Err(RegionError::Alloc(AllocError::SizeOverflow))
    );
    drop(r.vec_pop(&mut v).unwrap());
    assert_eq!(DROPS.with(Cell::get), 1);
    r.vec_truncate(&v, 900).unwrap();
    assert_eq!(DROPS.with(Cell::get), 100);
    drop(r);
    assert_eq!(
        DROPS.with(Cell::get),
        1000,
        "region drop finalizes the rest"
    );

    // Over-aligned zero-sized elements honor their alignment.
    #[repr(align(1024))]
    struct Wide;
    let (mut r, _) = counting(small(4096));
    r.alloc_bytes(3, 1).unwrap();
    let mut w = r.vec_new::<Wide>().unwrap();
    r.vec_push(&mut w, Wide).unwrap();
    assert_eq!(r.vec_as_slice(&w).unwrap().as_ptr().addr() % 1024, 0);
}

#[test]
fn over_aligned_elements_stay_aligned_across_growth() {
    #[repr(align(256))]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Line(u32);
    let (mut r, _) = counting(small(16 * 1024));
    r.alloc_bytes(5, 1).unwrap();
    let mut v = r.vec_new::<Line>().unwrap();
    let mut blocker = None;
    for i in 0..20 {
        r.vec_push(&mut v, Line(i)).unwrap();
        assert_eq!(addr(&r, &v) % 256, 0);
        if i == 2 {
            // Force relocation rather than in-place tip growth.
            blocker = Some(r.alloc_bytes(1, 1).unwrap());
        }
    }
    assert!(blocker.is_some());
    assert_eq!(
        r.vec_as_slice(&v).unwrap(),
        (0..20).map(Line).collect::<Vec<_>>()
    );

    #[repr(align(8192))]
    struct TooWide;
    assert_eq!(
        r.vec_new::<TooWide>().unwrap_err(),
        AllocError::AlignmentTooLarge {
            align: 8192,
            max: 4096
        }
    );
}

// ----- growth ---------------------------------------------------------------------

#[test]
fn tip_vectors_grow_in_place_and_others_relocate() {
    let (mut r, _) = counting(small(4096));
    let mut v = r.vec_with_capacity::<u16>(1).unwrap();
    r.vec_push(&mut v, 1).unwrap();
    let start = addr(&r, &v);
    r.vec_reserve_exact(&mut v, 10).unwrap();
    assert_eq!(addr(&r, &v), start, "tip growth is in place");
    assert_eq!(r.vec_capacity(&v), Ok(11));

    let other = r.alloc_bytes(1, 1).unwrap();
    let tracked = r.stats().tracked_allocations;
    r.vec_reserve_exact(&mut v, 20).unwrap();
    assert_ne!(addr(&r, &v), start, "non-tip growth relocates");
    assert_eq!(r.stats().tracked_allocations, tracked + 1);
    assert_eq!(r.vec_as_slice(&v).unwrap(), [1]);
    assert_eq!(r.bytes(&other).unwrap(), [0]);
}

#[test]
fn amortized_growth_falls_back_to_the_exact_requirement() {
    // 64-byte reservation: a vector of 7 u64 cannot double to 14, but can
    // still grow to exactly 8.
    let cfg = RegionConfig {
        initial_chunk_bytes: 64,
        max_chunk_bytes: 64,
        max_reserved_bytes: 64,
        ..small(64)
    };
    let (mut r, _) = counting(cfg);
    let mut v = r.vec_with_capacity::<u64>(7).unwrap();
    for i in 0..8 {
        r.vec_push(&mut v, i).unwrap();
    }
    assert_eq!(r.vec_capacity(&v), Ok(8));
    assert_eq!(r.vec_as_slice(&v).unwrap(), (0..8).collect::<Vec<_>>());
}

#[test]
fn failed_growth_returns_the_value_and_preserves_elements() {
    let log = Log::default();
    let (mut r, ledger) = counting(small(1 << 16));
    let mut v = r.vec_with_capacity::<Tracked>(2).unwrap();
    r.vec_push(&mut v, t(1, &log)).unwrap();
    r.vec_push(&mut v, t(2, &log)).unwrap();
    let used = r.stats().used_bytes;
    r.alloc_bytes(64 - used, 1).unwrap(); // fills chunk 0: growth needs a new chunk
    let before = r.stats();

    // Backing failure.
    ledger.fail_after.set(Some(ledger.allocations.get()));
    let err = r.vec_push(&mut v, t(3, &log)).unwrap_err();
    assert!(matches!(
        err.error,
        RegionError::Alloc(AllocError::OutOfMemory { .. })
    ));
    assert_eq!(err.value.id, 3);
    assert!(log.borrow().is_empty(), "nothing was dropped");
    assert_eq!(ids(r.vec_as_slice(&v).unwrap()), [1, 2]);
    assert_eq!(r.vec_capacity(&v), Ok(2));
    assert_eq!(r.stats(), before);
    drop(err);
    assert_eq!(*log.borrow(), [3], "the returned value is the caller's");
    ledger.fail_after.set(None);

    // Allocation-table limit.
    let cfg = RegionConfig {
        max_allocations: 1,
        ..small(4096)
    };
    let (mut r, _) = counting(cfg);
    let mut v = r.vec_with_capacity::<Tracked>(1).unwrap();
    r.vec_push(&mut v, t(10, &log)).unwrap();
    let mark = r.checkpoint().unwrap();
    let err = r.vec_push(&mut v, t(11, &log)).unwrap_err();
    assert_eq!(err.error, RegionError::ProtectedByWatermark);
    drop(err);
    r.commit(mark).unwrap();
    // Not the tip of a fitting chunk after a raw allocation: must relocate.
    r.alloc_raw(1, 1).unwrap();
    let err = r.vec_push(&mut v, t(12, &log)).unwrap_err();
    assert_eq!(
        err.error,
        RegionError::Alloc(AllocError::TooManyAllocations { limit: 1 })
    );
    assert_eq!(ids(r.vec_as_slice(&v).unwrap()), [10]);
    drop((err, r));
    assert_eq!(sorted(&log), [3, 10, 11, 12]);
}

#[test]
fn relocation_moves_elements_without_dropping_or_duplicating() {
    let log = Log::default();
    let (mut r, ledger) = counting(small(1 << 16));
    let mut v = r.vec_new::<Tracked>().unwrap();
    let mut blockers = Vec::new();
    for i in 0..50 {
        r.vec_push(&mut v, t(i, &log)).unwrap();
        if i % 7 == 0 {
            blockers.push(r.alloc_bytes(3, 1).unwrap());
        }
    }
    assert!(log.borrow().is_empty(), "growth never drops elements");
    assert_eq!(
        ids(r.vec_as_slice(&v).unwrap()),
        (0..50).collect::<Vec<_>>()
    );
    drop(r);
    assert_eq!(sorted(&log), (0..50).collect::<Vec<_>>());
    assert_eq!(ledger.live.get(), 0);
}

// ----- removal, finalization order and exactly-once drops ------------------------

#[test]
fn every_initialized_value_drops_or_is_returned_exactly_once() {
    let log = Log::default();
    let (mut r, ledger) = counting(small(1 << 16));
    let mut a = r.vec_new::<Tracked>().unwrap();
    for i in 0..6 {
        r.vec_push(&mut a, t(i, &log)).unwrap();
    }
    // pop returns ownership; the caller drops it.
    let popped = r.vec_pop(&mut a).unwrap().unwrap();
    assert_eq!(popped.id, 5);
    assert!(log.borrow().is_empty());
    drop(popped);
    // truncate drops the tail highest index first.
    r.vec_truncate(&a, 2).unwrap();
    assert_eq!(*log.borrow(), [5, 4, 3, 2]);
    r.vec_truncate(&a, 10).unwrap(); // longer: no-op
    assert_eq!(r.vec_len(&a), Ok(2));

    // A vector allocated after a watermark is finalized by its rewind.
    let mark = r.checkpoint().unwrap();
    let mut b = r.vec_new::<Tracked>().unwrap();
    for i in 10..13 {
        r.vec_push(&mut b, t(i, &log)).unwrap();
    }
    r.rewind(mark).unwrap();
    assert_eq!(log.borrow()[4..], [12, 11, 10]);
    assert_eq!(r.vec_len(&b), Err(HandleError::Stale));

    // reset finalizes everything else.
    let mut c = r.vec_new::<Tracked>().unwrap();
    r.vec_push(&mut c, t(20, &log)).unwrap();
    r.reset().unwrap();
    assert_eq!(r.vec_len(&a), Err(HandleError::Stale));
    assert_eq!(r.vec_len(&c), Err(HandleError::Stale));

    // region drop finalizes vectors LIFO, each highest index first.
    let mut d = r.vec_new::<Tracked>().unwrap();
    r.vec_push(&mut d, t(30, &log)).unwrap();
    r.vec_push(&mut d, t(31, &log)).unwrap();
    let mut e = r.vec_new::<Tracked>().unwrap();
    r.vec_push(&mut e, t(40, &log)).unwrap();
    drop(r);
    let all = log.borrow().clone();
    assert_eq!(all[all.len() - 3..], [40, 31, 30]);
    assert_eq!(sorted(&log), [0, 1, 2, 3, 4, 5, 10, 11, 12, 20, 30, 31, 40]);
    assert_eq!(ledger.live.get(), 0);
}

#[test]
fn clear_release_and_drain_have_defined_ownership() {
    let log = Log::default();
    let (mut r, _) = counting(small(1 << 16));
    let mut v = r.vec_new::<Tracked>().unwrap();
    for i in 0..4 {
        r.vec_push(&mut v, t(i, &log)).unwrap();
    }
    let cap = r.vec_capacity(&v).unwrap();
    let mut out = Vec::new();
    r.vec_drain(&mut v, |x| out.push(x)).unwrap();
    assert_eq!(ids(&out), [0, 1, 2, 3], "front to back");
    assert_eq!(r.vec_len(&v), Ok(0));
    assert_eq!(r.vec_capacity(&v), Ok(cap));
    assert!(log.borrow().is_empty());
    drop(out);

    r.vec_push(&mut v, t(10, &log)).unwrap();
    r.vec_push(&mut v, t(11, &log)).unwrap();
    r.vec_clear(&v).unwrap();
    assert_eq!(log.borrow()[4..], [11, 10]);

    r.vec_push(&mut v, t(20, &log)).unwrap();
    let used = r.stats().used_bytes;
    let tracked = r.stats().tracked_allocations;
    r.vec_release(v).unwrap();
    assert_eq!(log.borrow().last(), Some(&20));
    assert!(r.stats().used_bytes < used, "tip release reuses bytes");
    assert_eq!(r.stats().tracked_allocations, tracked - 1);
    drop(r);
    assert_eq!(sorted(&log), [0, 1, 2, 3, 10, 11, 20]);
}

#[test]
fn panicking_destructors_and_sinks_never_double_drop() {
    struct Bomb {
        id: u32,
        log: Log,
        explode: bool,
    }
    impl Drop for Bomb {
        fn drop(&mut self) {
            self.log.borrow_mut().push(self.id);
            if self.explode {
                panic!("bomb {}", self.id);
            }
        }
    }
    let bomb = |id, log: &Log, explode| Bomb {
        id,
        log: log.clone(),
        explode,
    };
    let log = Log::default();
    let (mut r, ledger) = counting(small(1 << 16));
    let mut v = r.vec_new::<Bomb>().unwrap();
    for i in 0..5 {
        r.vec_push(&mut v, bomb(i, &log, i == 3)).unwrap();
    }
    let res = catch_unwind(AssertUnwindSafe(|| r.vec_truncate(&v, 1)));
    assert!(res.is_err());
    assert_eq!(*log.borrow(), [4, 3, 2, 1], "lower elements still dropped");
    assert_eq!(r.vec_len(&v), Ok(1));

    // A panicking sink: elements not yet passed are dropped once.
    for i in 10..14 {
        r.vec_push(&mut v, bomb(i, &log, false)).unwrap();
    }
    log.borrow_mut().clear();
    let res = catch_unwind(AssertUnwindSafe(|| {
        r.vec_drain(&mut v, |b| {
            if b.id == 10 {
                panic!("sink");
            }
        })
    }));
    assert!(res.is_err());
    assert_eq!(sorted(&log), [0, 10, 11, 12, 13]);
    assert_eq!(r.vec_len(&v), Ok(0));

    // A panicking element during region drop.
    for i in 20..23 {
        r.vec_push(&mut v, bomb(i, &log, i == 21)).unwrap();
    }
    log.borrow_mut().clear();
    let res = catch_unwind(AssertUnwindSafe(move || drop(r)));
    assert!(res.is_err());
    assert_eq!(*log.borrow(), [22, 21, 20]);
    assert_eq!(ledger.live.get(), 0, "chunks still returned");
}

// ----- watermarks, rewind and handle validity ---------------------------------------

#[test]
fn pre_watermark_vectors_keep_reserved_capacity_and_survive_rewind() {
    let log = Log::default();
    let (mut r, _) = counting(small(1 << 16));
    let mut entries = r.vec_with_capacity::<Tracked>(0).unwrap();
    r.vec_reserve_exact(&mut entries, 3).unwrap(); // reserve before the window
    let before = addr(&r, &entries);
    for batch in 0..3 {
        let mark = r.checkpoint().unwrap();
        let mut scratch = r.vec_new::<u64>().unwrap();
        r.vec_extend_from_slice(&mut scratch, &[1, 2, 3]).unwrap();
        r.vec_push(&mut entries, t(batch, &log)).unwrap();
        r.rewind(mark).unwrap();
        assert_eq!(r.vec_len(&scratch), Err(HandleError::Stale));
    }
    assert_eq!(addr(&r, &entries), before);
    assert_eq!(ids(r.vec_as_slice(&entries).unwrap()), [0, 1, 2]);
    assert!(
        log.borrow().is_empty(),
        "elements pushed in the window survive"
    );

    // Growth beyond the reserved capacity inside the window is refused.
    let mark = r.checkpoint().unwrap();
    let err = r.vec_push(&mut entries, t(3, &log)).unwrap_err();
    assert_eq!(err.error, RegionError::ProtectedByWatermark);
    assert_eq!(err.value.id, 3);
    assert_eq!(r.vec_len(&entries), Ok(3));
    r.commit(mark).unwrap();
    drop(err);
    r.vec_push(&mut entries, t(4, &log)).unwrap();
    assert_eq!(ids(r.vec_as_slice(&entries).unwrap()), [0, 1, 2, 4]);
}

#[test]
fn stale_vectors_are_rejected_after_slot_and_address_reuse() {
    let (mut r, _) = counting(small(4096));
    let mark = r.checkpoint().unwrap();
    let mut old = r.vec_with_capacity::<u32>(4).unwrap();
    r.vec_push(&mut old, 7).unwrap();
    let old_addr = addr(&r, &old);
    r.rewind(mark).unwrap();
    let mut new = r.vec_with_capacity::<u32>(4).unwrap();
    r.vec_push(&mut new, 9).unwrap();
    assert_eq!(addr(&r, &new), old_addr, "same slot and bytes reused");
    assert_eq!(r.vec_as_slice(&old), Err(HandleError::Stale));
    assert_eq!(r.vec_get_mut(&old, 0), Err(HandleError::Stale));
    let err = r.vec_push(&mut old, 1).unwrap_err();
    assert_eq!(
        (err.value, err.error),
        (1, RegionError::Handle(HandleError::Stale))
    );
    assert_eq!(r.vec_pop(&mut old), Err(HandleError::Stale));
    let err = r.vec_release(old).unwrap_err();
    assert_eq!(err.error, RegionError::Handle(HandleError::Stale));
    assert_eq!(r.vec_as_slice(&new).unwrap(), [9]);

    // Reset invalidates too, even when the slot index is reused.
    r.reset().unwrap();
    let fresh = r.vec_with_capacity::<u32>(4).unwrap();
    assert_eq!(r.vec_len(&new), Err(HandleError::Stale));
    assert_eq!(r.vec_len(&fresh), Ok(0));
}

#[test]
fn foreign_handles_are_rejected() {
    let (mut a, _) = counting(small(4096));
    let (mut b, _) = counting(small(4096));
    let mut va = a.vec_new::<u8>().unwrap();
    let vb = b.vec_new::<u8>().unwrap();
    let cross = HandleError::CrossRegion {
        expected: b.id(),
        found: a.id(),
    };
    assert_eq!(b.vec_len(&va), Err(cross));
    assert_eq!(b.vec_as_mut_slice(&va).map(|s| s.len()), Err(cross));
    assert_eq!(
        b.vec_push(&mut va, 1).unwrap_err().error,
        RegionError::Handle(cross)
    );
    let sa = a.string_from_str("a").unwrap();
    assert_eq!(b.string_as_str(&sa), Err(cross));
    let frozen = a.vec_freeze(va).unwrap();
    assert_eq!(b.slice(&frozen), Err(cross));
    assert_eq!(b.slice_release(frozen), Err(cross));
    assert_eq!(b.vec_len(&vb), Ok(0));
    assert_eq!(a.slice_len(&frozen), Ok(0));
}

#[test]
fn copied_slice_aliases_are_read_only_and_invalidated_together() {
    let log = Log::default();
    let (mut r, _) = counting(small(4096));
    let mut v = r.vec_new::<Tracked>().unwrap();
    r.vec_push(&mut v, t(1, &log)).unwrap();
    r.vec_push(&mut v, t(2, &log)).unwrap();
    let s1 = r.vec_freeze(v).unwrap();
    let s2 = s1; // copy
    let (x, y) = (r.slice(&s1).unwrap(), r.slice(&s2).unwrap());
    assert_eq!(x.as_ptr(), y.as_ptr(), "copies alias the same elements");
    assert_eq!(ids(x), [1, 2]);
    r.slice_release(s1).unwrap();
    assert_eq!(*log.borrow(), [2, 1]);
    assert_eq!(r.slice(&s2).err(), Some(HandleError::Stale));
    assert_eq!(r.slice_release(s2), Err(HandleError::Stale));

    // Rewind invalidates every copy of a post-watermark slice.
    let mark = r.checkpoint().unwrap();
    let mut w = r.vec_new::<u8>().unwrap();
    r.vec_push(&mut w, 5).unwrap();
    let s3 = r.vec_freeze(w).unwrap();
    let s4 = s3;
    r.rewind(mark).unwrap();
    assert_eq!(r.slice_len(&s3), Err(HandleError::Stale));
    assert_eq!(r.slice_len(&s4), Err(HandleError::Stale));
    drop(r);
    assert_eq!(*log.borrow(), [2, 1], "released elements drop only once");
}

// ----- linear elements ------------------------------------------------------------------

#[test]
fn linear_elements_block_discard_until_drained() {
    let log = Log::default();
    let (mut r, _) = counting(small(1 << 16));
    let mark = r.checkpoint().unwrap();
    let mut v = r.vec_with_capacity_linear::<Tracked>(1).unwrap();
    assert_eq!(
        r.stats().live_linear,
        0,
        "an empty linear vector owes nothing"
    );
    r.vec_push(&mut v, t(1, &log)).unwrap();
    r.alloc_bytes(1, 1).unwrap();
    r.vec_push(&mut v, t(2, &log)).unwrap(); // relocates
    assert_eq!(r.stats().live_linear, 1, "obligation moved, not duplicated");

    assert_eq!(
        r.vec_truncate(&v, 1),
        Err(RegionError::LinearObligationsLive { count: 1 })
    );
    assert_eq!(
        r.reset(),
        Err(RegionError::LinearObligationsLive { count: 1 })
    );
    let mark = match r.rewind(mark) {
        Err(RewindError::LinearObligationsLive { mark, count: 1 }) => mark,
        other => panic!("unexpected {other:?}"),
    };
    let v = r.vec_release(v).unwrap_err().owner;
    let mut v = r.vec_freeze(v).unwrap_err().owner;
    assert_eq!(ids(r.vec_as_slice(&v).unwrap()), [1, 2]);
    assert!(log.borrow().is_empty());

    assert_eq!(r.vec_pop(&mut v).unwrap().map(|x| x.id), Some(2));
    assert_eq!(r.stats().live_linear, 1);
    let mut out = Vec::new();
    r.vec_drain(&mut v, |x| out.push(x)).unwrap();
    assert_eq!(r.stats().live_linear, 0);
    assert_eq!(ids(&out), [1]);
    r.vec_clear(&v).unwrap(); // nothing left to discard
    r.rewind(mark).unwrap();
    assert_eq!(r.vec_len(&v), Err(HandleError::Stale));
    drop(out);
    assert_eq!(sorted(&log), [1, 2]);
}

#[test]
fn close_reports_abandoned_linear_elements_and_finalizes_them() {
    let log = Log::default();
    let mut r = Region::new(small(4096)).unwrap();
    let mut v = r.vec_with_capacity_linear::<Tracked>(2).unwrap();
    r.vec_push(&mut v, t(1, &log)).unwrap();
    r.vec_push(&mut v, t(2, &log)).unwrap();
    assert_eq!(
        r.close().unwrap_err(),
        RegionError::LinearObligationsLive { count: 1 }
    );
    assert_eq!(*log.borrow(), [2, 1]);
}

// ----- strings --------------------------------------------------------------------------

#[test]
fn invalid_utf8_is_rejected_without_allocating() {
    let (mut r, _) = counting(small(4096));
    let before = r.stats();
    let invalid: [&[u8]; 6] = [
        b"\xff",
        b"\xc0\x80",         // overlong NUL
        b"\xed\xa0\x80",     // surrogate
        b"\xe2\x82",         // truncated euro sign
        b"ok\x80",           // stray continuation
        b"\xf4\x90\x80\x80", // above U+10FFFF
    ];
    for bytes in invalid {
        assert!(matches!(
            r.string_from_utf8(bytes),
            Err(StringError::Utf8(_))
        ));
    }
    assert_eq!(r.stats(), before);

    let mut s = r.string_from_utf8("€uro".as_bytes()).unwrap();
    for bytes in invalid {
        assert!(matches!(
            r.string_push_utf8(&mut s, bytes),
            Err(StringError::Utf8(_))
        ));
        assert_eq!(r.string_as_str(&s), Ok("€uro"));
    }
    // A split multibyte sequence is invalid even if the halves are joined
    // later; each append must be valid on its own.
    let euro = "€".as_bytes();
    assert!(r.string_push_utf8(&mut s, &euro[..1]).is_err());
    r.string_push_utf8(&mut s, euro).unwrap();
    assert_eq!(r.string_as_str(&s), Ok("€uro€"));
}

#[test]
fn multibyte_operations_preserve_char_boundaries() {
    let (mut r, _) = counting(small(4096));
    let mut s = r.string_new().unwrap();
    r.string_push_str(&mut s, "aé").unwrap();
    r.string_push(&mut s, '€').unwrap();
    r.string_push(&mut s, '𝄞').unwrap();
    assert_eq!(r.string_as_str(&s), Ok("aé€𝄞"));
    assert_eq!(r.string_len(&s), Ok(1 + 2 + 3 + 4));
    assert_eq!(r.string_as_bytes(&s).unwrap(), "aé€𝄞".as_bytes());

    for bad in [2, 4, 5, 7, 8, 9] {
        assert_eq!(
            r.string_truncate(&s, bad),
            Err(StringError::NotCharBoundary { index: bad })
        );
        assert_eq!(r.string_as_str(&s), Ok("aé€𝄞"));
    }
    assert_eq!(r.string_pop(&mut s), Ok(Some('𝄞')));
    assert_eq!(r.string_pop(&mut s), Ok(Some('€')));
    r.string_truncate(&s, 1).unwrap();
    assert_eq!(r.string_as_str(&s), Ok("a"));
    r.string_truncate(&s, 100).unwrap();
    assert_eq!(r.string_pop(&mut s), Ok(Some('a')));
    assert_eq!(r.string_pop(&mut s), Ok(None));
    assert_eq!(r.string_is_empty(&s), Ok(true));

    // `&mut str` allows only UTF-8-preserving edits.
    r.string_push_str(&mut s, "grüße").unwrap();
    r.string_as_mut_str(&s).unwrap().make_ascii_uppercase();
    assert_eq!(r.string_as_str(&s), Ok("GRüßE"));
    r.string_clear(&s).unwrap();
    assert_eq!(r.string_as_str(&s), Ok(""));
}

#[test]
fn string_growth_failure_leaves_the_string_unchanged() {
    let (mut r, _) = counting(small(128));
    let mut s = r.string_from_str("héllo").unwrap();
    let blocker = r.alloc_bytes(1, 1).unwrap();
    let big = "ü".repeat(64); // 128 bytes: cannot fit beside the rest
    let err = r.string_push_str(&mut s, &big).unwrap_err();
    assert!(matches!(
        err,
        RegionError::Alloc(AllocError::CapacityExceeded { .. })
    ));
    assert_eq!(r.string_as_str(&s), Ok("héllo"));
    // Relocating growth that fits keeps content and validity.
    r.string_push_str(&mut s, " wörld").unwrap();
    assert_eq!(r.string_as_str(&s), Ok("héllo wörld"));
    assert_eq!(r.bytes(&blocker).unwrap(), [0]);
    let cap = r.string_capacity(&s).unwrap();
    r.string_reserve_exact(&mut s, 0).unwrap();
    assert_eq!(r.string_capacity(&s), Ok(cap));
    r.string_reserve(&mut s, 1).unwrap();
}

#[test]
fn frozen_strings_are_copyable_and_go_stale_together() {
    let (mut r, _) = counting(small(4096));
    let mark = r.checkpoint().unwrap();
    let s = r.string_from_str("ζeta").unwrap();
    let f1 = r.string_freeze(s).unwrap();
    let f2 = f1;
    assert_eq!(r.str_as_str(&f1), Ok("ζeta"));
    assert_eq!(r.str_len(&f2), Ok(5));
    r.rewind(mark).unwrap();
    assert_eq!(r.str_as_str(&f1), Err(HandleError::Stale));
    assert_eq!(r.str_release(f2), Err(HandleError::Stale));

    let s = r.string_from_str("x").unwrap();
    let f = r.string_freeze(s).unwrap();
    r.str_release(f).unwrap();
    assert_eq!(r.str_as_str(&f), Err(HandleError::Stale));
    let s = r.string_from_str("y").unwrap();
    r.string_release(s).unwrap();
    assert_eq!(r.stats().used_bytes, 0);
}

// ----- model check ------------------------------------------------------------------------

/// Deterministic randomized check of vector operations against a `Vec`
/// model, with watermarks, rewinds and drop accounting.
#[test]
fn randomized_vectors_match_a_model_and_drop_exactly_once() {
    let steps = if cfg!(miri) { 200 } else { 5000 };
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut rand = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let log = Log::default();
    let (mut r, ledger) = counting(small(1 << 14));
    // (handle, model, depth at which it was created)
    let mut vecs: Vec<(RegionVec<Tracked>, Vec<u32>, usize)> = Vec::new();
    let mut marks = Vec::new();
    let mut next_id = 0u32;
    let mut alive: Vec<u32> = Vec::new(); // ids owned by the region
    for _ in 0..steps {
        match rand(10) {
            0 if vecs.len() < 6 => {
                if let Ok(v) = r.vec_with_capacity::<Tracked>(rand(4) as usize) {
                    vecs.push((v, Vec::new(), marks.len()));
                }
            }
            1..=3 if !vecs.is_empty() => {
                let i = rand(vecs.len() as u64) as usize;
                let id = next_id;
                next_id += 1;
                match r.vec_push(&mut vecs[i].0, t(id, &log)) {
                    Ok(()) => {
                        vecs[i].1.push(id);
                        alive.push(id);
                    }
                    Err(e) => {
                        assert_eq!(e.value.id, id);
                        drop(e); // the caller's value
                    }
                }
            }
            4 if !vecs.is_empty() => {
                let i = rand(vecs.len() as u64) as usize;
                let got = r.vec_pop(&mut vecs[i].0).unwrap().map(|x| x.id);
                let want = vecs[i].1.pop();
                assert_eq!(got, want);
                alive.retain(|a| Some(*a) != want);
            }
            5 if !vecs.is_empty() => {
                let i = rand(vecs.len() as u64) as usize;
                let n = rand(4) as usize;
                r.vec_truncate(&vecs[i].0, n).unwrap();
                let model = &mut vecs[i].1;
                let removed: Vec<u32> = model.drain(n.min(model.len())..).collect();
                alive.retain(|a| !removed.contains(a));
            }
            6 if !vecs.is_empty() => {
                let i = rand(vecs.len() as u64) as usize;
                let _ = r.vec_reserve(&mut vecs[i].0, rand(8) as usize);
            }
            7 if marks.len() < 4 => marks.push(r.checkpoint().unwrap()),
            8 if !marks.is_empty() => {
                let mark = marks.pop().unwrap();
                r.rewind(mark).unwrap();
                let depth = marks.len();
                let (gone, kept): (Vec<_>, Vec<_>) =
                    vecs.drain(..).partition(|(_, _, d)| *d > depth);
                for (v, model, _) in gone {
                    assert_eq!(r.vec_len(&v), Err(HandleError::Stale));
                    alive.retain(|a| !model.contains(a));
                }
                vecs = kept;
            }
            9 if !vecs.is_empty() && rand(3) == 0 => {
                let i = rand(vecs.len() as u64) as usize;
                let (v, model, _) = vecs.swap_remove(i);
                r.vec_release(v).unwrap();
                alive.retain(|a| !model.contains(a));
            }
            _ => {}
        }
        for (v, model, _) in &vecs {
            assert_eq!(ids(r.vec_as_slice(v).unwrap()), *model);
        }
        let mut dropped = sorted(&log);
        dropped.dedup();
        assert_eq!(dropped.len(), log.borrow().len(), "no double drop");
        assert!(alive.iter().all(|a| !log.borrow().contains(a)));
    }
    drop(r);
    assert_eq!(sorted(&log), (0..next_id).collect::<Vec<_>>());
    assert_eq!(ledger.live.get(), 0);
}
