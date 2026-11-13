//! Cross-architecture bit determinism: the fixed scene, run for a fixed
//! number of ticks, must hash to the committed golden value. CI runs this on
//! Linux x86_64, macOS AArch64 and Windows x86_64 MSVC, so one green CI run is
//! the cross-ISA evidence. The hash covers every body's position, velocity,
//! mass, compliance, collider and layer bits (see `hash_world`).

mod common;

use vesper_alloc::Region;
use vesper_kinetic::{BroadphaseMode, ContactMaterial, SolverMode, World, hash_world};

const TICKS: u64 = 600;
const GOLDEN_SHA256_TICK_1: &str =
    "43dd82d12e548012b88b979008cf1919eea1eb96737b61da4bdfdb8b04d80509";
const GOLDEN_SHA256_FINAL: &str =
    "d0005b688c2677b1e6bbfcf2df8e209fe20b4ba2240ccb761afd5e727adbf2dd";
const GOLDEN_BLAKE3_FINAL: &str =
    "462d71d065183ffa84a6848d9c0d7c13c6cda9c8abbf3219cc5facdda9606ace";

#[test]
fn fixed_scene_matches_committed_golden_hash() {
    let mut world = common::scene();
    let mut after_first = String::new();
    let mut max_contacts = 0;
    for tick in 1..=TICKS {
        let header = world.step().expect("step");
        max_contacts = max_contacts.max(header.manifold_count);
        // The scene must stay physically sane, not merely reproducible.
        for i in 0..world.bodies().len() {
            let b = world.bodies().get(i).unwrap();
            let speed = b.vel.iter().map(|v| v.abs()).sum::<f32>();
            assert!(speed < 30.0, "tick {tick}: body {i} speed {speed}");
        }
        if tick == 1 {
            after_first = common::hex(&hash_world(world.tick(), world.bodies()).sha256);
        }
    }
    let hashes = hash_world(world.tick(), world.bodies());
    let sha = common::hex(&hashes.sha256);
    let b3 = common::hex(&hashes.blake3);
    eprintln!(
        "arch={} os={} tick1={after_first} final_sha256={sha} final_blake3={b3} max_contacts={max_contacts} colours={}",
        std::env::consts::ARCH,
        std::env::consts::OS,
        world.last_layer_count()
    );
    assert!(max_contacts > 50, "scene must exercise contacts");
    assert_eq!(after_first, GOLDEN_SHA256_TICK_1, "state after one tick");
    assert_eq!(sha, GOLDEN_SHA256_FINAL, "SHA-256 after {TICKS} ticks");
    assert_eq!(b3, GOLDEN_BLAKE3_FINAL, "BLAKE3 after {TICKS} ticks");
    assert_eq!(world.header().world_state_blake3, hashes.blake3);
}

/// Run `ticks` ticks of `world`, stepping through a region arena when
/// `arena` is set; returns (tick-1 SHA-256, final SHA-256, final BLAKE3).
fn run(mut world: World, ticks: u64, arena: bool) -> (String, String, String) {
    let mut region = arena.then(|| Region::new(world.step_arena_config()).unwrap());
    let mut first = String::new();
    for tick in 1..=ticks {
        match region.as_mut() {
            Some(r) => world.step_in(r).expect("step_in"),
            None => world.step().expect("step"),
        };
        if tick == 1 {
            first = common::hex(&hash_world(world.tick(), world.bodies()).sha256);
        }
    }
    let h = hash_world(world.tick(), world.bodies());
    (first, common::hex(&h.sha256), common::hex(&h.blake3))
}

/// The 16-lane batched solver, the LBVH broadphase and the region-arena
/// step reproduce the scalar golden bits exactly, in every combination.
#[test]
fn batched_solver_lbvh_and_arena_match_the_scalar_golden_hash() {
    for solver in [SolverMode::Scalar, SolverMode::Batched16] {
        for broadphase in [BroadphaseMode::MortonCells, BroadphaseMode::Lbvh] {
            for arena in [false, true] {
                let mut world = common::scene();
                world.set_solver_mode(solver);
                world.set_broadphase(broadphase);
                let (first, sha, b3) = run(world, TICKS, arena);
                let what = format!("{solver:?} {broadphase:?} arena={arena}");
                assert_eq!(first, GOLDEN_SHA256_TICK_1, "{what}");
                assert_eq!(sha, GOLDEN_SHA256_FINAL, "{what}");
                assert_eq!(b3, GOLDEN_BLAKE3_FINAL, "{what}");
            }
        }
    }
}

const GOLDEN_FRICTION_SHA256: &str =
    "3b97ec94181efcd265e4c6454b7a35c67151f0c227a10b363ebbe6b595e93576";

/// With friction and restitution on, scalar and batched projection still
/// agree bit for bit, and the result is pinned across architectures.
#[test]
fn friction_and_restitution_scene_is_bit_identical_across_solvers() {
    let material = ContactMaterial {
        static_friction: 0.6,
        dynamic_friction: 0.4,
        restitution: 0.5,
    };
    let mut hashes = Vec::new();
    for solver in [SolverMode::Scalar, SolverMode::Batched16] {
        for broadphase in [BroadphaseMode::MortonCells, BroadphaseMode::Lbvh] {
            let mut world = common::scene();
            world.set_contact_material(material).unwrap();
            world.set_solver_mode(solver);
            world.set_broadphase(broadphase);
            hashes.push(run(world, 300, broadphase == BroadphaseMode::Lbvh).1);
        }
    }
    eprintln!("arch={} friction sha256={}", std::env::consts::ARCH, hashes[0]);
    assert!(hashes.iter().all(|h| *h == hashes[0]), "{hashes:?}");
    assert_eq!(hashes[0], GOLDEN_FRICTION_SHA256);
    // Friction changes the outcome (it is not silently ignored).
    assert_ne!(hashes[0], run(common::scene(), 300, false).1);
}
