//! Coulomb friction (static and dynamic) and restitution in XPBD contacts,
//! checked against closed-form particle mechanics.

#![allow(clippy::needless_range_loop)]

mod common;

use vesper_kinetic::{ContactMaterial, SolverMode, SphereCollider, StepError, World, WorldConfig};

const R: f32 = 0.25;
const G: f32 = 9.81;

fn one_ball(gravity: [f32; 3], pos: [f32; 3], vel: [f32; 3], m: ContactMaterial) -> World {
    let config = WorldConfig {
        gravity,
        contact_margin: 0.125,
        ..WorldConfig::default()
    };
    let mut w = World::new(
        config,
        &[common::dynamic(pos, vel)],
        &[SphereCollider { radius: R }],
        &[],
    )
    .unwrap();
    w.set_contact_material(m).unwrap();
    w
}

fn friction(mu_s: f32, mu_k: f32) -> ContactMaterial {
    ContactMaterial {
        static_friction: mu_s,
        dynamic_friction: mu_k,
        restitution: 0.0,
    }
}

#[test]
fn static_friction_holds_below_the_coulomb_limit() {
    // Tilted gravity models a 16.7° incline: tangential pull 0.3·g < μs·g.
    for solver in [SolverMode::Scalar, SolverMode::Batched16] {
        let mut w = one_ball(
            [0.3 * G, -G, 0.0],
            [0.0, R, 0.0],
            [0.0; 3],
            friction(0.5, 0.3),
        );
        w.set_solver_mode(solver);
        for _ in 0..240 {
            w.step().unwrap();
        }
        let b = w.bodies().get(0).unwrap();
        assert!(b.pos[0].abs() < 1e-3, "{solver:?}: crept {}", b.pos[0]);
        assert!(b.vel[0].abs() < 1e-2, "{solver:?}: v {}", b.vel[0]);
    }
    // Without friction the same ball slides away at 0.3·g.
    let mut w = one_ball(
        [0.3 * G, -G, 0.0],
        [0.0, R, 0.0],
        [0.0; 3],
        friction(0.0, 0.0),
    );
    for _ in 0..120 {
        w.step().unwrap();
    }
    let x = w.bodies().get(0).unwrap().pos[0];
    let expected = 0.5 * 0.3 * G;
    assert!((x - expected).abs() < 0.05 * expected, "frictionless x {x}");
}

#[test]
fn dynamic_friction_slides_with_reduced_acceleration() {
    // Pull 0.8·g exceeds μs·g = 0.5·g: the ball slides with a = (0.8 − 0.3)·g.
    let mut w = one_ball(
        [0.8 * G, -G, 0.0],
        [0.0, R, 0.0],
        [0.0; 3],
        friction(0.5, 0.3),
    );
    for _ in 0..120 {
        w.step().unwrap();
    }
    let x = w.bodies().get(0).unwrap().pos[0];
    let expected = 0.5 * (0.8 - 0.3) * G; // after 1 s
    assert!(
        (x - expected).abs() < 0.08 * expected,
        "x {x}, expected {expected}"
    );
}

#[test]
fn sliding_ball_stops_after_the_coulomb_distance() {
    let v0 = 3.0f32;
    let mu = 0.5f32;
    let mut w = one_ball(
        [0.0, -G, 0.0],
        [0.0, R, 0.0],
        [v0, 0.0, 0.0],
        friction(mu, mu),
    );
    for _ in 0..240 {
        w.step().unwrap();
    }
    let b = w.bodies().get(0).unwrap();
    let expected = v0 * v0 / (2.0 * mu * G);
    assert!(b.vel[0].abs() < 1e-3, "still moving at {}", b.vel[0]);
    assert!(
        (b.pos[0] - expected).abs() < 0.08 * expected,
        "stopped at {}, expected {expected}",
        b.pos[0]
    );
}

/// Highest centre height after the first ground contact.
fn rebound_height(e: f32, solver: SolverMode) -> f32 {
    let mut w = one_ball(
        [0.0, -G, 0.0],
        [0.0, 2.0, 0.0],
        [0.0; 3],
        ContactMaterial {
            restitution: e,
            ..ContactMaterial::default()
        },
    );
    w.set_solver_mode(solver);
    let mut touched = false;
    let mut best = 0.0f32;
    for _ in 0..360 {
        w.step().unwrap();
        let b = w.bodies().get(0).unwrap();
        if b.pos[1] < R + 0.01 {
            touched = true;
        }
        if touched {
            best = best.max(b.pos[1]);
        }
    }
    assert!(touched);
    best
}

#[test]
fn restitution_bounces_to_e_squared_of_the_drop() {
    let drop = 2.0 - R;
    for solver in [SolverMode::Scalar, SolverMode::Batched16] {
        for e in [0.5f32, 0.8] {
            let h = rebound_height(e, solver) - R;
            let expected = e * e * drop;
            assert!(
                (h - expected).abs() < 0.1 * expected,
                "{solver:?} e={e}: rebound {h}, expected {expected}"
            );
        }
        // e = 0: the ball stays down (no XPBD pop-out bounce).
        let h = rebound_height(0.0, solver) - R;
        assert!(h < 0.02, "{solver:?} inelastic rebound {h}");
    }
}

#[test]
fn materials_are_validated() {
    let mut w = common::scene();
    for (m, what) in [
        (friction(-0.1, 0.0), "static_friction must be >= 0"),
        (friction(0.0, f32::NAN), "dynamic_friction must be >= 0"),
        (
            ContactMaterial {
                restitution: 1.5,
                ..ContactMaterial::default()
            },
            "restitution must be in [0, 1]",
        ),
    ] {
        assert_eq!(
            w.set_contact_material(m),
            Err(StepError::InvalidMaterial(what))
        );
    }
    assert_eq!(w.contact_material(), ContactMaterial::default());
}
