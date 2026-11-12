use std::alloc::Layout;
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::rc::Rc;
use vesper_alloc::{
    AllocError, ChunkBacking, ConfigError, HandleError, Region, RegionBytes, RegionConfig,
    RegionError, RewindError, SystemBacking,
};

// ----- fixtures ---------------------------------------------------------------

#[derive(Default)]
struct Ledger {
    allocations: Cell<usize>,
    live: Cell<usize>,
    live_bytes: Cell<usize>,
    /// Refuse every allocation once this many have succeeded.
    fail_after: Cell<Option<usize>>,
}

/// Counts chunks and can inject allocation failure.
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
        l.live_bytes.set(l.live_bytes.get() + layout.size());
        Some(ptr)
    }

    unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) {
        let l = &self.0;
        l.live.set(l.live.get() - 1);
        l.live_bytes.set(l.live_bytes.get() - layout.size());
        unsafe { SystemBacking.deallocate(ptr, layout) }
    }
}

/// Records its id in a shared log when dropped.
struct Tracked {
    id: u32,
    log: Rc<RefCell<Vec<u32>>>,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.log.borrow_mut().push(self.id);
    }
}

fn tracked(id: u32, log: &Rc<RefCell<Vec<u32>>>) -> Tracked {
    Tracked {
        id,
        log: log.clone(),
    }
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

fn range(region: &Region<Counting>, h: &RegionBytes) -> (usize, usize) {
    let start = region.bytes_ptr(h).unwrap().as_ptr().addr();
    (start, start + region.len(h).unwrap())
}

// ----- configuration ------------------------------------------------------------

#[test]
fn rejects_invalid_configuration() {
    let base = small(1024);
    let cases = [
        (
            RegionConfig {
                initial_chunk_bytes: 0,
                ..base
            },
            ConfigError::ZeroInitialChunk,
        ),
        (
            RegionConfig {
                initial_chunk_bytes: 512,
                ..base
            },
            ConfigError::InitialExceedsMaxChunk,
        ),
        (
            RegionConfig {
                max_reserved_bytes: 32,
                ..base
            },
            ConfigError::InitialExceedsReservation,
        ),
        (
            RegionConfig {
                max_align: 48,
                ..base
            },
            ConfigError::MaxAlignNotPowerOfTwo,
        ),
        (
            RegionConfig {
                max_chunk_bytes: usize::MAX,
                ..base
            },
            ConfigError::ChunkLayout,
        ),
        (
            RegionConfig {
                max_allocations: u32::MAX as usize + 1,
                ..base
            },
            ConfigError::AllocationLimitTooLarge,
        ),
    ];
    for (config, expected) in cases {
        assert_eq!(Region::new(config).err(), Some(expected));
    }
    assert!(Region::new(RegionConfig::default()).is_ok());
    assert!(Region::new(RegionConfig::bounded(100)).is_ok());
}

#[test]
fn region_creation_is_lazy_and_identities_are_unique() {
    let (a, ledger) = counting(small(1024));
    let b = Region::new(small(1024)).unwrap();
    assert_ne!(a.id(), b.id());
    assert_eq!(ledger.allocations.get(), 0);
    assert_eq!(a.stats().reserved_bytes, 0);
}

// ----- alignment ------------------------------------------------------------------

#[test]
fn allocations_honor_every_supported_alignment() {
    let (mut r, _) = counting(small(1 << 16));
    let mut align = 1;
    while align <= 4096 {
        for len in [0, 1, 3, align, align + 1] {
            r.alloc_bytes(1, 1).unwrap(); // perturb the cursor
            let h = r.alloc_bytes(len, align).unwrap();
            let addr = r.bytes_ptr(&h).unwrap().as_ptr().addr();
            assert_eq!(addr % align, 0, "align {align} len {len}");
            assert_eq!(r.bytes(&h).unwrap(), vec![0; len].as_slice());
        }
        align *= 2;
    }
}

#[test]
fn typed_values_are_aligned() {
    #[repr(align(256))]
    struct Wide(u8);
    let (mut r, _) = counting(small(4096));
    r.alloc_bytes(3, 1).unwrap();
    let b = r.alloc_value(Wide(7)).unwrap();
    let v = r.get(&b).unwrap();
    assert_eq!((v as *const Wide).addr() % 256, 0);
    assert_eq!(v.0, 7);
    let z = r.alloc_value(()).unwrap();
    assert_eq!(r.take(z), Ok(()));
}

#[test]
fn rejects_invalid_and_oversized_alignment() {
    let (mut r, _) = counting(RegionConfig {
        max_align: 64,
        ..small(1024)
    });
    for align in [0, 3, 24] {
        assert_eq!(
            r.alloc_bytes(8, align),
            Err(AllocError::InvalidAlignment { align })
        );
    }
    assert_eq!(
        r.alloc_bytes(8, 128),
        Err(AllocError::AlignmentTooLarge {
            align: 128,
            max: 64
        })
    );
    assert_eq!(
        r.alloc_raw(8, 3),
        Err(AllocError::InvalidAlignment { align: 3 })
    );
    assert_eq!(r.stats().tracked_allocations, 0);
}

// ----- checked arithmetic and bounded capacity ----------------------------------------

#[test]
fn size_arithmetic_is_checked() {
    let (mut r, ledger) = counting(small(1024));
    assert_eq!(r.alloc_bytes(usize::MAX, 1), Err(AllocError::SizeOverflow));
    assert_eq!(
        r.alloc_bytes(isize::MAX as usize + 1, 1),
        Err(AllocError::SizeOverflow)
    );
    assert!(matches!(
        r.alloc_bytes(isize::MAX as usize, 1),
        Err(AllocError::CapacityExceeded { .. })
    ));
    assert_eq!(ledger.allocations.get(), 0);
    assert_eq!(r.alloc_raw(usize::MAX, 1), Err(AllocError::SizeOverflow));
    assert_eq!(
        r.alloc_value([0u8; 1 << 14]).unwrap_err().error,
        AllocError::CapacityExceeded {
            requested: 1 << 14,
            reserved: 0,
            limit: 1024
        }
    );
    // Alignment padding is part of the checked fit computation.
    let mut h = r.alloc_bytes(1, 1).unwrap();
    assert!(matches!(
        r.alloc_bytes(1020, 1024),
        Err(AllocError::CapacityExceeded { .. })
    ));
    assert_eq!(r.extend_bytes(&mut h, &[]), Ok(()));
    assert_eq!(r.len(&h), Ok(1));
}

#[test]
fn reservation_bound_is_enforced_and_failure_leaves_region_unchanged() {
    let (mut r, ledger) = counting(small(192));
    let a = r.alloc_bytes_copy(&[1; 60], 1).unwrap(); // chunk 64
    let b = r.alloc_bytes_copy(&[2; 100], 1).unwrap(); // chunk 128
    assert_eq!(r.stats().reserved_bytes, 192);
    let before = r.stats();
    let err = r.alloc_bytes(40, 1).unwrap_err();
    assert_eq!(
        err,
        AllocError::CapacityExceeded {
            requested: 40,
            reserved: 192,
            limit: 192
        }
    );
    assert_eq!(r.stats(), before);
    assert_eq!(r.bytes(&a).unwrap(), &[1; 60]);
    assert_eq!(r.bytes(&b).unwrap(), &[2; 100]);
    // Small requests still fit in the remaining space of the active chunk.
    let c = r.alloc_bytes(28, 1).unwrap();
    assert_eq!(r.len(&c), Ok(28));
    assert_eq!(ledger.live.get(), 2);
}

#[test]
fn backing_failure_is_reported_and_value_ownership_returned() {
    let (mut r, ledger) = counting(small(4096));
    let log = Rc::new(RefCell::new(Vec::new()));
    let kept = r.alloc_value(tracked(1, &log)).unwrap();
    ledger.fail_after.set(Some(1));
    let err = r.alloc_value(([0u8; 128], tracked(2, &log))).unwrap_err();
    let chunk_bytes = std::mem::size_of::<([u8; 128], Tracked)>();
    assert_eq!(err.error, AllocError::OutOfMemory { chunk_bytes });
    assert!(
        log.borrow().is_empty(),
        "failed allocation must not drop the value"
    );
    let (_, value) = err.value;
    assert_eq!(value.id, 2);
    drop(value);
    assert_eq!(*log.borrow(), [2]);
    assert_eq!(r.get(&kept).unwrap().id, 1);
    assert_eq!(r.stats().tracked_allocations, 1);
    ledger.fail_after.set(None);
    assert!(r.alloc_bytes(128, 1).is_ok());
    drop(r);
    assert_eq!(*log.borrow(), [2, 1]);
    assert_eq!(ledger.live.get(), 0);
}

#[test]
fn allocation_and_checkpoint_limits_are_enforced() {
    let (mut r, _) = counting(RegionConfig {
        max_allocations: 2,
        max_checkpoints: 1,
        ..small(1024)
    });
    let a = r.alloc_bytes(1, 1).unwrap();
    r.alloc_bytes(1, 1).unwrap();
    assert_eq!(
        r.alloc_bytes(1, 1),
        Err(AllocError::TooManyAllocations { limit: 2 })
    );
    assert_eq!(
        r.alloc_value(5u8).unwrap_err().error,
        AllocError::TooManyAllocations { limit: 2 }
    );
    let m = r.checkpoint().unwrap();
    assert_eq!(
        r.checkpoint().unwrap_err(),
        RegionError::TooManyCheckpoints { limit: 1 }
    );
    r.commit(m).unwrap();
    r.release_bytes(a).unwrap(); // not the tip: slot is retained until reset
    assert!(r.alloc_bytes(1, 1).is_err());
    r.reset().unwrap();
    assert!(r.alloc_bytes(1, 1).is_ok());
}

// ----- growth -----------------------------------------------------------------------

#[test]
fn chunks_grow_geometrically_up_to_the_cap_with_dedicated_oversize_chunks() {
    let (mut r, ledger) = counting(small(4096));
    for _ in 0..40 {
        r.alloc_bytes(16, 1).unwrap();
    }
    // 64 + 128 + 256 + 256 = 704 >= 640 bytes of payload.
    assert_eq!(r.stats().chunks, 4);
    assert_eq!(r.stats().reserved_bytes, 704);
    let big = r.alloc_bytes_copy(&[9; 1000], 1).unwrap();
    assert_eq!(r.stats().reserved_bytes, 1704);
    assert_eq!(r.bytes(&big).unwrap(), &[9; 1000]);
    assert_eq!(ledger.live_bytes.get(), 1704);
}

#[test]
fn tip_buffers_grow_in_place_and_others_relocate() {
    let (mut r, _) = counting(small(4096));
    let mut buf = r.alloc_bytes_copy(b"ab", 8).unwrap();
    let p0 = r.bytes_ptr(&buf).unwrap();
    r.extend_bytes(&mut buf, b"cd").unwrap();
    assert_eq!(r.bytes_ptr(&buf).unwrap(), p0, "tip grows in place");
    assert_eq!(r.stats().tracked_allocations, 1);

    let other = r.alloc_bytes_copy(b"zz", 1).unwrap();
    let old = buf;
    r.extend_bytes(&mut buf, b"ef").unwrap();
    assert_ne!(r.bytes_ptr(&buf).unwrap(), p0, "non-tip relocates");
    assert_eq!(r.bytes(&buf).unwrap(), b"abcdef");
    assert_eq!(r.bytes_ptr(&buf).unwrap().as_ptr().addr() % 8, 0);
    assert_eq!(r.bytes(&old), Err(HandleError::Stale));
    assert_eq!(r.bytes(&other).unwrap(), b"zz");

    // Growth past the active chunk relocates into a new chunk.
    let tail = [7u8; 200];
    r.extend_bytes(&mut buf, &tail).unwrap();
    assert_eq!(&r.bytes(&buf).unwrap()[..6], b"abcdef");
    assert_eq!(&r.bytes(&buf).unwrap()[6..], &tail);
}

#[test]
fn growth_failure_leaves_buffer_intact() {
    let (mut r, _) = counting(small(64));
    let mut buf = r.alloc_bytes_copy(b"hello", 1).unwrap();
    let before = buf;
    let err = r.extend_bytes(&mut buf, &[0; 100]).unwrap_err();
    assert!(matches!(
        err,
        RegionError::Alloc(AllocError::CapacityExceeded { .. })
    ));
    assert_eq!(buf, before);
    assert_eq!(r.bytes(&buf).unwrap(), b"hello");
}

#[test]
fn watermark_protects_older_buffers_from_lossy_growth() {
    let (mut r, _) = counting(small(4096));
    let mut buf = r.alloc_bytes_copy(b"keep", 1).unwrap();
    let mark = r.checkpoint().unwrap();
    assert_eq!(
        r.extend_bytes(&mut buf, b"!"),
        Err(RegionError::ProtectedByWatermark)
    );
    assert_eq!(r.extend_bytes(&mut buf, b""), Ok(()));
    let mut scratch = r.alloc_bytes_copy(b"tmp", 1).unwrap();
    r.extend_bytes(&mut scratch, b"++").unwrap();
    r.rewind(mark).unwrap();
    assert_eq!(r.bytes(&buf).unwrap(), b"keep");
    assert_eq!(r.bytes(&scratch), Err(HandleError::Stale));
    r.extend_bytes(&mut buf, b"!").unwrap();
    assert_eq!(r.bytes(&buf).unwrap(), b"keep!");
}

// ----- overlap ------------------------------------------------------------------------

/// Deterministic model check: random allocate/extend/release/checkpoint/rewind
/// sequences against a shadow model. After every step, every live allocation
/// holds exactly its model contents, and live allocations are pairwise disjoint.
#[test]
fn randomized_operations_never_overlap_and_preserve_contents() {
    let steps = if cfg!(miri) { 150 } else { 4000 };
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut r, ledger) = counting(small(1 << 16));
    // (handle, contents, mark depth at creation)
    let mut live: Vec<(RegionBytes, Vec<u8>, usize)> = Vec::new();
    let mut marks = Vec::new();
    let mut stale = Vec::new();
    // [in-place growth, relocation, protected, release, rewind, commit]
    let mut seen = [0usize; 6];
    for step in 0..steps {
        match next() % 10 {
            0..=3 => {
                let len = (next() % 48) as usize;
                let align = 1 << (next() % 7);
                let fill = (step % 251) as u8;
                let data = vec![fill; len];
                let h = r.alloc_bytes_copy(&data, align).unwrap();
                assert_eq!(r.bytes_ptr(&h).unwrap().as_ptr().addr() % align, 0);
                live.push((h, data, marks.len()));
            }
            4 | 5 if !live.is_empty() => {
                let i = (next() as usize) % live.len();
                let extra = vec![(step % 13) as u8; (next() % 40) as usize];
                let (h, data, depth) = &mut live[i];
                let old = *h;
                match r.extend_bytes(h, &extra) {
                    Ok(()) => {
                        assert!(extra.is_empty() || *depth == marks.len());
                        data.extend_from_slice(&extra);
                        if old != *h {
                            stale.push(old);
                            *depth = marks.len();
                            seen[1] += 1;
                        } else if !extra.is_empty() {
                            seen[0] += 1;
                        }
                    }
                    Err(RegionError::ProtectedByWatermark) => {
                        assert!(*depth < marks.len());
                        seen[2] += 1;
                    }
                    Err(e) => panic!("{e:?}"),
                }
            }
            6 if !live.is_empty() => {
                let i = (next() as usize) % live.len();
                let (h, _, _) = live.swap_remove(i);
                r.release_bytes(h).unwrap();
                stale.push(h);
                seen[3] += 1;
            }
            7 if marks.len() < 8 => marks.push(r.checkpoint().unwrap()),
            8 if !marks.is_empty() => {
                let depth = marks.len() - 1;
                r.rewind(marks.pop().unwrap()).unwrap();
                seen[4] += 1;
                live.retain(|(h, _, d)| {
                    if *d > depth {
                        stale.push(*h);
                    }
                    *d <= depth
                });
            }
            9 if !marks.is_empty() => {
                let mark = marks.pop().unwrap();
                r.commit(mark).unwrap();
                seen[5] += 1;
                let depth = marks.len();
                for (_, _, d) in &mut live {
                    *d = (*d).min(depth);
                }
            }
            _ => {}
        }
        let mut ranges: Vec<_> = live
            .iter()
            .map(|(h, data, _)| {
                assert_eq!(r.bytes(h).unwrap(), data.as_slice(), "step {step}");
                range(&r, h)
            })
            .filter(|(s, e)| s != e)
            .collect();
        ranges.sort();
        for w in ranges.windows(2) {
            assert!(w[0].1 <= w[1].0, "overlap at step {step}: {w:?}");
        }
        for h in &stale {
            assert!(r.bytes(h).is_err(), "stale handle accepted at step {step}");
        }
    }
    assert!(
        seen.iter().all(|n| *n > 0),
        "every path exercised: {seen:?}"
    );
    assert!(r.stats().chunks > 1, "growth exercised");
    drop(r);
    assert_eq!(ledger.live.get(), 0);
}

#[test]
fn mutable_pairs_are_disjoint_and_aliasing_is_rejected() {
    let (mut r, _) = counting(small(1024));
    let a = r.alloc_bytes(4, 1).unwrap();
    let b = r.alloc_bytes(4, 1).unwrap();
    {
        let (x, y) = r.bytes_mut_pair(&a, &b).unwrap();
        x.copy_from_slice(b"aaaa");
        y.copy_from_slice(b"bbbb");
    }
    assert_eq!(r.bytes(&a).unwrap(), b"aaaa");
    assert_eq!(r.bytes(&b).unwrap(), b"bbbb");
    assert_eq!(r.bytes_mut_pair(&a, &a).unwrap_err(), HandleError::Aliased);
    let typed = r.alloc_value(5u32).unwrap();
    *r.get_mut(&typed).unwrap() += 1;
    assert_eq!(r.get(&typed), Ok(&6));
}

// ----- reuse and reclamation -------------------------------------------------------------

#[test]
fn rewind_reuses_bytes_and_keeps_spare_chunks() {
    let (mut r, ledger) = counting(small(4096));
    let keep = r.alloc_bytes_copy(b"keep", 1).unwrap();
    let mark = r.checkpoint().unwrap();
    let first = r.alloc_bytes(8, 8).unwrap();
    let p = r.bytes_ptr(&first).unwrap();
    for _ in 0..20 {
        r.alloc_bytes(32, 1).unwrap();
    }
    let chunks = r.stats().chunks;
    assert!(chunks > 1);
    r.rewind(mark).unwrap();
    assert_eq!(r.stats().tracked_allocations, 1);
    assert_eq!(r.stats().chunks, chunks, "spares retained");
    let again = r.alloc_bytes(8, 8).unwrap();
    assert_eq!(r.bytes_ptr(&again).unwrap(), p, "bytes reused");
    assert_eq!(r.bytes(&first), Err(HandleError::Stale));
    assert_eq!(r.bytes(&again).unwrap(), &[0; 8], "reused bytes are zeroed");
    let allocs = ledger.allocations.get();
    for _ in 0..20 {
        r.alloc_bytes(32, 1).unwrap();
    }
    assert_eq!(ledger.allocations.get(), allocs, "spare chunks reused");
    assert_eq!(r.bytes(&keep).unwrap(), b"keep");
}

#[test]
fn spare_chunk_too_small_for_request_is_returned() {
    let (mut r, ledger) = counting(small(4096));
    let mark = r.checkpoint().unwrap();
    r.alloc_bytes(60, 1).unwrap(); // chunk 0 (64)
    r.alloc_bytes(100, 1).unwrap(); // chunk 1 (128)
    r.rewind(mark).unwrap();
    r.alloc_bytes(60, 1).unwrap();
    let big = r.alloc_bytes(200, 1).unwrap(); // spare 128 cannot hold it
    assert_eq!(r.len(&big), Ok(200));
    assert_eq!(r.stats().chunks, 2);
    assert_eq!(ledger.live.get(), 2);
    assert_eq!(r.stats().reserved_bytes, ledger.live_bytes.get());
}

#[test]
fn reset_retains_configured_capacity_and_returns_the_rest() {
    let (mut r, ledger) = counting(small(4096));
    let first = r.alloc_bytes(8, 1).unwrap();
    let p = r.bytes_ptr(&first).unwrap();
    for _ in 0..30 {
        r.alloc_bytes(32, 1).unwrap();
    }
    let epoch = r.stats().epoch;
    assert!(ledger.live.get() > 1);
    r.reset().unwrap();
    assert_eq!(ledger.live.get(), 1, "only retained_bytes (64) kept");
    assert_eq!(r.stats().reserved_bytes, 64);
    assert_eq!(r.stats().epoch, epoch + 1);
    assert_eq!(r.bytes(&first), Err(HandleError::Stale));
    let again = r.alloc_bytes(8, 1).unwrap();
    assert_eq!(r.bytes_ptr(&again).unwrap(), p);
}

#[test]
fn releasing_or_taking_the_tip_reuses_its_bytes() {
    let (mut r, _) = counting(small(1024));
    let a = r.alloc_bytes(4, 1).unwrap();
    let b = r.alloc_bytes(16, 16).unwrap();
    let pb = r.bytes_ptr(&b).unwrap();
    let used = r.stats().used_bytes;
    r.release_bytes(b).unwrap();
    assert_eq!(r.stats().used_bytes, 4, "padding reclaimed too");
    let c = r.alloc_bytes(16, 16).unwrap();
    assert_eq!(r.bytes_ptr(&c).unwrap(), pb);
    assert_eq!(r.stats().used_bytes, used);
    assert_eq!(r.bytes(&b), Err(HandleError::Stale));
    assert_eq!(r.release_bytes(b), Err(HandleError::Stale));

    // Non-tip release is deferred, then cascades once the tip is released.
    r.release_bytes(a).unwrap();
    assert_eq!(r.stats().tracked_allocations, 2);
    r.release_bytes(c).unwrap();
    assert_eq!(r.stats().tracked_allocations, 0);
    assert_eq!(r.stats().used_bytes, 0);

    let v = r.alloc_value(1u64).unwrap();
    assert_eq!(r.take(v), Ok(1));
    assert_eq!(r.stats().used_bytes, 0);
}

#[test]
fn tail_release_does_not_cross_a_watermark_or_raw_allocation() {
    let (mut r, _) = counting(small(1024));
    let a = r.alloc_bytes(8, 1).unwrap();
    let mark = r.checkpoint().unwrap();
    r.release_bytes(a).unwrap();
    assert_eq!(r.stats().used_bytes, 8, "protected by watermark");
    r.commit(mark).unwrap();
    assert_eq!(
        r.stats().used_bytes,
        0,
        "reclaimed once the watermark is committed"
    );

    let b = r.alloc_bytes(8, 1).unwrap();
    let raw = r.alloc_raw(8, 8).unwrap();
    unsafe { raw.as_ptr().write_bytes(0xAA, 8) };
    r.release_bytes(b).unwrap();
    assert_eq!(r.stats().used_bytes, 16, "raw bytes above it stay reserved");
}

// ----- stale and cross-region handles -----------------------------------------------------

#[test]
fn handles_from_another_region_are_rejected_even_with_matching_slots() {
    let mut r1 = Region::new(small(1024)).unwrap();
    let mut r2 = Region::new(small(1024)).unwrap();
    let h1 = r1.alloc_bytes(4, 1).unwrap();
    let _h2 = r2.alloc_bytes(4, 1).unwrap();
    let expected = HandleError::CrossRegion {
        expected: r2.id(),
        found: r1.id(),
    };
    assert_eq!(r2.bytes(&h1), Err(expected));
    assert_eq!(r2.release_bytes(h1), Err(expected));
    let b1 = r1.alloc_value(1u8).unwrap();
    assert_eq!(r2.get(&b1), Err(expected));
    assert_eq!(r2.take(b1), Err(expected));
    let m1 = r1.checkpoint().unwrap();
    match r2.rewind(m1) {
        Err(RewindError::Handle(e)) => assert_eq!(e, expected),
        other => panic!("{other:?}"),
    }
    let m1 = r1.checkpoint().unwrap();
    assert_eq!(r2.commit(m1), Err(expected));
}

#[test]
fn rewinding_an_outer_watermark_invalidates_inner_ones() {
    let (mut r, _) = counting(small(1024));
    let outer = r.checkpoint().unwrap();
    let inner = r.checkpoint().unwrap();
    r.rewind(outer).unwrap();
    match r.rewind(inner) {
        Err(RewindError::Handle(HandleError::Stale)) => {}
        other => panic!("{other:?}"),
    }
    // A new watermark taken at the same position is distinct.
    let fresh = r.checkpoint().unwrap();
    assert_eq!(r.stats().checkpoints, 1);
    r.rewind(fresh).unwrap();
}

#[test]
fn committing_an_inner_watermark_keeps_the_outer_one() {
    let (mut r, _) = counting(small(1024));
    let outer = r.checkpoint().unwrap();
    let inner = r.checkpoint().unwrap();
    let a = r.alloc_bytes(4, 1).unwrap();
    r.commit(inner).unwrap();
    assert_eq!(r.len(&a), Ok(4));
    r.rewind(outer).unwrap();
    assert_eq!(r.len(&a), Err(HandleError::Stale));
}

#[test]
fn stale_typed_handles_cannot_reach_reused_memory() {
    let (mut r, _) = counting(small(1024));
    let mark = r.checkpoint().unwrap();
    let old = r.alloc_value(11u64).unwrap();
    let old_addr = r.get(&old).unwrap() as *const u64;
    r.rewind(mark).unwrap();
    let new = r.alloc_value(22u64).unwrap();
    assert_eq!(
        r.get(&new).unwrap() as *const u64,
        old_addr,
        "memory reused"
    );
    assert_eq!(r.get(&old), Err(HandleError::Stale));
    assert_eq!(r.get_mut(&old), Err(HandleError::Stale));
    assert_eq!(r.take(old), Err(HandleError::Stale));
    assert_eq!(r.get(&new), Ok(&22));
}

// ----- finalizers and linear payloads -------------------------------------------------------

#[test]
fn finalizers_run_lifo_and_taken_values_are_not_finalized() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let (mut r, _) = counting(small(4096));
    let _a = r.alloc_value(tracked(1, &log)).unwrap();
    let mark = r.checkpoint().unwrap();
    let _b = r.alloc_value(tracked(2, &log)).unwrap();
    let c = r.alloc_value(tracked(3, &log)).unwrap();
    let _d = r.alloc_value(tracked(4, &log)).unwrap();
    let taken = r.take(c).unwrap();
    r.rewind(mark).unwrap();
    assert_eq!(*log.borrow(), [4, 2]);
    drop(taken);
    let _e = r.alloc_value(tracked(5, &log)).unwrap();
    r.reset().unwrap();
    assert_eq!(*log.borrow(), [4, 2, 3, 5, 1]);
    let _f = r.alloc_value(tracked(6, &log)).unwrap();
    drop(r);
    assert_eq!(*log.borrow(), [4, 2, 3, 5, 1, 6]);
}

#[test]
fn live_linear_payloads_block_rewind_and_reset() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let (mut r, _) = counting(small(4096));
    let before = r.alloc_linear(tracked(0, &log)).unwrap();
    let mark = r.checkpoint().unwrap();
    let txn = r.alloc_linear(tracked(1, &log)).unwrap();
    let mark = match r.rewind(mark) {
        Err(RewindError::LinearObligationsLive { mark, count: 1 }) => mark,
        other => panic!("{other:?}"),
    };
    assert_eq!(r.get(&txn).unwrap().id, 1, "failed rewind changed nothing");
    assert_eq!(r.stats().live_linear, 2);
    let value = r.take(txn).unwrap();
    assert_eq!(value.id, 1);
    drop(value);
    r.rewind(mark).unwrap();
    assert_eq!(
        r.reset(),
        Err(RegionError::LinearObligationsLive { count: 1 })
    );
    assert_eq!(r.get(&before).unwrap().id, 0);
    drop(r.take(before).unwrap());
    r.reset().unwrap();
    assert_eq!(*log.borrow(), [1, 0]);
}

#[test]
fn close_reports_abandoned_linear_payloads_but_still_finalizes_them() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let (mut r, ledger) = counting(small(4096));
    let _txn = r.alloc_linear(tracked(9, &log)).unwrap();
    assert_eq!(
        r.close().unwrap_err(),
        RegionError::LinearObligationsLive { count: 1 }
    );
    assert_eq!(*log.borrow(), [9]);
    assert_eq!(ledger.live.get(), 0);

    let (mut r, _) = counting(small(4096));
    let txn = r.alloc_linear(1u32).unwrap();
    assert_eq!(r.take(txn), Ok(1));
    let stats = r.close().unwrap();
    assert_eq!(stats.live_linear, 0);
}

// ----- cleanup on error -------------------------------------------------------------------

#[test]
fn scoped_region_is_reclaimed_on_success_error_and_violation() {
    let backing = Counting::default();
    let ledger = backing.0.clone();
    let log = Rc::new(RefCell::new(Vec::new()));
    let out = Region::scoped_in(small(4096), backing.clone(), |r| -> Result<u32, &str> {
        let a = r.alloc_value(tracked(1, &log)).map_err(|_| "alloc")?;
        let n = r.get(&a).map_err(|_| "get")?.id;
        r.alloc_bytes(500, 64).map_err(|_| "bytes")?;
        Err::<u32, _>("decode failed").map(|v| v + n)
    });
    assert_eq!(out, Ok(Err("decode failed")));
    assert_eq!(*log.borrow(), [1]);
    assert_eq!(ledger.live.get(), 0);

    let out = Region::scoped_in(small(4096), backing.clone(), |r| {
        let _ = r.alloc_linear(tracked(2, &log)).unwrap();
        7
    });
    assert_eq!(out, Err(RegionError::LinearObligationsLive { count: 1 }));
    assert_eq!(*log.borrow(), [1, 2]);
    assert_eq!(ledger.live.get(), 0);

    let bad = RegionConfig {
        initial_chunk_bytes: 0,
        ..small(64)
    };
    let out = Region::scoped_in(bad, backing, |_| 0u8);
    assert_eq!(out, Err(RegionError::Config(ConfigError::ZeroInitialChunk)));
    assert_eq!(
        Region::scoped(small(1024), |r| r.alloc_bytes(3, 1).map(|h| r.len(&h))),
        Ok(Ok(Ok(3)))
    );
}

#[test]
fn panicking_scope_still_finalizes_and_frees() {
    let backing = Counting::default();
    let ledger = backing.0.clone();
    let log = Rc::new(RefCell::new(Vec::new()));
    let result = catch_unwind(AssertUnwindSafe(|| {
        Region::scoped_in(small(4096), backing, |r| -> u8 {
            r.alloc_value(tracked(1, &log)).unwrap();
            let _ = r.checkpoint().unwrap();
            r.alloc_value(tracked(2, &log)).unwrap();
            r.alloc_bytes(300, 1).unwrap();
            panic!("body failed");
        })
    }));
    assert!(result.is_err());
    assert_eq!(*log.borrow(), [2, 1]);
    assert_eq!(ledger.live.get(), 0);
}

#[test]
fn panicking_finalizer_does_not_leak_chunks_or_double_drop() {
    struct Bomb(Rc<Cell<u32>>);
    impl Drop for Bomb {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
            panic!("finalizer failed");
        }
    }
    let drops = Rc::new(Cell::new(0));
    let log = Rc::new(RefCell::new(Vec::new()));
    let (mut r, ledger) = counting(small(4096));
    r.alloc_value(tracked(1, &log)).unwrap();
    let mark = r.checkpoint().unwrap();
    r.alloc_value(Bomb(drops.clone())).unwrap();
    assert!(catch_unwind(AssertUnwindSafe(|| r.rewind(mark))).is_err());
    assert_eq!(drops.get(), 1);
    // The region remains usable, and the bomb is never dropped again.
    let h = r.alloc_bytes(4, 1).unwrap();
    assert_eq!(r.len(&h), Ok(4));
    r.alloc_value(Bomb(drops.clone())).unwrap();
    assert!(catch_unwind(AssertUnwindSafe(|| drop(r))).is_err());
    assert_eq!(drops.get(), 2);
    assert_eq!(*log.borrow(), [1], "remaining payloads still finalized");
    assert_eq!(ledger.live.get(), 0, "chunks freed despite the panic");
}

// ----- review follow-ups ---------------------------------------------------------------

#[test]
fn releasing_across_a_chunk_boundary_reclaims_both_chunks() {
    let (mut r, ledger) = counting(RegionConfig {
        max_allocations: 2,
        ..small(4096)
    });
    let a = r.alloc_bytes(64, 1).unwrap(); // fills chunk 0
    let b = r.alloc_bytes(128, 1).unwrap(); // fills chunk 1
    r.release_bytes(a).unwrap();
    r.release_bytes(b).unwrap();
    assert_eq!(r.stats().tracked_allocations, 0);
    assert_eq!(r.stats().used_bytes, 0);
    let allocs = ledger.allocations.get();
    let c = r.alloc_bytes(64, 1).unwrap();
    let d = r.alloc_bytes(128, 1).unwrap();
    assert_eq!(
        ledger.allocations.get(),
        allocs,
        "emptied chunk reused as a spare"
    );
    assert_eq!((r.len(&c), r.len(&d)), (Ok(64), Ok(128)));
}

#[test]
fn cross_chunk_reclaim_stops_at_a_watermark_in_the_emptied_chunk() {
    let (mut r, _) = counting(small(4096));
    let a = r.alloc_bytes(64, 1).unwrap();
    let b = r.alloc_bytes(16, 1).unwrap(); // chunk 1
    let mark = r.checkpoint().unwrap();
    r.release_bytes(a).unwrap();
    r.release_bytes(b).unwrap();
    // `b` is protected by the watermark, so nothing is reclaimed yet.
    assert_eq!(r.stats().tracked_allocations, 2);
    r.commit(mark).unwrap();
    assert_eq!(r.stats().tracked_allocations, 0);
    assert_eq!(r.stats().used_bytes, 0);

    // A watermark taken at the start of an emptied chunk pins that chunk.
    let a = r.alloc_bytes(64, 1).unwrap();
    let b = r.alloc_bytes(16, 1).unwrap();
    r.release_bytes(b).unwrap(); // chunk 1 empties; chunk 0 becomes active again
    let mark = r.checkpoint().unwrap();
    r.release_bytes(a).unwrap();
    assert_eq!(r.stats().used_bytes, 64, "watermark pins the earlier chunk");
    let c = r.alloc_bytes(8, 1).unwrap();
    r.rewind(mark).unwrap();
    assert_eq!(r.len(&c), Err(HandleError::Stale));
    assert_eq!(r.stats().used_bytes, 64);
}

#[test]
fn failed_allocation_keeps_spare_chunks() {
    let (mut r, ledger) = counting(small(192));
    let mark = r.checkpoint().unwrap();
    r.alloc_bytes(60, 1).unwrap(); // chunk 0 (64)
    r.alloc_bytes(100, 1).unwrap(); // chunk 1 (128)
    r.rewind(mark).unwrap();
    r.alloc_bytes(64, 1).unwrap(); // fill chunk 0 again
    let before = r.stats();
    assert!(matches!(
        r.alloc_bytes(193, 1),
        Err(AllocError::CapacityExceeded { .. })
    ));
    assert_eq!(r.stats(), before);
    assert_eq!(ledger.live.get(), 2, "spare kept on capacity failure");

    ledger.fail_after.set(Some(ledger.allocations.get()));
    let (mut r2, ledger2) = counting(small(4096));
    let mark = r2.checkpoint().unwrap();
    r2.alloc_bytes(60, 1).unwrap();
    r2.alloc_bytes(100, 1).unwrap();
    r2.rewind(mark).unwrap();
    r2.alloc_bytes(64, 1).unwrap();
    ledger2.fail_after.set(Some(ledger2.allocations.get()));
    let before = r2.stats();
    assert!(matches!(
        r2.alloc_bytes(200, 1),
        Err(AllocError::OutOfMemory { .. })
    ));
    assert_eq!(r2.stats(), before);
    assert_eq!(ledger2.live.get(), 2, "spare kept on backing failure");
    // The spare still serves a request that fits it, with no backing call.
    let h2 = r2.alloc_bytes(100, 1).unwrap();
    assert_eq!(r2.len(&h2), Ok(100));
    let h = r.alloc_bytes(100, 1).unwrap();
    assert_eq!(r.len(&h), Ok(100));
}

#[test]
fn spare_capacity_counts_toward_a_replacement_chunk() {
    let (mut r, ledger) = counting(small(256));
    let mark = r.checkpoint().unwrap();
    r.alloc_bytes(60, 1).unwrap(); // 64
    r.alloc_bytes(100, 1).unwrap(); // 128
    r.rewind(mark).unwrap();
    r.alloc_bytes(64, 1).unwrap();
    // 64 + 128 + 150 would exceed 256; replacing the unusable spare fits.
    let big = r.alloc_bytes(150, 1).unwrap();
    assert_eq!(r.len(&big), Ok(150));
    assert_eq!(ledger.live.get(), 2);
    assert!(r.stats().reserved_bytes <= 256);
    assert_eq!(r.stats().reserved_bytes, ledger.live_bytes.get());
}

#[test]
fn system_backing_rejects_zero_sized_layouts() {
    assert!(SystemBacking.allocate(Layout::new::<()>()).is_none());
    let layout = Layout::from_size_align(8, 8).unwrap();
    let p = SystemBacking.allocate(layout).unwrap();
    unsafe { SystemBacking.deallocate(p, layout) };
}
