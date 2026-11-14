//! Sleeping islands and continuous collision detection.

use vesper_kinetic::rigid::{
    CcdConfig, ColliderShape, GroundPlane, RigidBody, RigidCollider, RigidConfig, RigidError,
    RigidJoint, RigidMaterial, RigidWorld, SleepConfig, box_inv_inertia, sphere_inv_inertia,
};

fn material(mu: f32) -> RigidMaterial {
    RigidMaterial {
        static_friction: mu,
        dynamic_friction: mu,
        restitution: 0.0,
        rolling_friction: 0.0,
    }
}

fn cube(m: RigidMaterial) -> RigidCollider {
    RigidCollider {
        shape: ColliderShape::Box {
            half_extents: [0.5; 3],
        },
        material: m,
    }
}

fn ball(r: f32, m: RigidMaterial) -> RigidCollider {
    RigidCollider {
        shape: ColliderShape::Sphere { radius: r },
        material: m,
    }
}

fn stack(levels: usize, substeps: u32) -> (Vec<RigidBody>, RigidConfig) {
    let bodies = (0..levels)
        .map(|i| {
            RigidBody::new(
                [0.0, 0.5 + 1.0 * i as f32, 0.0],
                1.0,
                box_inv_inertia(1.0, [0.5; 3]),
                0,
            )
        })
        .collect();
    let cfg = RigidConfig {
        substeps,
        ground: Some(GroundPlane {
            height: 0.0,
            material: material(0.6),
        }),
        ..RigidConfig::default()
    };
    (bodies, cfg)
}

#[test]
fn a_resting_stack_falls_asleep_and_a_falling_ball_wakes_it() {
    let levels = 5;
    let (mut bodies, cfg) = stack(levels, 16);
    // A ball parked far away, asleep with the rest until it is launched.
    bodies.push(RigidBody::new(
        [6.0, 0.25, 0.0],
        1.0,
        sphere_inv_inertia(1.0, 0.25),
        1,
    ));
    let colliders = [cube(material(0.6)), ball(0.25, material(0.6))];
    let mut w = RigidWorld::new(cfg, &bodies, &colliders).unwrap();
    w.set_sleeping(Some(SleepConfig::default())).unwrap();
    let mut slept_at = None;
    for t in 0..600 {
        w.step().unwrap();
        if w.sleeping_count() == levels + 1 {
            slept_at = Some(t);
            break;
        }
    }
    let slept_at = slept_at.expect("the stack and the ball fall asleep");
    eprintln!("asleep after {slept_at} ticks");
    // The stack stands: every box within 1 cm of its rest position.
    for i in 0..levels {
        let b = w.body(i).unwrap();
        let rest = 0.5 + i as f32;
        assert!(
            (b.pos[1] - rest).abs() < 0.01 && b.pos[0].abs() < 0.01 && b.pos[2].abs() < 0.01,
            "box {i} at {:?}",
            b.pos
        );
    }
    // Asleep, nothing moves: the state is bit-identical tick to tick.
    let frozen = w.hash();
    let before: Vec<_> = (0..w.len()).map(|i| w.body(i).unwrap()).collect();
    for _ in 0..30 {
        w.step().unwrap();
    }
    let after: Vec<_> = (0..w.len()).map(|i| w.body(i).unwrap()).collect();
    assert_eq!(before, after);
    assert_ne!(frozen, w.hash(), "the tick counter advanced");
    // Launch the ball at the top box: the ball wakes, its island reaches
    // the whole stack within the step of the impact.
    let ball_index = levels;
    w.apply_velocity_change(ball_index, [-6.0, 0.0, 0.0], [0.0; 3])
        .unwrap();
    assert!(!w.is_asleep(ball_index));
    let mut woke_all = false;
    for _ in 0..240 {
        w.step().unwrap();
        if (0..levels).all(|i| !w.is_asleep(i)) {
            woke_all = true;
            break;
        }
    }
    assert!(woke_all, "the impact woke the whole stack");
    // After the disturbance everything settles and sleeps again.
    let mut again = false;
    for _ in 0..1200 {
        w.step().unwrap();
        if w.sleeping_count() == levels + 1 {
            again = true;
            break;
        }
    }
    assert!(again, "the world sleeps again");
}

#[test]
fn sleeping_matches_the_awake_world_until_it_sleeps_and_is_deterministic() {
    let (bodies, cfg) = stack(3, 8);
    let colliders = [cube(material(0.6))];
    let mut awake = RigidWorld::new(cfg, &bodies, &colliders).unwrap();
    let mut sleepy = RigidWorld::new(cfg, &bodies, &colliders).unwrap();
    sleepy
        .set_sleeping(Some(SleepConfig {
            ticks: 1_000_000,
            ..SleepConfig::default()
        }))
        .unwrap();
    for _ in 0..120 {
        awake.step().unwrap();
        sleepy.step().unwrap();
    }
    // With a sleep delay that is never reached, sleeping changes nothing
    // about the bodies.
    for i in 0..3 {
        assert_eq!(awake.body(i), sleepy.body(i));
    }
    // Two sleeping runs are bit-identical.
    let run = || {
        let mut w = RigidWorld::new(cfg, &bodies, &colliders).unwrap();
        w.set_sleeping(Some(SleepConfig::default())).unwrap();
        for _ in 0..300 {
            w.step().unwrap();
        }
        w.hash()
    };
    assert_eq!(run(), run());
    let mut w = RigidWorld::new(cfg, &bodies, &colliders).unwrap();
    for bad in [
        SleepConfig {
            ticks: 0,
            ..SleepConfig::default()
        },
        SleepConfig {
            linear_threshold: f32::NAN,
            ..SleepConfig::default()
        },
        SleepConfig {
            angular_threshold: -1.0,
            ..SleepConfig::default()
        },
    ] {
        assert!(matches!(
            w.set_sleeping(Some(bad)),
            Err(RigidError::InvalidConfig(_))
        ));
    }
}

/// A fast ball aimed past the corner of a static box onto its side face.
/// At the start of the tick the closest feature is the box's top edge, so
/// a speculative contact has the diagonal edge normal; the impact is on the
/// face, whose normal is −x.
fn corner_shot(ccd: bool) -> RigidWorld {
    let r = 0.1f32;
    let mut shot = RigidBody::new([-1.5, 1.5, 0.0], 1.0, sphere_inv_inertia(1.0, r), 1);
    let dt = 1.0 / 120.0;
    shot.vel = [1.0 / dt, -1.2 / dt, 0.0];
    let wall = RigidBody {
        inv_mass: 0.0,
        ..RigidBody::new([0.0; 3], 0.0, [0.0; 3], 0)
    };
    let cfg = RigidConfig {
        ground: None,
        gravity: [0.0; 3],
        dt,
        ..RigidConfig::default()
    };
    let mut w = RigidWorld::new(
        cfg,
        &[shot, wall],
        &[cube(material(0.0)), ball(r, material(0.0))],
    )
    .unwrap();
    if ccd {
        w.set_ccd(Some(CcdConfig::default())).unwrap();
    }
    w
}

#[test]
fn ccd_hits_the_face_at_the_time_of_impact() {
    let mut with = corner_shot(true);
    let mut without = corner_shot(false);
    with.step().unwrap();
    without.step().unwrap();
    assert_eq!(with.last_ccd_hits(), 1);
    assert_eq!(without.last_ccd_hits(), 0);
    let n_ccd = with.manifolds()[0].normal;
    let n_spec = without.manifolds()[0].normal;
    eprintln!("ccd normal {n_ccd:?}, speculative normal {n_spec:?}");
    assert!(
        (n_ccd[0] + 1.0).abs() < 1e-3 && n_ccd[1].abs() < 1e-3,
        "{n_ccd:?}"
    );
    assert!(
        n_spec[1] > 0.3,
        "speculative contact uses the edge normal: {n_spec:?}"
    );
    // Frictionless, no restitution: with CCD the face stops the x motion
    // and the ball keeps sliding down the face at its original speed.
    let (vc, vs) = (with.body(0).unwrap().vel, without.body(0).unwrap().vel);
    eprintln!("after impact: ccd {vc:?}, speculative {vs:?}");
    let vy = -1.2 * 120.0;
    assert!(vc[0].abs() < 0.5, "ccd x velocity {vc:?}");
    assert!(
        (vc[1] - vy).abs() < 0.01 * vy.abs(),
        "ccd keeps the tangential speed {vc:?}"
    );
    assert!(
        (vs[1] - vy).abs() > 0.1 * vy.abs(),
        "speculative deflects: {vs:?}"
    );
    // Neither run lets the ball into the box.
    for _ in 0..30 {
        with.step().unwrap();
        let p = with.body(0).unwrap().pos;
        assert!(
            p[0] < -0.59 || p[1] > 0.59 || p[1] < -0.59,
            "inside the box at {p:?}"
        );
    }
}

#[test]
fn ccd_bullets_do_not_cross_a_thin_wall_or_the_ground() {
    let thin = RigidCollider {
        shape: ColliderShape::Box {
            half_extents: [0.01, 1.0, 1.0],
        },
        material: material(0.0),
    };
    let r = 0.02f32;
    let dt = 1.0 / 60.0;
    let mut bodies = vec![RigidBody {
        inv_mass: 0.0,
        ..RigidBody::new([0.0, 1.5, 0.0], 0.0, [0.0; 3], 0)
    }];
    for k in 0..4 {
        // 600 m/s: 10 m per tick against a 2 cm wall.
        let mut b = RigidBody::new(
            [-5.0, 1.0 + 0.2 * k as f32, -0.3 + 0.2 * k as f32],
            1.0,
            sphere_inv_inertia(1.0, r),
            1,
        );
        b.vel = [600.0, 0.0, 0.0];
        bodies.push(b);
    }
    // A spinning box thrown at the ground.
    let mut spinner = RigidBody::new([4.0, 3.0, 0.0], 1.0, box_inv_inertia(1.0, [0.5; 3]), 2);
    spinner.vel = [0.0, -150.0, 0.0];
    spinner.ang_vel = [40.0, 0.0, 25.0];
    bodies.push(spinner);
    let cfg = RigidConfig {
        dt,
        substeps: 8,
        gravity: [0.0; 3],
        ..RigidConfig::default()
    };
    let colliders = [thin, ball(r, material(0.0)), cube(material(0.5))];
    let mut w = RigidWorld::new(cfg, &bodies, &colliders).unwrap();
    w.set_ccd(Some(CcdConfig::default())).unwrap();
    let mut hits = 0;
    for _ in 0..60 {
        w.step().unwrap();
        hits += w.last_ccd_hits();
        for k in 1..5 {
            let p = w.body(k).unwrap().pos;
            assert!(p[0] < 0.0, "bullet {k} crossed the wall: {p:?}");
        }
        let s = w.body(5).unwrap();
        assert!(s.pos[1] > 0.4, "box went through the ground: {s:?}");
    }
    assert!(hits >= 5, "CCD found the impacts: {hits}");
    assert!(matches!(
        w.set_ccd(Some(CcdConfig {
            motion_fraction: 0.0,
            ..CcdConfig::default()
        })),
        Err(RigidError::InvalidConfig(_))
    ));
}

#[test]
fn adding_a_joint_wakes_the_bodies_it_binds() {
    let cfg = RigidConfig {
        gravity: [0.0; 3],
        ground: None,
        ..RigidConfig::default()
    };
    let b = RigidBody::new([0.0; 3], 1.0, sphere_inv_inertia(1.0, 0.3), 0);
    let mut w = RigidWorld::new(cfg, &[b], &[ball(0.3, material(0.5))]).unwrap();
    w.set_sleeping(Some(SleepConfig::default())).unwrap();
    for _ in 0..40 {
        w.step().unwrap();
    }
    assert!(w.is_asleep(0));
    // A rigid ball joint from the centre to a world point one unit away.
    w.add_joint(RigidJoint::ball(0, None, [0.0; 3], [1.0, 0.0, 0.0]))
        .unwrap();
    assert!(!w.is_asleep(0));
    for _ in 0..5 {
        w.step().unwrap();
    }
    let p = w.body(0).unwrap().pos;
    assert!((p[0] - 1.0).abs() < 1e-3, "pulled to the anchor: {p:?}");
}
