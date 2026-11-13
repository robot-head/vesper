//! Zero allocation per step after warm-up, measured with a counting global
//! allocator. This file holds a single test so no other test thread
//! allocates while it counts.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use vesper_kinetic::{SnapshotRing, hash_world};

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

#[test]
fn steady_state_steps_snapshots_and_rewinds_do_not_allocate() {
    let mut world = common::scene();
    let mut ring = SnapshotRing::new(&world, 4).unwrap();
    for _ in 0..30 {
        world.step().unwrap();
    }
    // The counter itself works.
    COUNTING.store(true, Ordering::SeqCst);
    drop(std::hint::black_box(Box::new([0u8; 64])));
    COUNTING.store(false, Ordering::SeqCst);
    assert_eq!(
        ALLOCS.swap(0, Ordering::SeqCst),
        1,
        "counting allocator self-check"
    );

    COUNTING.store(true, Ordering::SeqCst);
    let mut checksum = 0u8;
    for round in 0..20 {
        let ticket = ring.capture(&world).unwrap();
        for _ in 0..6 {
            let header = world.step().unwrap();
            checksum ^= header.world_state_blake3[0];
        }
        checksum ^= hash_world(world.tick(), world.bodies()).sha256[0];
        if round % 2 == 0 {
            ring.rewind_and_resimulate(ticket, &mut world, 6, |_, _| {})
                .unwrap();
        } else {
            ring.confirm(ticket).unwrap();
        }
    }
    COUNTING.store(false, Ordering::SeqCst);
    let allocations = ALLOCS.load(Ordering::SeqCst);
    assert_eq!(
        allocations, 0,
        "allocations in steady state (checksum {checksum})"
    );
}
