//! VirtualArena: contiguous reservation, in-place tip growth, rewind, reset.
use vesper_alloc::{VIRTUAL_PAGE, VirtualArena, VirtualArenaConfig, VirtualArenaError};

fn arena(bytes: usize) -> VirtualArena {
    VirtualArena::new(VirtualArenaConfig::reserve(bytes)).unwrap()
}

#[test]
fn config_and_alignment_errors_are_typed() {
    assert!(matches!(
        VirtualArena::new(VirtualArenaConfig::reserve(0)),
        Err(VirtualArenaError::InvalidConfig(_))
    ));
    assert_eq!(
        VirtualArena::new(VirtualArenaConfig::reserve(usize::MAX)).unwrap_err(),
        VirtualArenaError::SizeOverflow
    );
    let mut a = arena(1);
    assert_eq!(a.reserved_bytes(), VIRTUAL_PAGE);
    assert_eq!(
        a.alloc_bytes(1, 3).unwrap_err(),
        VirtualArenaError::InvalidAlignment { align: 3 }
    );
    assert_eq!(
        a.alloc_bytes(1, 2 * VIRTUAL_PAGE).unwrap_err(),
        VirtualArenaError::InvalidAlignment {
            align: 2 * VIRTUAL_PAGE
        }
    );
    assert!(matches!(
        a.alloc_bytes(VIRTUAL_PAGE + 1, 1),
        Err(VirtualArenaError::Exhausted { .. })
    ));
}

#[test]
fn allocations_are_aligned_zeroed_and_contiguous() {
    let mut a = arena(64 * 1024);
    let x = a.alloc_bytes(3, 1).unwrap();
    let y = a.alloc_bytes(16, 64).unwrap();
    assert_eq!(a.bytes_addr(&y).unwrap() % 64, 0);
    assert!(a.bytes_addr(&y).unwrap() > a.bytes_addr(&x).unwrap());
    a.bytes_mut(&x).unwrap().copy_from_slice(b"abc");
    assert_eq!(a.bytes(&x).unwrap(), b"abc");
    assert_eq!(a.bytes(&y).unwrap(), &[0; 16]);
}

#[test]
fn tip_vectors_grow_in_place_without_copying() {
    let mut a = arena(1 << 20);
    let mut v = a.vec_with_capacity::<u64>(1).unwrap();
    let addr = a.vec_addr(&v).unwrap();
    for i in 0..10_000u64 {
        a.vec_push(&mut v, i * 3).unwrap();
    }
    assert_eq!(a.vec_addr(&v).unwrap(), addr, "base never moves");
    assert!(a.vec_capacity(&v).unwrap() >= 10_000);
    assert!(a.stats().in_place_growths > 5);
    let s = a.vec_as_slice(&v).unwrap();
    assert!(s.iter().enumerate().all(|(i, x)| *x == i as u64 * 3));
    a.vec_as_mut_slice(&v).unwrap()[0] = 99;
    assert_eq!(a.vec_pop(&mut v).unwrap(), Some(9_999 * 3));
    assert_eq!(a.vec_len(&v).unwrap(), 9_999);
    assert_eq!(a.vec_as_slice(&v).unwrap()[0], 99);
}

#[test]
fn only_the_tip_grows_and_exhaustion_returns_the_value() {
    let mut a = arena(VIRTUAL_PAGE);
    let mut v = a.vec_with_capacity::<u32>(4).unwrap();
    let other = a.alloc_bytes(8, 8).unwrap();
    for i in 0..4 {
        a.vec_push(&mut v, i).unwrap();
    }
    let err = a.vec_push(&mut v, 77).unwrap_err();
    assert_eq!((err.value, err.error), (77, VirtualArenaError::NotTip));
    a.extend(&other, VIRTUAL_PAGE - 64).unwrap();
    let err = a.extend(&other, VIRTUAL_PAGE).unwrap_err();
    assert!(matches!(err, VirtualArenaError::Exhausted { .. }));
    assert_eq!(a.vec_as_slice(&v).unwrap(), [0, 1, 2, 3]);
}

#[test]
fn rewind_invalidates_newer_handles_and_protects_older_growth() {
    let mut a = arena(64 * 1024);
    let mut keep = a.vec_with_capacity::<u16>(2).unwrap();
    a.vec_push(&mut keep, 1).unwrap();
    let mark = a.checkpoint().unwrap();
    a.vec_push(&mut keep, 2).unwrap(); // within capacity: allowed
    let err = a.vec_push(&mut keep, 3).unwrap_err();
    assert_eq!(err.error, VirtualArenaError::ProtectedByWatermark);
    let temp = a.alloc_bytes(100, 8).unwrap();
    let used = a.stats().used_bytes;
    a.rewind(mark).unwrap();
    assert!(a.stats().used_bytes < used);
    assert_eq!(a.bytes(&temp).unwrap_err(), VirtualArenaError::Stale);
    assert_eq!(a.vec_as_slice(&keep).unwrap(), [1, 2]);
    a.vec_push(&mut keep, 3).unwrap(); // tip again, no watermark
    // Bytes reused after the rewind come back zeroed under a new serial.
    let reused = a.alloc_bytes(100, 8).unwrap();
    assert_ne!(reused, temp);
    assert_eq!(a.bytes(&temp).unwrap_err(), VirtualArenaError::Stale);
    assert!(a.bytes(&reused).unwrap().iter().all(|b| *b == 0));
}

#[test]
fn commit_and_mark_errors() {
    let mut a = arena(VIRTUAL_PAGE);
    let mut b = arena(VIRTUAL_PAGE);
    let outer = a.checkpoint().unwrap();
    let inner = a.checkpoint().unwrap();
    let x = a.alloc_bytes(8, 8).unwrap();
    a.commit(inner).unwrap();
    let foreign = b.checkpoint().unwrap();
    assert_eq!(
        a.rewind(foreign).unwrap_err(),
        VirtualArenaError::CrossArena
    );
    assert_eq!(b.bytes(&x).unwrap_err(), VirtualArenaError::CrossArena);
    a.rewind(outer).unwrap();
    assert_eq!(a.bytes(&x).unwrap_err(), VirtualArenaError::Stale);
    let mut tight = VirtualArena::new(VirtualArenaConfig {
        max_checkpoints: 1,
        max_allocations: 1,
        ..VirtualArenaConfig::reserve(VIRTUAL_PAGE)
    })
    .unwrap();
    let m = tight.checkpoint().unwrap();
    assert!(matches!(
        tight.checkpoint(),
        Err(VirtualArenaError::TooManyCheckpoints { limit: 1 })
    ));
    tight.alloc_bytes(1, 1).unwrap();
    assert!(matches!(
        tight.alloc_bytes(1, 1),
        Err(VirtualArenaError::TooManyAllocations { limit: 1 })
    ));
    tight.rewind(m).unwrap();
}

#[test]
fn reset_invalidates_everything_and_reuses_zeroed_memory() {
    let mut a = arena(1 << 20);
    let x = a.alloc_bytes(256 * 1024, 8).unwrap();
    a.bytes_mut(&x).unwrap().fill(0xAB);
    let _mark = a.checkpoint().unwrap();
    assert!(a.stats().touched_bytes >= 256 * 1024);
    a.reset();
    assert_eq!(a.bytes(&x).unwrap_err(), VirtualArenaError::Stale);
    let st = a.stats();
    assert_eq!(
        (st.used_bytes, st.touched_bytes, st.allocations, st.resets),
        (0, 0, 0, 1)
    );
    let y = a.alloc_bytes(256 * 1024, 8).unwrap();
    assert!(a.bytes(&y).unwrap().iter().all(|b| *b == 0));
}

#[test]
fn zero_sized_vectors_are_rejected() {
    let mut a = arena(VIRTUAL_PAGE);
    assert!(matches!(
        a.vec_with_capacity::<()>(1),
        Err(VirtualArenaError::InvalidConfig(_))
    ));
    assert_eq!(
        a.vec_with_capacity::<u64>(usize::MAX).unwrap_err(),
        VirtualArenaError::SizeOverflow
    );
}

/// Resident set size in pages, from `/proc/self/statm`.
#[cfg(all(target_os = "linux", not(miri)))]
fn rss_bytes() -> usize {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: usize = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096
}

#[cfg(all(target_os = "linux", not(miri)))]
#[test]
fn a_large_reservation_is_lazily_backed() {
    const GIB: usize = 1 << 30;
    let before = rss_bytes();
    let mut a = arena(4 * GIB);
    assert_eq!(a.backing_kind(), vesper_alloc::VirtualBackingKind::Mmap);
    assert!(
        rss_bytes() < before + 64 * 1024 * 1024,
        "reserve must not commit 4 GiB"
    );
    let mut v = a.vec_with_capacity::<u8>(0).unwrap();
    for _ in 0..8 * 1024 * 1024 {
        a.vec_push(&mut v, 1).unwrap();
    }
    let after = rss_bytes();
    assert!(
        after >= before + 4 * 1024 * 1024,
        "touched pages are resident"
    );
    assert!(after < before + 256 * 1024 * 1024);
    a.reset();
}

// ----- wave nine: platform reservations, 2 MiB hint, region chunks --------

#[test]
fn the_platform_reservation_path_is_used_and_commits_lazily() {
    use vesper_alloc::VirtualBackingKind;
    let expected = if cfg!(miri) {
        VirtualBackingKind::GlobalAllocator
    } else if cfg!(windows) {
        VirtualBackingKind::VirtualAlloc
    } else if cfg!(unix) {
        VirtualBackingKind::Mmap
    } else {
        VirtualBackingKind::GlobalAllocator
    };
    let lazy = expected != VirtualBackingKind::GlobalAllocator;
    // 1 GiB of address space where reservation is lazy; only what is written
    // may be committed. The eager fallback commits everything up front.
    let reserve = if lazy { 1 << 30 } else { 4 << 20 };
    let mut a = VirtualArena::new(VirtualArenaConfig::reserve(reserve)).unwrap();
    assert_eq!(a.backing_kind(), expected);
    let committed_or_all = |a: &VirtualArena, lazy_value: usize| {
        if lazy { lazy_value } else { a.reserved_bytes() }
    };
    assert_eq!(a.stats().committed_bytes, committed_or_all(&a, 0));
    let h = a.alloc_bytes(100_000, 64).unwrap();
    a.bytes_mut(&h).unwrap()[99_999] = 7;
    let committed = a.stats().committed_bytes;
    if lazy {
        assert!((100_000..=256 * 1024).contains(&committed), "{committed}");
    } else {
        assert_eq!(committed, a.reserved_bytes());
    }
    // Growing the tip past the committed prefix commits more, in place.
    let addr = a.bytes_addr(&h).unwrap();
    a.extend(&h, 1 << 20).unwrap();
    assert_eq!(a.bytes_addr(&h).unwrap(), addr);
    assert_eq!(a.bytes(&h).unwrap()[99_999], 7);
    assert!(a.bytes(&h).unwrap()[100_000..].iter().all(|b| *b == 0));
    a.reset();
    assert_eq!(a.stats().committed_bytes, committed_or_all(&a, 0));
    // Pages handed out again after a reset read as zero.
    let h = a.alloc_bytes(100_000, 64).unwrap();
    assert!(a.bytes(&h).unwrap().iter().all(|b| *b == 0));
}

#[test]
fn the_huge_page_hint_aligns_to_2_mib_and_reports_its_outcome() {
    use vesper_alloc::{HUGE_PAGE, HugePageStatus};
    let mut a = VirtualArena::new(VirtualArenaConfig::reserve(16 << 20).huge_page_hint()).unwrap();
    let status = a.huge_pages();
    if cfg!(all(target_os = "linux", not(miri))) {
        assert_eq!(a.base_addr() % HUGE_PAGE, 0);
        assert!(
            matches!(
                status,
                HugePageStatus::Advised | HugePageStatus::Refused { .. }
            ),
            "{status:?}"
        );
        // Touch 8 MiB; with THP in `madvise` or `always` mode the kernel may
        // back it with huge pages. Report, do not require: khugepaged and
        // defrag settings decide.
        let h = a.alloc_bytes(8 << 20, 4096).unwrap();
        for page in a.bytes_mut(&h).unwrap().chunks_mut(4096) {
            page[0] = 1;
        }
        let huge = a.resident_huge_bytes();
        eprintln!("huge page hint: {status:?}, AnonHugePages {huge:?}");
        assert!(huge.is_some());
    } else if cfg!(all(target_os = "macos", not(miri))) {
        assert_eq!(a.base_addr() % HUGE_PAGE, 0);
        assert_eq!(status, HugePageStatus::AlignedOnly);
    } else {
        assert_eq!(status, HugePageStatus::Unsupported);
    }
    let plain = VirtualArena::new(VirtualArenaConfig::reserve(4096)).unwrap();
    assert_eq!(plain.huge_pages(), HugePageStatus::NotRequested);
}

#[test]
fn regions_over_a_virtual_reservation_get_contiguous_chunks() {
    use vesper_alloc::{Region, RegionConfig, VirtualChunkBacking};
    let config = RegionConfig {
        initial_chunk_bytes: 4096,
        max_chunk_bytes: 64 * 1024,
        max_reserved_bytes: 1 << 20,
        retained_bytes: 4096,
        ..RegionConfig::default()
    };
    let mut r = Region::new_virtual(config, false).unwrap();
    let base = r.backing().base_addr();
    let mut handles = Vec::new();
    for i in 0..200u32 {
        handles.push(r.alloc_value([i; 64]).unwrap());
    }
    let s = r.backing().stats();
    assert!(r.stats().chunks > 3, "{:?}", r.stats());
    assert_eq!(s.live_chunks, r.stats().chunks);
    // Doubling chunks were bump-allocated in address order: the cursor equals
    // the sum of their capacities (no gaps, the chunk alignment divides every
    // capacity).
    assert_eq!(s.cursor_bytes, r.stats().reserved_bytes, "{s:?}");
    assert_eq!(s.hole_bytes, 0);
    for (i, h) in handles.iter().enumerate() {
        assert_eq!(r.get(h).unwrap()[63], i as u32);
    }
    assert!(base != 0);
    // Reset returns all but the retained first chunk; the tail goes back.
    r.reset().unwrap();
    let s = r.backing().stats();
    assert_eq!(s.live_chunks, 1);
    assert_eq!(s.cursor_bytes, 4096);
    assert!(s.tail_releases > 0);
    assert!(s.committed_bytes <= 4096 || cfg!(any(miri, not(any(unix, windows)))));
    // Budget errors still come from the region, not the backing.
    assert!(r.alloc_bytes(2 << 20, 8).is_err());
    let big = Region::new_virtual(
        RegionConfig {
            max_align: 3,
            ..RegionConfig::default()
        },
        false,
    );
    assert!(big.is_err());
    let _ = VirtualChunkBacking::reservation_for(&config).unwrap();
}

#[test]
fn chunk_backing_reuses_holes_first_fit() {
    use std::alloc::Layout;
    use vesper_alloc::{ChunkBacking, VirtualChunkBacking};
    let mut b = VirtualChunkBacking::new(1 << 20, false).unwrap();
    let l = Layout::from_size_align(8192, 64).unwrap();
    let x = b.allocate(l).unwrap();
    let y = b.allocate(l).unwrap();
    let z = b.allocate(l).unwrap();
    assert_eq!(y.as_ptr() as usize - x.as_ptr() as usize, 8192);
    // SAFETY: each pointer came from `allocate` with `l` and is returned once.
    unsafe { b.deallocate(y, l) };
    assert_eq!(b.stats().hole_bytes, 8192);
    let small = Layout::from_size_align(4096, 64).unwrap();
    let y2 = b.allocate(small).unwrap();
    assert_eq!(y2, y);
    assert_eq!(b.stats().hole_bytes, 4096);
    // SAFETY: as above.
    unsafe {
        b.deallocate(z, l);
        b.deallocate(y2, small);
        b.deallocate(x, l);
    }
    let s = b.stats();
    assert_eq!((s.cursor_bytes, s.hole_bytes, s.live_chunks), (0, 0, 0));
    assert!(
        b.allocate(Layout::from_size_align(2 << 20, 64).unwrap())
            .is_none()
    );
}
