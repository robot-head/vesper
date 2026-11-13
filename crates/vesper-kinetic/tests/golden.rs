//! Cross-architecture bit determinism: the fixed scene, run for a fixed
//! number of ticks, must hash to the committed golden value. CI runs this on
//! Linux x86_64, macOS AArch64 and Windows x86_64 MSVC, so one green CI run is
//! the cross-ISA evidence. The hash covers every body's position, velocity,
//! mass, compliance, collider and layer bits (see `hash_world`).

mod common;

use vesper_kinetic::hash_world;

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
