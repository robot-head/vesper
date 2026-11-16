//! `BroadphaseMode::Gpu` on a real Vulkan device (Mesa lavapipe in CI): the
//! device pairs reproduce the CPU broadphases' bits, including the
//! committed golden hash of the fixed scene, tick for tick.
//!
//! Without a device the tests print why and pass, except with
//! `VESPER_GPU_REQUIRE=1` (set by the lavapipe workflow), where a missing
//! device fails them.

use vesper_kinetic::{
    Body, BroadphaseMode, DistanceConstraint, SphereCollider, World, WorldConfig, hash_world,
};
use vesper_kinetic_gpu::GpuPairBackend;

const RADIUS: f32 = 0.25;

fn dynamic(pos: [f32; 3], vel: [f32; 3]) -> Body {
    Body {
        pos,
        vel,
        inv_mass: 1.0,
        compliance_alpha: 0.0,
        collider_asset_id: 0,
        collision_layer_mask: 1,
    }
}

fn anchor(pos: [f32; 3]) -> Body {
    Body {
        inv_mass: 0.0,
        ..dynamic(pos, [0.0; 3])
    }
}

/// 6×6 columns of 4 stacked-but-separated spheres with small sideways
/// velocities, plus 4 pendulum chains of 8 links hanging from static
/// anchors and swinging into the pile. 180 bodies, 32 joints.
fn scene_parts() -> (Vec<Body>, Vec<DistanceConstraint>) {
    let mut bodies = Vec::new();
    for gx in 0..6i32 {
        for gz in 0..6i32 {
            for level in 0..4i32 {
                let seed = (gx * 31 + gz * 17 + level * 7) % 11 - 5;
                bodies.push(dynamic(
                    [
                        gx as f32 * 0.625,
                        0.5 + level as f32 * 0.75 + (gx + gz) as f32 * 0.0625,
                        gz as f32 * 0.625,
                    ],
                    [seed as f32 * 0.125, 0.0, -(seed as f32) * 0.0625],
                ));
            }
        }
    }
    let mut joints = Vec::new();
    for chain in 0..4i32 {
        // Chains hang in separate z planes so their links never start
        // overlapping each other.
        let base = [chain as f32 * 1.25 - 0.5, 6.0, -1.5 - chain as f32 * 0.75];
        let first = bodies.len() as u32;
        bodies.push(anchor(base));
        for link in 1..=8i32 {
            bodies.push(dynamic(
                [base[0] + link as f32 * 0.5, base[1], base[2]],
                [0.0; 3],
            ));
            joints.push(DistanceConstraint {
                a: first + link as u32 - 1,
                b: first + link as u32,
                rest_length: 0.5,
                compliance: if chain % 2 == 0 { 0.0 } else { 1.0e-4 },
            });
        }
    }
    (bodies, joints)
}

fn config() -> WorldConfig {
    WorldConfig {
        cell_size: 1.0,
        contact_margin: 0.125,
        max_contacts: 2048,
        ..WorldConfig::default()
    }
}

fn scene() -> World {
    let (bodies, joints) = scene_parts();
    World::new(
        config(),
        &bodies,
        &[SphereCollider { radius: RADIUS }],
        &joints,
    )
    .expect("scene")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `vesper-kinetic`'s `tests/golden.rs` value for the same scene.
const GOLDEN_SHA256_FINAL: &str =
    "d0005b688c2677b1e6bbfcf2df8e209fe20b4ba2240ccb761afd5e727adbf2dd";

#[test]
fn gpu_broadphase_reproduces_the_golden_hash() {
    let Some(ctx) = vesper_gpu_compute::test_context() else {
        return;
    };
    eprintln!("device: {:?}", ctx.info());
    let mut gpu = scene();
    let mut cpu = scene();
    cpu.set_broadphase(BroadphaseMode::Lbvh);
    gpu.set_broadphase(BroadphaseMode::Gpu);
    let backend = GpuPairBackend::for_world(&ctx, &gpu).expect("device buffers");
    gpu.set_pair_backend(Some(Box::new(backend)));
    assert!(gpu.pair_backend_name().unwrap().starts_with("vulkan:"));
    for tick in 1..=600u64 {
        let a = gpu.step().expect("gpu step");
        let b = cpu.step().expect("cpu step");
        assert_eq!(a, b, "tick {tick}");
    }
    let sha = hex(&hash_world(gpu.tick(), gpu.bodies()).sha256);
    assert_eq!(sha, GOLDEN_SHA256_FINAL);
}

#[test]
fn gpu_broadphase_capacity_errors_are_typed() {
    let Some(ctx) = vesper_gpu_compute::test_context() else {
        return;
    };
    let mut world = scene();
    world.set_broadphase(BroadphaseMode::Gpu);
    // Room for fewer boxes than the world has.
    let small = GpuPairBackend::new(&ctx, 8, 64).unwrap();
    world.set_pair_backend(Some(Box::new(small)));
    let before = hash_world(world.tick(), world.bodies());
    let err = world.step().unwrap_err();
    assert!(
        matches!(
            err,
            vesper_kinetic::StepError::PairBackend(vesper_kinetic::PairBackendError::Broadphase(
                vesper_kinetic::lbvh::LbvhError::CapacityExceeded { .. }
            ))
        ),
        "{err:?}"
    );
    assert_eq!(hash_world(world.tick(), world.bodies()), before);
}
