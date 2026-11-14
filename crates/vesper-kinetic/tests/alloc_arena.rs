//! Zero allocation per step after warm-up for the region-arena step with the
//! batched solver, the LBVH broadphase, friction and restitution, measured
//! with a counting global allocator. This file holds a single test so no
//! other test thread allocates while it counts.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use vesper_alloc::Region;
use vesper_kinetic::{BroadphaseMode, ContactMaterial, SolverMode, hash_world};

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
fn region_arena_steps_do_not_allocate_in_steady_state() {
    for broadphase in [BroadphaseMode::Lbvh, BroadphaseMode::SweepAndPrune] {
        steady_state_does_not_allocate(broadphase);
    }
}

fn steady_state_does_not_allocate(broadphase: BroadphaseMode) {
    let mut world = common::scene();
    world.set_solver_mode(SolverMode::Batched16);
    world.set_broadphase(broadphase);
    world
        .set_contact_material(ContactMaterial {
            static_friction: 0.6,
            dynamic_friction: 0.4,
            restitution: 0.5,
        })
        .unwrap();
    let mut arena = Region::new(world.step_arena_config()).unwrap();
    for _ in 0..30 {
        world.step_in(&mut arena).unwrap();
    }
    let warmed = arena.stats();
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
    for _ in 0..120 {
        let header = world.step_in(&mut arena).unwrap();
        checksum ^= header.world_state_blake3[0];
    }
    checksum ^= hash_world(world.tick(), world.bodies()).sha256[0];
    COUNTING.store(false, Ordering::SeqCst);
    assert_eq!(
        ALLOCS.load(Ordering::SeqCst),
        0,
        "{broadphase:?}: allocations in steady state (checksum {checksum})"
    );
    // The arena is rewound after every step: nothing accumulates.
    let after = arena.stats();
    assert_eq!(after.used_bytes, 0);
    assert_eq!(after.tracked_allocations, 0);
    assert_eq!(after.reserved_bytes, warmed.reserved_bytes);
    assert_eq!(after.chunks, 1);
}
