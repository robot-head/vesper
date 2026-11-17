//! `BroadphaseMode::Gpu` with a host-side `PairBackend`: a backend that
//! keeps the contract reproduces the golden bits; malformed or failing
//! backends are typed errors that leave the world unchanged. The Vulkan
//! backend itself is tested on Mesa lavapipe in `vesper-kinetic-gpu`.

mod common;

use vesper_kinetic::lbvh::{Aabb, LbvhError, brute_force_pairs};
use vesper_kinetic::{BroadphaseMode, PairBackend, PairBackendError, StepError, hash_world};

/// Brute force over all box pairs, exactly the contract.
#[derive(Debug)]
struct BruteForce;

impl PairBackend for BruteForce {
    fn name(&self) -> &str {
        "brute-force"
    }
    fn overlapping_pairs(
        &mut self,
        boxes: &[Aabb],
        out: &mut Vec<(u32, u32)>,
        max_pairs: usize,
    ) -> Result<(), PairBackendError> {
        out.clear();
        let pairs = brute_force_pairs(boxes);
        if pairs.len() > max_pairs {
            return Err(PairBackendError::Broadphase(LbvhError::PairOverflow {
                capacity: max_pairs,
            }));
        }
        out.extend(pairs);
        Ok(())
    }
}

/// A backend that corrupts its output in a chosen way.
#[derive(Debug)]
struct Corrupt(u8);

impl PairBackend for Corrupt {
    fn name(&self) -> &str {
        "corrupt"
    }
    fn overlapping_pairs(
        &mut self,
        boxes: &[Aabb],
        out: &mut Vec<(u32, u32)>,
        _max_pairs: usize,
    ) -> Result<(), PairBackendError> {
        out.clear();
        out.extend(brute_force_pairs(boxes));
        match self.0 {
            0 => out.reverse(),
            1 => out.push((0, boxes.len() as u32)),
            2 => out.push((5, 5)),
            3 => {
                let p = out[0];
                out.insert(0, p);
            }
            _ => return Err(PairBackendError::Device),
        }
        Ok(())
    }
}

const TICKS: u64 = 600;
const GOLDEN_SHA256_FINAL: &str =
    "d0005b688c2677b1e6bbfcf2df8e209fe20b4ba2240ccb761afd5e727adbf2dd";

#[test]
fn a_contract_keeping_backend_reproduces_the_golden_hash() {
    let mut world = common::scene();
    world.set_broadphase(BroadphaseMode::Gpu);
    world.set_pair_backend(Some(Box::new(BruteForce)));
    assert_eq!(world.pair_backend_name().as_deref(), Some("brute-force"));
    for _ in 0..TICKS {
        world.step().unwrap();
    }
    let sha = common::hex(&hash_world(world.tick(), world.bodies()).sha256);
    assert_eq!(sha, GOLDEN_SHA256_FINAL);
}

#[test]
fn missing_malformed_and_failing_backends_are_typed_errors() {
    let mut world = common::scene();
    world.set_broadphase(BroadphaseMode::Gpu);
    let before = hash_world(world.tick(), world.bodies());
    assert_eq!(world.step().unwrap_err(), StepError::NoPairBackend);
    for (mode, want) in [
        (0u8, "order"),
        (1, "range"),
        (2, "self pair"),
        (3, "duplicate"),
        (4, "device"),
    ] {
        world.set_pair_backend(Some(Box::new(Corrupt(mode))));
        let err = world.step().unwrap_err();
        match (mode, err) {
            (4, StepError::PairBackend(PairBackendError::Device)) => {}
            (0..=3, StepError::MalformedPairs { .. }) => {}
            other => panic!("{want}: {other:?}"),
        }
        assert_eq!(hash_world(world.tick(), world.bodies()), before, "{want}");
    }
    // Switching back to a CPU broadphase needs no backend.
    world.set_broadphase(BroadphaseMode::SweepAndPrune);
    world.step().unwrap();
}
