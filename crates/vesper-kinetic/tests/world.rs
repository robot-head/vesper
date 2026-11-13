//! Solver sanity: energy, constraint satisfaction, contacts, validation.

mod common;

use common::{RADIUS, anchor, dynamic};
use vesper_alloc::{Region, RegionConfig};
use vesper_kinetic::{
    BroadphaseMode, DistanceConstraint, KineticSnapshotWireHdr, SNAPSHOT_HEADER_LEN,
    SphereCollider, StepError, World, WorldConfig, hash_world,
};

const BALL: [SphereCollider; 1] = [SphereCollider { radius: RADIUS }];

fn total_energy(w: &World) -> f64 {
    w.kinetic_energy() + w.potential_energy()
}

#[test]
fn free_flight_without_gravity_keeps_velocity_and_moves_linearly() {
    let cfg = WorldConfig {
        gravity: [0.0; 3],
        ground_y: None,
        ..common::config()
    };
    let mut w = World::new(
        cfg,
        &[dynamic([0.0, 5.0, 0.0], [1.0, 0.5, -2.0])],
        &BALL,
        &[],
    )
    .unwrap();
    for _ in 0..120 {
        w.step().unwrap();
    }
    let b = w.bodies().get(0).unwrap();
    // XPBD re-derives velocity from binary32 positions each substep, so it
    // drifts by about one position ulp divided by the substep (~2e-4 here).
    for (got, want) in b.vel.iter().zip([1.0, 0.5, -2.0]) {
        assert!((got - want).abs() < 1e-3, "{got} vs {want}");
    }
    for (got, want) in b.pos.iter().zip([1.0, 5.5, -2.0]) {
        assert!((got - want).abs() < 1e-3, "{got} vs {want}");
    }
}

#[test]
fn dropped_sphere_comes_to_rest_on_the_ground_and_loses_energy() {
    let mut w = World::new(
        common::config(),
        &[dynamic([0.0, 2.0, 0.0], [0.0; 3])],
        &BALL,
        &[],
    )
    .unwrap();
    let start = total_energy(&w);
    let mut last = start;
    for _ in 0..360 {
        w.step().unwrap();
        let e = total_energy(&w);
        assert!(e <= last + 1e-3, "energy rose from {last} to {e}");
        last = e;
        assert!(
            w.max_penetration() < 0.02,
            "penetration {}",
            w.max_penetration()
        );
    }
    let b = w.bodies().get(0).unwrap();
    assert!((b.pos[1] - RADIUS).abs() < 0.01, "rest height {}", b.pos[1]);
    assert!(b.vel[1].abs() < 0.1, "rest velocity {}", b.vel[1]);
    assert!(last < start);
}

#[test]
fn stacked_spheres_settle_without_deep_penetration() {
    let bodies: Vec<_> = (0..5)
        .map(|i| dynamic([0.0, 0.3 + i as f32 * 0.55, 0.0], [0.0; 3]))
        .collect();
    let mut w = World::new(common::config(), &bodies, &BALL, &[]).unwrap();
    for _ in 0..480 {
        w.step().unwrap();
    }
    assert!(
        w.max_penetration() < 0.05,
        "penetration {}",
        w.max_penetration()
    );
    for i in 0..5 {
        let b = w.bodies().get(i).unwrap();
        assert!(b.pos[1] > 0.2 && b.pos[1] < 3.0, "body {i} at {:?}", b.pos);
    }
}

#[test]
fn pendulum_chain_keeps_joint_lengths_and_does_not_gain_energy() {
    let mut bodies = vec![anchor([0.0, 5.0, 0.0])];
    let mut joints = Vec::new();
    for link in 1..=6u32 {
        bodies.push(dynamic([link as f32 * 0.5, 5.0, 0.0], [0.0; 3]));
        joints.push(DistanceConstraint {
            a: link - 1,
            b: link,
            rest_length: 0.5,
            compliance: 0.0,
        });
    }
    let cfg = WorldConfig {
        ground_y: None,
        ..common::config()
    };
    // Different layer masks keep links from colliding with each other.
    for (i, b) in bodies.iter_mut().enumerate() {
        b.collision_layer_mask = 1 << i;
    }
    let mut w = World::new(cfg, &bodies, &BALL, &joints).unwrap();
    let start = total_energy(&w);
    for _ in 0..600 {
        w.step().unwrap();
        assert!(
            w.max_joint_error() < 0.05,
            "joint error {}",
            w.max_joint_error()
        );
    }
    assert!(
        total_energy(&w) <= start + 1e-3,
        "energy {} > {start}",
        total_energy(&w)
    );
    let anchor_now = w.bodies().get(0).unwrap();
    assert_eq!(anchor_now.pos, [0.0, 5.0, 0.0], "static anchor never moves");
}

#[test]
fn identical_worlds_stay_bit_identical_and_use_several_colours() {
    let mut a = common::scene();
    let mut b = common::scene();
    for _ in 0..120 {
        let ha = a.step().unwrap();
        let hb = b.step().unwrap();
        assert_eq!(ha, hb);
    }
    assert!(a.last_layer_count() >= 2);
    assert_eq!(
        hash_world(a.tick(), a.bodies()),
        hash_world(b.tick(), b.bodies())
    );
}

#[test]
fn snapshot_header_is_48_padding_free_bytes() {
    let mut w = common::scene();
    let h = w.step().unwrap();
    let bytes = h.encode();
    assert_eq!(bytes.len(), SNAPSHOT_HEADER_LEN);
    assert_eq!(&bytes[0..8], &1u64.to_le_bytes());
    assert_eq!(&bytes[8..12], &(w.bodies().len() as u32).to_le_bytes());
    assert_eq!(KineticSnapshotWireHdr::decode(&bytes).unwrap(), h);
    assert!(KineticSnapshotWireHdr::decode(&bytes[..47]).is_err());
    assert!(KineticSnapshotWireHdr::decode(&[0u8; 49]).is_err());
}

#[test]
fn invalid_inputs_are_typed_errors() {
    let ok = dynamic([0.0, 1.0, 0.0], [0.0; 3]);
    let cfg = common::config();
    let bad_dt = WorldConfig { dt: 0.0, ..cfg };
    assert!(matches!(
        World::new(bad_dt, &[ok], &BALL, &[]),
        Err(StepError::InvalidConfig(_))
    ));
    let bad_sub = WorldConfig { substeps: 0, ..cfg };
    assert!(matches!(
        World::new(bad_sub, &[ok], &BALL, &[]),
        Err(StepError::InvalidConfig(_))
    ));
    let small_cell = WorldConfig {
        cell_size: 0.5,
        ..cfg
    };
    assert!(matches!(
        World::new(small_cell, &[ok], &BALL, &[]),
        Err(StepError::InvalidConfig(_))
    ));
    let nan = dynamic([f32::NAN, 1.0, 0.0], [0.0; 3]);
    assert!(matches!(
        World::new(cfg, &[nan], &BALL, &[]),
        Err(StepError::InvalidBody { body: 0, .. })
    ));
    let neg = vesper_kinetic::Body {
        inv_mass: -1.0,
        ..ok
    };
    assert!(matches!(
        World::new(cfg, &[neg], &BALL, &[]),
        Err(StepError::InvalidBody { .. })
    ));
    let ghost = vesper_kinetic::Body {
        collider_asset_id: 3,
        ..ok
    };
    assert!(matches!(
        World::new(cfg, &[ghost], &BALL, &[]),
        Err(StepError::UnknownCollider { .. })
    ));
    let joint = |a, b, rest| DistanceConstraint {
        a,
        b,
        rest_length: rest,
        compliance: 0.0,
    };
    assert!(matches!(
        World::new(cfg, &[ok, ok], &BALL, &[joint(0, 2, 1.0)]),
        Err(StepError::InvalidJoint { .. })
    ));
    assert!(matches!(
        World::new(cfg, &[ok, ok], &BALL, &[joint(1, 1, 1.0)]),
        Err(StepError::InvalidJoint { .. })
    ));
    assert!(matches!(
        World::new(cfg, &[ok, ok], &BALL, &[joint(0, 1, -1.0)]),
        Err(StepError::InvalidJoint { .. })
    ));

    let mut w = World::new(cfg, &[ok, anchor([0.0; 3])], &BALL, &[]).unwrap();
    assert!(
        w.apply_velocity_change(1, [1.0, 0.0, 0.0]).is_err(),
        "static body"
    );
    assert!(
        w.apply_velocity_change(2, [1.0, 0.0, 0.0]).is_err(),
        "unknown body"
    );
    assert!(
        w.apply_velocity_change(0, [f32::INFINITY, 0.0, 0.0])
            .is_err()
    );
    w.apply_velocity_change(0, [1.0, 0.0, 0.0]).unwrap();
}

#[test]
fn contact_overflow_fails_the_step_and_leaves_the_world_unchanged() {
    let cfg = WorldConfig {
        max_contacts: 3,
        ..common::config()
    };
    let bodies: Vec<_> = (0..8)
        .map(|i| dynamic([i as f32 * 0.4, 0.2, 0.0], [0.0; 3]))
        .collect();
    let mut w = World::new(cfg, &bodies, &BALL, &[]).unwrap();
    let before = hash_world(w.tick(), w.bodies());
    assert_eq!(w.step(), Err(StepError::ContactOverflow { capacity: 3 }));
    assert_eq!(w.tick(), 0);
    assert_eq!(hash_world(w.tick(), w.bodies()), before);
}

#[test]
fn fast_bodies_do_not_tunnel_through_each_other_within_a_tick() {
    // 72 units/s toward each other: without swept candidates one tick swaps
    // them with no contact.
    let cfg = WorldConfig {
        gravity: [0.0; 3],
        ground_y: None,
        ..common::config()
    };
    let bodies = [
        dynamic([-0.3, 0.0, 0.0], [36.0, 0.0, 0.0]),
        dynamic([0.3, 0.0, 0.0], [-36.0, 0.0, 0.0]),
    ];
    let mut w = World::new(cfg, &bodies, &BALL, &[]).unwrap();
    let h = w.step().unwrap();
    assert!(h.manifold_count >= 1, "contact found");
    let (a, b) = (w.bodies().get(0).unwrap(), w.bodies().get(1).unwrap());
    assert!(a.pos[0] < b.pos[0], "no swap: {:?} {:?}", a.pos, b.pos);
    assert!(
        w.max_penetration() < 0.05,
        "penetration {}",
        w.max_penetration()
    );

    // Too fast for the bounded search: a typed error, world unchanged.
    let rocket = [dynamic([0.0; 3], [1.0e4, 0.0, 0.0])];
    let mut w = World::new(cfg, &rocket, &BALL, &[]).unwrap();
    assert_eq!(w.step(), Err(StepError::SweepTooLarge { body: 0 }));
    assert_eq!(w.tick(), 0);
}

#[test]
fn coincident_centres_are_separated_deterministically() {
    let cfg = WorldConfig {
        gravity: [0.0; 3],
        ground_y: None,
        ..common::config()
    };
    let same = [
        dynamic([1.0, 2.0, 3.0], [0.0; 3]),
        dynamic([1.0, 2.0, 3.0], [0.0; 3]),
    ];
    let mut w = World::new(cfg, &same, &BALL, &[]).unwrap();
    for _ in 0..10 {
        w.step().unwrap();
    }
    assert!(
        w.max_penetration() < 0.01,
        "penetration {}",
        w.max_penetration()
    );
    let (a, b) = (w.bodies().get(0).unwrap(), w.bodies().get(1).unwrap());
    assert!(a.pos[1] > b.pos[1], "separated along +y");

    let mut joined = same;
    for body in &mut joined {
        body.collision_layer_mask = 0;
    }
    let joint = DistanceConstraint {
        a: 0,
        b: 1,
        rest_length: 0.5,
        compliance: 0.0,
    };
    let mut w = World::new(cfg, &joined, &BALL, &[joint]).unwrap();
    for _ in 0..10 {
        w.step().unwrap();
    }
    assert!(
        w.max_joint_error() < 1e-3,
        "joint error {}",
        w.max_joint_error()
    );
}

#[test]
fn lbvh_broadphase_has_no_search_radius_limit() {
    // The rocket that the bounded cell search refuses still meets its
    // partner through the LBVH, whose boxes cover the whole sweep.
    // 300 units/s with 8 substeps moves 0.31 per substep (less than a
    // diameter, so the substeps can resolve the contact) but 2.5 per tick,
    // beyond the cell search radius.
    let cfg = WorldConfig {
        gravity: [0.0; 3],
        ground_y: None,
        substeps: 8,
        ..common::config()
    };
    let bodies = [
        dynamic([0.0; 3], [300.0, 0.0, 0.0]),
        dynamic([2.0, 0.0, 0.0], [0.0; 3]),
    ];
    let mut cells = World::new(cfg, &bodies, &BALL, &[]).unwrap();
    assert_eq!(cells.step(), Err(StepError::SweepTooLarge { body: 0 }));
    let mut w = World::new(cfg, &bodies, &BALL, &[]).unwrap();
    w.set_broadphase(BroadphaseMode::Lbvh);
    let h = w.step().unwrap();
    assert_eq!(h.manifold_count, 1);
    let (a, b) = (w.bodies().get(0).unwrap(), w.bodies().get(1).unwrap());
    assert!(a.pos[0] < b.pos[0], "no tunnelling: {:?} {:?}", a.pos, b.pos);
}

#[test]
fn a_too_small_step_arena_is_a_typed_error_and_changes_nothing() {
    let mut w = common::scene();
    let mut arena = Region::new(RegionConfig::bounded(256)).unwrap();
    let before = hash_world(w.tick(), w.bodies());
    assert!(matches!(w.step_in(&mut arena), Err(StepError::Arena(_))));
    assert_eq!(w.tick(), 0);
    assert_eq!(hash_world(w.tick(), w.bodies()), before);
    // The failed step left nothing behind in the arena.
    assert_eq!(arena.stats().used_bytes, 0);
    assert_eq!(arena.stats().checkpoints, 0);
    // A correctly sized arena works and matches `step`.
    let mut arena = Region::new(w.step_arena_config()).unwrap();
    let mut reference = common::scene();
    for _ in 0..5 {
        assert_eq!(w.step_in(&mut arena).unwrap(), reference.step().unwrap());
    }
}
