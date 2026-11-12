//! Cross-architecture bit determinism: the fixed scene, run for a fixed
//! number of ticks, must hash to the committed golden value. CI runs this on
//! Linux x86_64, macOS AArch64 and Windows x86_64 MSVC, so one green CI run is
//! the cross-ISA evidence. The hash covers every body's position, velocity,
//! mass, compliance, collider and layer bits (see `hash_world`).

mod common;

use vesper_kinetic::hash_world;

const TICKS: u64 = 600;
const GOLDEN_SHA256_TICK_1: &str =
    "36325a53fa4b30863c752ad6f88ce5e6a90ba536640c1c8aa90f5ce7988f5191";
const GOLDEN_SHA256_FINAL: &str =
    "f8d98c3b51da4d885a0460f7059fb1850cb028caa8865a21dded2100709acc10";
const GOLDEN_BLAKE3_FINAL: &str =
    "f4c91c0d2e95d3e342d92b50e50e06a9cd9dfd3a70c2ba09f62cc8e08b25cbb0";

#[test]
fn fixed_scene_matches_committed_golden_hash() {
    let mut world = common::scene();
    let mut after_first = String::new();
    let mut max_contacts = 0;
    for tick in 1..=TICKS {
        let header = world.step().expect("step");
        max_contacts = max_contacts.max(header.manifold_count);
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
