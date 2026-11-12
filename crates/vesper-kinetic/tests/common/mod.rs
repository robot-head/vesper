//! Shared fixed test scene. Every coordinate is built from small integers
//! with exactly representable binary32 arithmetic, so the initial state is
//! identical on every platform.

#![allow(dead_code)]

use vesper_kinetic::{Body, DistanceConstraint, SphereCollider, World, WorldConfig};

pub const RADIUS: f32 = 0.25;

pub fn dynamic(pos: [f32; 3], vel: [f32; 3]) -> Body {
    Body {
        pos,
        vel,
        inv_mass: 1.0,
        compliance_alpha: 0.0,
        collider_asset_id: 0,
        collision_layer_mask: 1,
    }
}

pub fn anchor(pos: [f32; 3]) -> Body {
    Body {
        inv_mass: 0.0,
        ..dynamic(pos, [0.0; 3])
    }
}

/// 6×6 columns of 4 stacked-but-separated spheres with small sideways
/// velocities, plus 4 pendulum chains of 8 links hanging from static
/// anchors and swinging into the pile. 180 bodies, 32 joints.
pub fn scene_parts() -> (Vec<Body>, Vec<DistanceConstraint>) {
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
        let base = [chain as f32 * 1.25 - 0.5, 6.0, -1.5];
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

pub fn config() -> WorldConfig {
    WorldConfig {
        cell_size: 1.0,
        contact_margin: 0.125,
        max_contacts: 2048,
        ..WorldConfig::default()
    }
}

pub fn scene() -> World {
    let (bodies, joints) = scene_parts();
    World::new(
        config(),
        &bodies,
        &[SphereCollider { radius: RADIUS }],
        &joints,
    )
    .expect("scene")
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
