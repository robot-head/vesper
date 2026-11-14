//! Capsules along any body axis, and one scene with every new rigid
//! feature (joints, gyroscopic term, sleeping, CCD, segment capsules)
//! pinned to a golden hash and checked across both broadphases.

use vesper_kinetic::rigid::{
    CcdConfig, ColliderShape, JointKind, RigidBody, RigidBroadphase, RigidCollider, RigidConfig,
    RigidError, RigidJoint, RigidMaterial, RigidWorld, SleepConfig, box_inv_inertia,
    capsule_inv_inertia, segment_capsule_inertia, sphere_inv_inertia,
};

fn material(mu: f32) -> RigidMaterial {
    RigidMaterial {
        static_friction: mu,
        dynamic_friction: mu,
        restitution: 0.0,
        rolling_friction: 0.0,
    }
}

#[test]
fn a_capsule_along_any_axis_matches_the_rotated_y_capsule() {
    let (half, r) = (0.4f32, 0.15f32);
    let s = std::f32::consts::FRAC_1_SQRT_2;
    // Segment along body (1, 0, 1)/√2, identity orientation.
    let axis = [s * half, 0.0, s * half];
    let seg = RigidCollider {
        shape: ColliderShape::Segment {
            a: [-axis[0], 0.0, -axis[2]],
            b: axis,
            radius: r,
        },
        material: material(0.5),
    };
    let (inv, frame) = segment_capsule_inertia(1.0, seg_a(&seg), seg_b(&seg), r).unwrap();
    assert_eq!(inv, capsule_inv_inertia(1.0, half, r));
    let mut a = RigidBody::new([0.0, 0.6, 0.0], 1.0, inv, 0);
    a.inertia_frame = frame;
    a.ang_vel = [0.4, 0.0, -0.3];
    // The same capsule as a y capsule turned onto that axis.
    let y = RigidCollider {
        shape: ColliderShape::Capsule {
            half_height: half,
            radius: r,
        },
        material: material(0.5),
    };
    let mut b = RigidBody::new([0.0, 0.6, 0.0], 1.0, inv, 0);
    b.orientation = frame;
    b.ang_vel = a.ang_vel;
    let cfg = RigidConfig::default();
    let mut wa = RigidWorld::new(cfg, &[a], &[seg]).unwrap();
    let mut wb = RigidWorld::new(cfg, &[b], &[y]).unwrap();
    for _ in 0..240 {
        wa.step().unwrap();
        wb.step().unwrap();
    }
    let (ba, bb) = (wa.body(0).unwrap(), wb.body(0).unwrap());
    eprintln!("segment {:?}\ny-capsule {:?}", ba.pos, bb.pos);
    for k in 0..3 {
        assert!(
            (ba.pos[k] - bb.pos[k]).abs() < 2e-3,
            "{:?} vs {:?}",
            ba.pos,
            bb.pos
        );
    }
    assert!(
        (ba.pos[1] - r).abs() < 3e-3,
        "lies on the ground: {:?}",
        ba.pos
    );
    assert_eq!(wa.manifolds()[0].count, 2, "a lying capsule has two points");
    assert_eq!(wa.body(0).unwrap().inertia_frame, frame);
    // A segment that is not centred is rejected by the inertia helper.
    assert!(matches!(
        segment_capsule_inertia(1.0, [0.0; 3], [1.0, 0.0, 0.0], r),
        Err(RigidError::InvalidCollider { .. })
    ));
}

fn seg_a(c: &RigidCollider) -> [f32; 3] {
    match c.shape {
        ColliderShape::Segment { a, .. } => a,
        _ => unreachable!(),
    }
}

fn seg_b(c: &RigidCollider) -> [f32; 3] {
    match c.shape {
        ColliderShape::Segment { b, .. } => b,
        _ => unreachable!(),
    }
}

fn feature_scene(broadphase: RigidBroadphase) -> RigidWorld {
    let colliders = [
        RigidCollider {
            shape: ColliderShape::Box {
                half_extents: [0.5; 3],
            },
            material: material(0.6),
        },
        RigidCollider {
            shape: ColliderShape::Sphere { radius: 0.2 },
            material: material(0.6),
        },
        RigidCollider {
            shape: ColliderShape::Segment {
                a: [-0.3, -0.2, 0.0],
                b: [0.3, 0.2, 0.0],
                radius: 0.1,
            },
            material: material(0.6),
        },
        RigidCollider {
            shape: ColliderShape::Box {
                half_extents: [0.05, 0.3, 0.05],
            },
            material: material(0.6),
        },
    ];
    let cube = |p: [f32; 3]| RigidBody::new(p, 1.0, box_inv_inertia(1.0, [0.5; 3]), 0);
    let mut bodies = vec![cube([0.0, 0.5, 0.0]), cube([0.05, 1.5, 0.0])];
    // A fast ball (CCD) at the stack.
    let mut shot = RigidBody::new([-6.0, 1.5, 0.0], 1.0, sphere_inv_inertia(1.0, 0.2), 1);
    shot.vel = [30.0, 0.0, 0.0];
    bodies.push(shot);
    // A segment capsule dropped with spin (gyroscopic).
    let (inv, frame) =
        segment_capsule_inertia(1.0, [-0.3, -0.2, 0.0], [0.3, 0.2, 0.0], 0.1).unwrap();
    let mut cap = RigidBody::new([3.0, 2.0, 1.0], 1.0, inv, 2);
    cap.inertia_frame = frame;
    cap.ang_vel = [3.0, 7.0, -2.0];
    bodies.push(cap);
    // A three-link hinge chain hanging from the world, and a prismatic
    // slider.
    let rod = box_inv_inertia(1.0, [0.05, 0.3, 0.05]);
    for k in 0..3 {
        bodies.push(RigidBody::new(
            [-3.0, 4.0 - 0.3 - 0.6 * k as f32, 0.0],
            1.0,
            rod,
            3,
        ));
    }
    let mut slider = RigidBody::new([3.0, 4.0, -2.0], 1.0, rod, 3);
    slider.vel = [0.0, 0.0, 1.0];
    bodies.push(slider);
    let cfg = RigidConfig {
        broadphase,
        substeps: 12,
        ..RigidConfig::default()
    };
    let mut w = RigidWorld::new(cfg, &bodies, &colliders).unwrap();
    w.set_gyroscopic(true);
    w.set_ccd(Some(CcdConfig::default())).unwrap();
    w.set_sleeping(Some(SleepConfig::default())).unwrap();
    let hinge = JointKind::Hinge {
        axis_a: [0.0, 0.0, 1.0],
        axis_b: [0.0, 0.0, 1.0],
        limits: Some([-1.0, 1.0]),
    };
    w.add_joint(RigidJoint {
        kind: hinge,
        ..RigidJoint::ball(4, None, [0.0, 0.3, 0.0], [-3.0, 4.0, 0.0])
    })
    .unwrap();
    for k in 5..7u32 {
        w.add_joint(RigidJoint {
            kind: hinge,
            ..RigidJoint::ball(k, Some(k - 1), [0.0, 0.3, 0.0], [0.0, -0.3, 0.0])
        })
        .unwrap();
    }
    w.apply_velocity_change(6, [2.0, 0.0, 0.0], [0.0; 3])
        .unwrap();
    w.add_joint(RigidJoint {
        kind: JointKind::Prismatic {
            axis_a: [0.0, 0.6, 0.8],
            limits: Some([-0.5, 0.5]),
        },
        ..RigidJoint::ball(7, None, [0.0; 3], [3.0, 4.0, -2.0])
    })
    .unwrap();
    w
}

const FEATURE_TICKS: u32 = 300;
const GOLDEN_FEATURE_SHA256: &str =
    "81337213e19f731f3510e56f8461e0fcca60f2be7ba133a22ae3a354ee17783d";

#[test]
fn feature_scene_is_bit_identical_across_broadphases_and_pinned() {
    let mut hashes = Vec::new();
    for bp in [RigidBroadphase::Lbvh, RigidBroadphase::SweepAndPrune] {
        let mut w = feature_scene(bp);
        let mut ccd = 0;
        let mut slept = 0;
        for _ in 0..FEATURE_TICKS {
            w.step().unwrap();
            ccd += w.last_ccd_hits();
            slept = slept.max(w.sleeping_count());
        }
        assert!(ccd > 0, "CCD ran");
        assert!(slept > 0, "something slept");
        for i in 0..w.len() {
            let b = w.body(i).unwrap();
            assert!(
                b.pos.iter().all(|v| v.is_finite() && v.abs() < 50.0),
                "{i} {b:?}"
            );
        }
        hashes.push(w.hash());
    }
    assert_eq!(hashes[0], hashes[1]);
    let hex: String = hashes[0]
        .sha256
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    eprintln!("arch={} feature sha256={hex}", std::env::consts::ARCH);
    assert_eq!(hex, GOLDEN_FEATURE_SHA256);
}
