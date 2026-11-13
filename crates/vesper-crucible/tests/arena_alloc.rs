//! Zero heap allocation per execution in the warmed-up steady state of a
//! `VirtualArena`, measured with a counting global allocator. This file
//! holds a single test so no other test thread allocates while it counts.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use vesper_crucible::{Verdict, VirtualArena, draw_region_bytes, draw_region_vec};

struct Counting;
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator unchanged; only counts.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// A generator and property over arena values: a byte string and a vector
/// of `u32`, both drawn straight into the region, and a checksum.
fn prop(
    src: &mut vesper_crucible::Source,
    region: &mut vesper_crucible::Region,
    sum: &mut u64,
) -> Result<(), vesper_crucible::PropError> {
    let bytes = draw_region_bytes(src, region, 512)?;
    let words = draw_region_vec::<u32>(src, region, 64)?;
    let b = region.bytes(&bytes).expect("live handle");
    let w = region.vec_as_slice(&words).expect("live handle");
    *sum = b.iter().map(|&x| u64::from(x)).sum::<u64>()
        + w.iter().map(|&x| u64::from(x)).sum::<u64>()
        + sum.wrapping_mul(31);
    Ok(())
}

#[test]
fn warmed_up_arena_executions_do_not_allocate() {
    let mut arena = VirtualArena::new().unwrap();
    let mut sum = 0u64;
    // Saved tapes to replay (allocated before counting).
    let mut tapes: Vec<Vec<u64>> = Vec::new();
    // Warm-up: grow the region's slot table, the source's buffers and the
    // chunk to their steady-state sizes over the same seeds measured below.
    for seed in 0..512u64 {
        let verdict = arena.run_random(&[], seed, 1 << 16, |s, r| prop(s, r, &mut sum));
        assert_eq!(verdict, Verdict::Pass);
        tapes.push(arena.realized().to_vec());
    }
    // One large input: 48 KiB of bytes (three quarters of the arena).
    let mut big = Vec::new();
    for i in 0..48 * 1024u64 {
        big.push(1);
        big.push(i % 256);
    }
    big.push(0);
    big.push(0);
    let large =
        |s: &mut vesper_crucible::Source, r: &mut vesper_crucible::Region, sum: &mut u64| {
            let b = draw_region_bytes(s, r, 64 * 1024)?;
            *sum += r.bytes(&b).expect("live handle").len() as u64;
            Ok(())
        };
    tapes.push(big);
    for tape in &tapes {
        assert_eq!(
            arena.run_replay(tape, |s, r| prop(s, r, &mut sum)),
            Verdict::Pass
        );
    }
    let big = tapes.last().unwrap().clone();
    assert_eq!(
        arena.run_replay(&big, |s, r| large(s, r, &mut sum)),
        Verdict::Pass
    );

    // The counter itself works.
    COUNTING.store(true, Ordering::SeqCst);
    drop(std::hint::black_box(Box::new([0u8; 64])));
    COUNTING.store(false, Ordering::SeqCst);
    assert_eq!(
        ALLOCS.swap(0, Ordering::SeqCst),
        1,
        "counting allocator self-check"
    );

    let before = arena.stats();
    COUNTING.store(true, Ordering::SeqCst);
    for round in 0..4u64 {
        for seed in 0..512u64 {
            let verdict = arena.run_random(&[], seed, 1 << 16, |s, r| prop(s, r, &mut sum));
            if verdict != Verdict::Pass {
                COUNTING.store(false, Ordering::SeqCst);
                panic!("round {round} seed {seed}: {verdict:?}");
            }
        }
        for tape in &tapes[..512] {
            let verdict = arena.run_replay(tape, |s, r| prop(s, r, &mut sum));
            if verdict != Verdict::Pass {
                COUNTING.store(false, Ordering::SeqCst);
                panic!("replay: {verdict:?}");
            }
        }
        let verdict = arena.run_replay(&big, |s, r| large(s, r, &mut sum));
        if verdict != Verdict::Pass {
            COUNTING.store(false, Ordering::SeqCst);
            panic!("large replay: {verdict:?}");
        }
    }
    COUNTING.store(false, Ordering::SeqCst);
    let allocs = ALLOCS.swap(0, Ordering::SeqCst);
    let after = arena.stats();
    std::hint::black_box(sum);
    println!(
        "{} executions, {} heap allocations, arena high water {} of {} bytes",
        after.executions - before.executions,
        allocs,
        after.high_water_bytes,
        arena.bytes()
    );
    assert_eq!(after.executions - before.executions, 4 * 1025);
    assert!(after.high_water_bytes >= 48 * 1024);
    assert_eq!(allocs, 0, "steady-state arena executions allocated");
}
