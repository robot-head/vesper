//! Rigid bodies with GJK/EPA colliders, manifolds, angular dynamics,
//! per-collider materials and rolling friction: physical behaviour, typed
//! errors and a cross-architecture golden hash.

use vesper_kinetic::rigid::{
    ColliderShape, GroundPlane, RigidBody, RigidBroadphase, RigidCollider, RigidConfig, RigidError,
    RigidMaterial, RigidWorld, box_inv_inertia, capsule_inv_inertia, sphere_inv_inertia,
};

fn material(mu: f32) -> RigidMaterial {
    RigidMaterial {
        static_friction: mu,
        dynamic_friction: mu,
        restitution: 0.0,
        rolling_friction: 0.0,
    }
}

fn config(ground: RigidMaterial) -> RigidConfig {
    RigidConfig {
        ground: Some(GroundPlane {
            height: 0.0,
            material: ground,
        }),
        ..RigidConfig::default()
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

fn cube_body(pos: [f32; 3], collider: u32) -> RigidBody {
    RigidBody::new(pos, 1.0, box_inv_inertia(1.0, [0.5; 3]), collider)
}

fn run(world: &mut RigidWorld, ticks: u32) {
    for t in 0..ticks {
        world.step().unwrap_or_else(|e| panic!("tick {t}: {e}"));
    }
}

fn speed(b: &RigidBody) -> f32 {
    b.vel.iter().map(|v| v * v).sum::<f32>().sqrt()
}

fn spin(b: &RigidBody) -> f32 {
    b.ang_vel.iter().map(|v| v * v).sum::<f32>().sqrt()
}

/// The body's local axis `k` in world coordinates.
fn axis(b: &RigidBody, k: usize) -> [f32; 3] {
    let [x, y, z, w] = b.orientation;
    let m = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];
    [m[0][k], m[1][k], m[2][k]]
}

fn quat_about(axis: [f32; 3], angle: f32) -> [f32; 4] {
    let (s, c) = ((angle * 0.5).sin(), (angle * 0.5).cos());
    [axis[0] * s, axis[1] * s, axis[2] * s, c]
}

#[test]
fn a_box_rests_flat_on_a_four_point_manifold() {
    let mut w = RigidWorld::new(
        config(material(0.5)),
        &[cube_body([0.0, 0.505, 0.0], 0)],
        &[cube(material(0.5))],
    )
    .unwrap();
    run(&mut w, 240);
    let b = w.body(0).unwrap();
    assert_eq!(w.manifolds().len(), 1);
    assert_eq!(w.manifolds()[0].count, 4, "face contact keeps four corners");
    assert!((b.pos[1] - 0.5).abs() < 5e-3, "height {}", b.pos[1]);
    assert!(
        b.pos[0].abs() < 1e-3 && b.pos[2].abs() < 1e-3,
        "drift {:?}",
        b.pos
    );
    assert!(speed(&b) < 1e-2 && spin(&b) < 1e-2, "{b:?}");
    assert!((b.orientation[3] - 1.0).abs() < 1e-4, "{:?}", b.orientation);
}

#[test]
fn a_stack_of_three_boxes_stays_up() {
    let bodies: Vec<RigidBody> = (0..3)
        .map(|i| cube_body([0.01 * i as f32, 0.505 + 1.01 * i as f32, 0.0], 0))
        .collect();
    let mut w = RigidWorld::new(config(material(0.6)), &bodies, &[cube(material(0.6))]).unwrap();
    run(&mut w, 240);
    let before: Vec<[f32; 3]> = (0..3).map(|i| w.body(i).unwrap().pos).collect();
    run(&mut w, 120);
    for (i, start) in before.iter().enumerate() {
        let b = w.body(i).unwrap();
        assert!(
            (b.pos[1] - (0.5 + i as f32)).abs() < 0.03,
            "box {i} height {}",
            b.pos[1]
        );
        assert!(
            (b.pos[0] - 0.01 * i as f32).abs() < 0.03,
            "box {i} x {}",
            b.pos[0]
        );
        // The stack jitters (a few cm/s at 8 substeps, less with more) but
        // does not creep: over the last second no box moves 5 mm.
        let moved: f32 = (0..3)
            .map(|k| (b.pos[k] - start[k]).powi(2))
            .sum::<f32>()
            .sqrt();
        assert!(moved < 5e-3, "box {i} crept {moved}");
        assert!(speed(&b) < 0.06 && spin(&b) < 0.06, "box {i} {b:?}");
    }
    assert!(w.manifolds().iter().all(|m| m.count == 4));
}

#[test]
fn a_tilted_box_falls_and_settles_on_a_face() {
    let mut b = cube_body([0.0, 1.5, 0.0], 0);
    b.orientation = quat_about([0.0, 0.0, 1.0], 0.5);
    b.ang_vel = [0.3, 0.0, 0.5];
    let mut w = RigidWorld::new(config(material(0.5)), &[b], &[cube(material(0.5))]).unwrap();
    run(&mut w, 480);
    let b = w.body(0).unwrap();
    assert!((b.pos[1] - 0.5).abs() < 0.02, "height {}", b.pos[1]);
    assert!(speed(&b) < 0.02 && spin(&b) < 0.05, "{b:?}");
    // Some body axis is vertical.
    let up = (0..3).map(|k| axis(&b, k)[1].abs()).fold(0.0f32, f32::max);
    assert!(up > 0.999, "no face down: {:?}", b.orientation);
}

#[test]
fn sliding_box_stops_at_the_coulomb_distance() {
    let mut b = cube_body([0.0, 0.5, 0.0], 0);
    b.vel = [3.0, 0.0, 0.0];
    let mut w = RigidWorld::new(config(material(0.5)), &[b], &[cube(material(0.5))]).unwrap();
    run(&mut w, 240);
    let b = w.body(0).unwrap();
    let expected = 3.0f32 * 3.0 / (2.0 * 0.5 * 9.81);
    assert!(speed(&b) < 1e-2, "{b:?}");
    assert!(
        (b.pos[0] - expected).abs() < 0.1 * expected,
        "slid {} expected {expected}",
        b.pos[0]
    );
    // It slides without tipping over.
    assert!(axis(&b, 1)[1] > 0.999);
}

#[test]
fn static_friction_holds_below_the_coulomb_limit() {
    for (mu, holds) in [(0.5f32, true), (0.2, false)] {
        let cfg = RigidConfig {
            gravity: [0.3 * 9.81, -9.81, 0.0],
            ..config(material(mu))
        };
        let mut w =
            RigidWorld::new(cfg, &[cube_body([0.0, 0.5, 0.0], 0)], &[cube(material(mu))]).unwrap();
        run(&mut w, 120);
        let x = w.body(0).unwrap().pos[0];
        if holds {
            assert!(x.abs() < 2e-3, "mu {mu}: moved {x}");
        } else {
            // a = (0.3 - 0.2) g for one second: 0.49 m.
            assert!((x - 0.49).abs() < 0.1, "mu {mu}: moved {x}");
        }
    }
}

#[test]
fn per_collider_materials_change_the_outcome() {
    let mut a = cube_body([0.0, 0.5, 0.0], 0);
    let mut b = cube_body([0.0, 0.5, 3.0], 1);
    a.vel = [2.0, 0.0, 0.0];
    b.vel = [2.0, 0.0, 0.0];
    let mut w = RigidWorld::new(
        config(material(0.4)),
        &[a, b],
        &[cube(material(0.0)), cube(material(0.8))],
    )
    .unwrap();
    run(&mut w, 240);
    let (xa, xb) = (w.body(0).unwrap().pos[0], w.body(1).unwrap().pos[0]);
    // Combined μ: (0 + 0.4)/2 = 0.2 and (0.8 + 0.4)/2 = 0.6.
    let da = 4.0 / (2.0 * 0.2 * 9.81);
    let db = 4.0 / (2.0 * 0.6 * 9.81);
    assert!(
        (xa - da).abs() < 0.1 * da,
        "slippery box slid {xa}, expected {da}"
    );
    assert!(
        (xb - db).abs() < 0.1 * db,
        "grippy box slid {xb}, expected {db}"
    );
}

#[test]
fn a_ball_slips_into_rolling_and_rolling_friction_stops_it() {
    let r = 0.5;
    let mut results = Vec::new();
    for rolling in [0.0f32, 0.02] {
        let m = RigidMaterial {
            rolling_friction: rolling,
            ..material(0.5)
        };
        let mut b = RigidBody::new([0.0, r, 0.0], 1.0, sphere_inv_inertia(1.0, r), 0);
        b.vel = [2.0, 0.0, 0.0];
        let mut w = RigidWorld::new(config(m), &[b], &[ball(r, m)]).unwrap();
        run(&mut w, 120);
        let b = w.body(0).unwrap();
        results.push((b, w.body(0).unwrap().pos[0]));
        if rolling == 0.0 {
            // Slipping ends at v = 5/7 v0, then it rolls: ω = -v/r.
            let v = b.vel[0];
            assert!((v - 2.0 * 5.0 / 7.0).abs() < 0.05, "rolling speed {v}");
            assert!((b.ang_vel[2] + v / r).abs() < 0.05, "ω {:?}", b.ang_vel);
            // It keeps rolling.
            run(&mut w, 120);
            assert!((w.body(0).unwrap().vel[0] - v).abs() < 0.02);
        } else {
            // Rolling resistance a = μ_r g / (r (1 + 2/5)) = 0.28 m/s² stops
            // 1.43 m/s in about 5 s.
            run(&mut w, 120 * 6);
            let b = w.body(0).unwrap();
            assert!(speed(&b) < 0.02 && spin(&b) < 0.05, "still rolling: {b:?}");
        }
    }
}

#[test]
fn restitution_bounces_to_e_squared_height() {
    let r = 0.25;
    for e in [0.0f32, 0.5, 0.8] {
        let m = RigidMaterial {
            restitution: e,
            ..material(0.3)
        };
        let b = RigidBody::new([0.0, 1.0 + r, 0.0], 1.0, sphere_inv_inertia(1.0, r), 0);
        let mut w = RigidWorld::new(config(m), &[b], &[ball(r, m)]).unwrap();
        let mut bounced = false;
        let mut peak = 0.0f32;
        for _ in 0..240 {
            w.step().unwrap();
            let b = w.body(0).unwrap();
            if b.vel[1] > 0.0 {
                bounced = true;
            }
            if bounced {
                peak = peak.max(b.pos[1] - r);
            }
        }
        let want = e * e;
        if e == 0.0 {
            assert!(peak < 0.01, "e=0 bounced to {peak}");
        } else {
            assert!(
                (peak - want).abs() < 0.12 * want,
                "e={e}: peak {peak}, want {want}"
            );
        }
    }
}

#[test]
fn a_capsule_lies_on_two_points_and_a_hull_comes_to_rest() {
    let half = 0.5;
    let r = 0.2;
    let mut cap = RigidBody::new(
        [0.0, r + 0.01, 0.0],
        1.0,
        capsule_inv_inertia(1.0, half, r),
        0,
    );
    cap.orientation = quat_about([0.0, 0.0, 1.0], std::f32::consts::FRAC_PI_2);
    let octa = vec![
        [0.4, 0.0, 0.0],
        [-0.4, 0.0, 0.0],
        [0.0, 0.4, 0.0],
        [0.0, -0.4, 0.0],
        [0.0, 0.0, 0.4],
        [0.0, 0.0, -0.4],
    ];
    let mut hull = RigidBody::new([3.0, 1.0, 0.0], 1.0, box_inv_inertia(1.0, [0.3; 3]), 1);
    hull.orientation = quat_about([0.6, 0.0, 0.8], 0.3);
    let mut w = RigidWorld::new(
        config(material(0.5)),
        &[cap, hull],
        &[
            RigidCollider {
                shape: ColliderShape::Capsule {
                    half_height: half,
                    radius: r,
                },
                material: material(0.5),
            },
            RigidCollider {
                shape: ColliderShape::ConvexHull { vertices: octa },
                material: material(0.5),
            },
        ],
    )
    .unwrap();
    run(&mut w, 480);
    let c = w.body(0).unwrap();
    assert!((c.pos[1] - r).abs() < 5e-3, "capsule height {}", c.pos[1]);
    assert!(axis(&c, 1)[1].abs() < 1e-3, "capsule still lies flat");
    let ground = w
        .manifolds()
        .iter()
        .find(|m| m.a == 0 && m.b.is_none())
        .unwrap();
    assert_eq!(ground.count, 2, "two end caps touch");
    let h = w.body(1).unwrap();
    assert!(speed(&h) < 0.02 && spin(&h) < 0.05, "{h:?}");
    // An octahedron rests on a face: its centre is at the inradius 0.4/√3.
    let inradius = 0.4 / 3.0f32.sqrt();
    assert!(
        (h.pos[1] - inradius).abs() < 0.01,
        "hull height {}",
        h.pos[1]
    );
}

#[test]
fn a_ball_rests_on_a_static_box_and_spin_is_conserved_in_free_flight() {
    let table = RigidBody {
        inv_mass: 0.0,
        ..cube_body([0.0, 0.5, 0.0], 0)
    };
    let r = 0.25;
    let ball_body = RigidBody::new(
        [0.1, 1.0 + r + 0.01, 0.05],
        1.0,
        sphere_inv_inertia(1.0, r),
        1,
    );
    let mut spinner = RigidBody::new([10.0, 5.0, 0.0], 1.0, box_inv_inertia(1.0, [0.5; 3]), 0);
    spinner.ang_vel = [0.0, 3.0, 0.0];
    spinner.layer_mask = 2;
    let cfg = config(material(0.5));
    let mut w = RigidWorld::new(
        cfg,
        &[table, ball_body],
        &[cube(material(0.5)), ball(r, material(0.5))],
    )
    .unwrap();
    run(&mut w, 240);
    let b = w.body(1).unwrap();
    assert!(
        (b.pos[1] - (1.0 + r)).abs() < 5e-3,
        "ball height {}",
        b.pos[1]
    );
    assert!(speed(&b) < 1e-2);
    assert_eq!(
        w.body(0).unwrap().pos,
        [0.0, 0.5, 0.0],
        "static body never moves"
    );

    let free = RigidConfig {
        gravity: [0.0; 3],
        ground: None,
        ..RigidConfig::default()
    };
    let mut w = RigidWorld::new(free, &[spinner], &[cube(material(0.5))]).unwrap();
    let e0 = w.kinetic_energy();
    run(&mut w, 120);
    let s = w.body(0).unwrap();
    assert!((s.ang_vel[1] - 3.0).abs() < 1e-4, "{:?}", s.ang_vel);
    let n: f32 = s.orientation.iter().map(|q| q * q).sum();
    assert!((n - 1.0).abs() < 1e-5);
    assert!((w.kinetic_energy() - e0).abs() < 1e-4 * e0);
    // One second at 3 rad/s about y.
    let expected = quat_about([0.0, 1.0, 0.0], 3.0);
    let dot: f32 = s.orientation.iter().zip(expected).map(|(a, b)| a * b).sum();
    assert!(dot.abs() > 0.9999, "{:?} vs {expected:?}", s.orientation);
}

#[test]
fn malformed_worlds_are_typed_errors() {
    let ok = [cube(material(0.5))];
    let body = [cube_body([0.0, 1.0, 0.0], 0)];
    let bad_cfg = RigidConfig {
        dt: 0.0,
        ..RigidConfig::default()
    };
    assert!(matches!(
        RigidWorld::new(bad_cfg, &body, &ok),
        Err(RigidError::InvalidConfig(_))
    ));
    for shape in [
        ColliderShape::Sphere { radius: -1.0 },
        ColliderShape::Box {
            half_extents: [1.0, 0.0, 1.0],
        },
        ColliderShape::Capsule {
            half_height: f32::NAN,
            radius: 1.0,
        },
        ColliderShape::ConvexHull {
            vertices: vec![[0.0; 3]; 3],
        },
        ColliderShape::ConvexHull {
            vertices: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0],
            ],
        },
    ] {
        let c = [RigidCollider {
            shape,
            material: material(0.5),
        }];
        assert!(matches!(
            RigidWorld::new(RigidConfig::default(), &body, &c),
            Err(RigidError::InvalidCollider { collider: 0, .. })
        ));
    }
    let bad_mat = [RigidCollider {
        material: RigidMaterial {
            restitution: 1.5,
            ..material(0.5)
        },
        ..cube(material(0.5))
    }];
    assert!(matches!(
        RigidWorld::new(RigidConfig::default(), &body, &bad_mat),
        Err(RigidError::InvalidCollider { .. })
    ));
    let mut b = body[0];
    b.orientation = [0.0, 0.0, 0.0, 2.0];
    assert!(matches!(
        RigidWorld::new(RigidConfig::default(), &[b], &ok),
        Err(RigidError::InvalidBody { body: 0, .. })
    ));
    b = body[0];
    b.collider = 3;
    assert!(matches!(
        RigidWorld::new(RigidConfig::default(), &[b], &ok),
        Err(RigidError::UnknownCollider {
            body: 0,
            collider: 3
        })
    ));
    b = body[0];
    b.inv_inertia = [1.0, -1.0, 1.0];
    assert!(matches!(
        RigidWorld::new(RigidConfig::default(), &[b], &ok),
        Err(RigidError::InvalidBody { .. })
    ));
    // Too many manifolds for the reserved capacity.
    let cfg = RigidConfig {
        max_manifolds: 1,
        ..config(material(0.5))
    };
    let two = [cube_body([0.0, 0.5, 0.0], 0), cube_body([3.0, 0.5, 0.0], 0)];
    let mut w = RigidWorld::new(cfg, &two, &ok).unwrap();
    let before = w.hash();
    assert_eq!(w.step(), Err(RigidError::ManifoldOverflow { capacity: 1 }));
    assert_eq!(w.hash(), before, "a failed step changes nothing");
    assert!(w.apply_velocity_change(5, [1.0; 3], [0.0; 3]).is_err());
}

/// A mixed scene: a box stack, a tumbling box, rolling and bouncing balls,
/// a capsule, a hull and a static ramp block.
fn mixed_scene(broadphase: RigidBroadphase) -> RigidWorld {
    let bouncy = RigidMaterial {
        restitution: 0.6,
        rolling_friction: 0.01,
        ..material(0.4)
    };
    let colliders = vec![
        cube(material(0.6)),
        ball(0.25, bouncy),
        RigidCollider {
            shape: ColliderShape::Capsule {
                half_height: 0.4,
                radius: 0.15,
            },
            material: material(0.5),
        },
        RigidCollider {
            shape: ColliderShape::ConvexHull {
                vertices: vec![
                    [0.4, 0.0, 0.0],
                    [-0.4, 0.0, 0.0],
                    [0.0, 0.4, 0.0],
                    [0.0, -0.4, 0.0],
                    [0.0, 0.0, 0.4],
                    [0.0, 0.0, -0.4],
                ],
            },
            material: material(0.5),
        },
    ];
    let mut bodies = Vec::new();
    for i in 0..3 {
        bodies.push(cube_body([0.0, 0.5 + 1.0 * i as f32, 0.0], 0));
    }
    let mut tumbler = cube_body([2.5, 2.0, 0.5], 0);
    tumbler.orientation = quat_about([0.6, 0.0, 0.8], 0.7);
    tumbler.ang_vel = [1.0, 2.0, -0.5];
    bodies.push(tumbler);
    let mut roller = RigidBody::new([-3.0, 0.25, 0.0], 1.0, sphere_inv_inertia(1.0, 0.25), 1);
    roller.vel = [2.0, 0.0, 0.25];
    bodies.push(roller);
    let mut dropper = RigidBody::new([0.2, 4.5, 0.1], 1.0, sphere_inv_inertia(1.0, 0.25), 1);
    dropper.vel = [0.0, -1.0, 0.0];
    bodies.push(dropper);
    let mut cap = RigidBody::new(
        [1.0, 1.2, -2.0],
        1.0,
        capsule_inv_inertia(1.0, 0.4, 0.15),
        2,
    );
    cap.orientation = quat_about([1.0, 0.0, 0.0], 0.9);
    bodies.push(cap);
    let mut hull = RigidBody::new([-1.5, 1.5, -1.5], 1.0, box_inv_inertia(1.0, [0.3; 3]), 3);
    hull.ang_vel = [0.0, 0.0, 2.0];
    bodies.push(hull);
    bodies.push(RigidBody {
        inv_mass: 0.0,
        ..cube_body([-1.5, 0.5, 1.5], 0)
    });
    let cfg = RigidConfig {
        broadphase,
        ..config(material(0.5))
    };
    RigidWorld::new(cfg, &bodies, &colliders).unwrap()
}

const RIGID_TICKS: u32 = 240;
const GOLDEN_RIGID_SHA256: &str =
    "dbfa41eda96f2f37a877133e7df18566427f53d2f29e0ac1b276c10860d1962b";

#[test]
fn mixed_scene_is_bit_identical_across_broadphases_and_pinned() {
    let mut hashes = Vec::new();
    for bp in [RigidBroadphase::Lbvh, RigidBroadphase::SweepAndPrune] {
        let mut w = mixed_scene(bp);
        let mut max_manifolds = 0;
        for _ in 0..RIGID_TICKS {
            let h = w.step().unwrap();
            max_manifolds = max_manifolds.max(h.manifold_count);
        }
        assert!(
            max_manifolds >= 6,
            "scene exercises contacts: {max_manifolds}"
        );
        for i in 0..w.len() {
            let b = w.body(i).unwrap();
            assert!(speed(&b) < 20.0, "body {i} {b:?}");
        }
        hashes.push(w.hash());
        assert_eq!(w.header().world_state_blake3, w.hash().blake3);
    }
    assert_eq!(hashes[0], hashes[1]);
    let hex: String = hashes[0]
        .sha256
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    eprintln!("arch={} rigid sha256={hex}", std::env::consts::ARCH);
    assert_eq!(hex, GOLDEN_RIGID_SHA256);
}
