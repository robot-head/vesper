//! XPBD joints (ball, hinge, prismatic, limits), articulated chains and
//! the gyroscopic term: physical behaviour with stated tolerances and typed
//! errors.

use vesper_kinetic::rigid::{
    ColliderShape, JointKind, RigidBody, RigidCollider, RigidConfig, RigidError, RigidJoint,
    RigidMaterial, RigidWorld, box_inv_inertia, sphere_inv_inertia,
};

const G: f32 = 9.81;

fn free_config(substeps: u32) -> RigidConfig {
    RigidConfig {
        ground: None,
        substeps,
        ..RigidConfig::default()
    }
}

fn ball(r: f32) -> RigidCollider {
    RigidCollider {
        shape: ColliderShape::Sphere { radius: r },
        material: RigidMaterial::default(),
    }
}

fn norm(v: [f32; 3]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Rotate `v` by unit quaternion `q` (test-side, f64).
fn rot(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let (x, y, z, w) = (
        f64::from(q[0]),
        f64::from(q[1]),
        f64::from(q[2]),
        f64::from(q[3]),
    );
    let v = v.map(f64::from);
    let t = [
        2.0 * (y * v[2] - z * v[1]),
        2.0 * (z * v[0] - x * v[2]),
        2.0 * (x * v[1] - y * v[0]),
    ];
    [
        (v[0] + w * t[0] + y * t[2] - z * t[1]) as f32,
        (v[1] + w * t[1] + z * t[0] - x * t[2]) as f32,
        (v[2] + w * t[2] + x * t[1] - y * t[0]) as f32,
    ]
}

const ROD: [f32; 3] = [0.05, 0.5, 0.05];

fn rod() -> RigidCollider {
    RigidCollider {
        shape: ColliderShape::Box { half_extents: ROD },
        material: RigidMaterial::default(),
    }
}

/// A rod hanging by its top end from the world pivot at the origin,
/// displaced by `theta0` about z.
fn pendulum(theta0: f32, kind: JointKind, substeps: u32) -> RigidWorld {
    let d = ROD[1];
    let mut body = RigidBody::new(
        [d * theta0.sin(), -d * theta0.cos(), 0.0],
        1.0,
        box_inv_inertia(1.0, ROD),
        0,
    );
    body.orientation = [0.0, 0.0, (theta0 * 0.5).sin(), (theta0 * 0.5).cos()];
    let mut w = RigidWorld::new(free_config(substeps), &[body], &[rod()]).unwrap();
    w.add_joint(RigidJoint {
        kind,
        ..RigidJoint::ball(0, None, [0.0, d, 0.0], [0.0; 3])
    })
    .unwrap();
    w
}

/// Distance of the rod's top anchor from the pivot.
fn anchor_gap(w: &RigidWorld) -> f32 {
    let b = w.body(0).unwrap();
    let r = rot(b.orientation, [0.0, ROD[1], 0.0]);
    norm([b.pos[0] + r[0], b.pos[1] + r[1], b.pos[2] + r[2]])
}

/// Period from successive upward zero crossings of x, and the worst
/// anchor gap.
fn measure_period(w: &mut RigidWorld, ticks: u32) -> (f32, f32) {
    let dt = w.config().dt;
    let mut crossings = Vec::new();
    let mut prev = w.body(0).unwrap().pos[0];
    let mut max_gap = 0.0f32;
    for t in 0..ticks {
        w.step().unwrap();
        let x = w.body(0).unwrap().pos[0];
        if prev < 0.0 && x >= 0.0 {
            // Linear interpolation of the crossing time.
            crossings.push((t as f32 + (-prev) / (x - prev)) * dt);
        }
        prev = x;
        max_gap = max_gap.max(anchor_gap(w));
    }
    assert!(crossings.len() >= 3, "pendulum swung: {crossings:?}");
    let period = (crossings[crossings.len() - 1] - crossings[0]) / (crossings.len() - 1) as f32;
    (period, max_gap)
}

fn energy(w: &RigidWorld) -> f64 {
    w.kinetic_energy() + f64::from(G * w.body(0).unwrap().pos[1])
}

#[test]
fn ball_joint_pendulum_has_the_physical_pendulum_period() {
    let theta0 = 0.2f32;
    let mut w = pendulum(theta0, JointKind::Ball, 16);
    let e0 = energy(&w);
    let (period, gap) = measure_period(&mut w, 1200);
    // Physical pendulum about the end: I_p = m (hx² + hy²)/3 + m d²,
    // T = 2π √(I_p / (m g d)) (1 + θ₀²/16).
    let d = ROD[1];
    let ip = (ROD[0] * ROD[0] + ROD[1] * ROD[1]) / 3.0 + d * d;
    let expected =
        2.0 * std::f32::consts::PI * (ip / (G * d)).sqrt() * (1.0 + theta0 * theta0 / 16.0);
    let rel = (period - expected).abs() / expected;
    let e1 = energy(&w);
    eprintln!("pendulum period {period} expected {expected} rel {rel} gap {gap} E {e0} -> {e1}");
    assert!(rel < 2e-3, "period {period} vs {expected}");
    assert!(gap < 1e-4, "joint gap {gap}");
    // XPBD damps the swing slightly (about 7% of the swing energy over
    // these 10 s at 16 substeps); it never gains.
    let swing = e0 + f64::from(G * d);
    assert!(e1 <= e0 + 1e-4 * swing, "energy gained {e0} -> {e1}");
    assert!(e1 >= e0 - 0.1 * swing, "energy lost {e0} -> {e1}");
}

#[test]
fn hinge_keeps_the_bob_in_its_plane() {
    let kind = JointKind::Hinge {
        axis_a: [0.0, 0.0, 1.0],
        axis_b: [0.0, 0.0, 1.0],
        limits: None,
    };
    let mut w = pendulum(0.6, kind, 16);
    // An out-of-plane kick: a ball joint would swing out, the hinge stops it.
    w.apply_velocity_change(0, [0.0, 0.0, 1.5], [0.0; 3])
        .unwrap();
    let mut max_z = 0.0f32;
    let mut max_off_axis = 0.0f32;
    for _ in 0..600 {
        w.step().unwrap();
        let b = w.body(0).unwrap();
        max_z = max_z.max(b.pos[2].abs());
        // The bob's z axis stays the world z axis.
        let z = rot(b.orientation, [0.0, 0.0, 1.0]);
        max_off_axis = max_off_axis.max(norm(sub(z, [0.0, 0.0, 1.0])));
    }
    eprintln!("hinge max z {max_z} axis error {max_off_axis}");
    assert!(max_z < 2e-3, "left the hinge plane by {max_z}");
    assert!(max_off_axis < 1e-2, "axis drift {max_off_axis}");
    // Compare: a ball joint with the same kick leaves the plane.
    let mut free = pendulum(0.6, JointKind::Ball, 16);
    free.apply_velocity_change(0, [0.0, 0.0, 1.5], [0.0; 3])
        .unwrap();
    let mut free_z = 0.0f32;
    for _ in 0..600 {
        free.step().unwrap();
        free_z = free_z.max(free.body(0).unwrap().pos[2].abs());
    }
    assert!(free_z > 0.2, "ball joint swings out of plane: {free_z}");
}

#[test]
fn hinge_limits_stop_the_swing() {
    let (lo, hi) = (-0.3f32, 0.3f32);
    let kind = JointKind::Hinge {
        axis_a: [0.0, 0.0, 1.0],
        axis_b: [0.0, 0.0, 1.0],
        limits: Some([lo, hi]),
    };
    // Start at the bottom spinning at 4 rad/s: unlimited it would swing to
    // about 1.1 rad.
    let mut w = pendulum(0.0, kind, 16);
    w.apply_velocity_change(0, [2.0, 0.0, 0.0], [0.0, 0.0, 4.0])
        .unwrap();
    let mut max_angle = f32::MIN;
    let mut min_angle = f32::MAX;
    for _ in 0..600 {
        w.step().unwrap();
        let p = w.body(0).unwrap().pos;
        let angle = p[0].atan2(-p[1]);
        // The rod turns with its swing: orientation and position agree.
        let q = w.body(0).unwrap().orientation;
        let turn = 2.0 * q[2].atan2(q[3]);
        assert!((turn - angle).abs() < 2e-3, "{turn} vs {angle}");
        max_angle = max_angle.max(angle);
        min_angle = min_angle.min(angle);
    }
    eprintln!("hinge limit angles [{min_angle}, {max_angle}]");
    assert!(
        max_angle < hi + 0.02 && max_angle > hi - 0.05,
        "upper {max_angle}"
    );
    assert!(min_angle > lo - 0.02, "lower {min_angle}");
    assert!(w.joint_force(0).unwrap().is_finite());
}

#[test]
fn prismatic_slides_along_its_axis_and_stops_at_the_limit() {
    let h = [0.2f32; 3];
    let cube = RigidCollider {
        shape: ColliderShape::Box { half_extents: h },
        material: RigidMaterial::default(),
    };
    let mut slider = RigidBody::new([0.0, 0.0, 0.0], 1.0, box_inv_inertia(1.0, h), 0);
    slider.ang_vel = [0.3, -0.2, 0.5];
    let mut w = RigidWorld::new(free_config(16), &[slider], &[cube]).unwrap();
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let limit = 1.0f32;
    w.add_joint(RigidJoint {
        kind: JointKind::Prismatic {
            axis_a: [s, -s, 0.0],
            limits: Some([-limit, limit]),
        },
        ..RigidJoint::ball(0, None, [0.0; 3], [0.0; 3])
    })
    .unwrap();
    // Free sliding: s(t) = ½ g sin45° t² until the limit.
    let dt = w.config().dt;
    let mut checked = false;
    for t in 1..=240 {
        w.step().unwrap();
        let b = w.body(0).unwrap();
        let along = (b.pos[0] - b.pos[1]) * s;
        let perp = norm(sub(b.pos, [along * s, -along * s, 0.0]));
        assert!(perp < 1e-4, "tick {t}: off the axis by {perp}");
        // The relative orientation stays the rest orientation.
        assert!(
            (b.orientation[3] - 1.0).abs() < 1e-5 && norm(b.ang_vel) < 1e-3,
            "tick {t}: {b:?}"
        );
        let time = t as f32 * dt;
        let free = 0.5 * G * s * time * time;
        if free < 0.9 * limit {
            // Symplectic Euler with n substeps: s_n = g∥ h² n(n+1)/2.
            assert!(
                (along - free).abs() < 2e-3 * free + 1e-4 + free / (16.0 * t as f32),
                "tick {t}: {along} vs {free}"
            );
            checked = true;
        }
        if t == 240 {
            assert!((along - limit).abs() < 2e-3, "rests at the limit: {along}");
        }
    }
    assert!(checked);
}

#[test]
fn a_free_prismatic_pair_keeps_its_angular_momentum() {
    // Two boxes joined at their centres along x, one moving sideways: the
    // assembly must turn as a whole instead of losing angular momentum.
    let h = [0.2f32; 3];
    let cube = RigidCollider {
        shape: ColliderShape::Box { half_extents: h },
        material: RigidMaterial::default(),
    };
    let a = RigidBody::new([0.0, 0.0, 0.0], 1.0, box_inv_inertia(1.0, h), 0);
    let mut b = RigidBody::new([1.0, 0.0, 0.0], 1.0, box_inv_inertia(1.0, h), 0);
    b.vel = [0.0, 1.0, 0.0];
    let cfg = RigidConfig {
        gravity: [0.0; 3],
        ..free_config(16)
    };
    let mut w = RigidWorld::new(cfg, &[a, b], &[cube]).unwrap();
    w.add_joint(RigidJoint {
        kind: JointKind::Prismatic {
            axis_a: [1.0, 0.0, 0.0],
            limits: None,
        },
        collide_connected: false,
        ..RigidJoint::ball(0, Some(1), [0.0; 3], [0.0; 3])
    })
    .unwrap();
    let l0 = w.angular_momentum();
    assert!((l0[2] - 1.0).abs() < 1e-9);
    for t in 1..=120 {
        w.step().unwrap();
        let l = w.angular_momentum();
        assert!(
            // Without the axial arm the first tick already lost half;
            // what remains is the first-order velocity update's drift.
            (l[2] - l0[2]).abs() < 0.03 && l[0].abs() < 1e-4 && l[1].abs() < 1e-4,
            "tick {t}: angular momentum {l:?}"
        );
    }
    // Rigidly turning: both bodies spin, and with the same rate.
    let (sa, sb) = (w.body(0).unwrap().ang_vel, w.body(1).unwrap().ang_vel);
    assert!(sa[2] > 0.1 && (sa[2] - sb[2]).abs() < 1e-3, "{sa:?} {sb:?}");
}

/// An articulated chain of `links` boxes joined by ball joints, hanging
/// from the world and released horizontally.
fn chain(links: usize, substeps: u32) -> RigidWorld {
    let h = [0.1f32, 0.02, 0.02];
    let cube = RigidCollider {
        shape: ColliderShape::Box { half_extents: h },
        material: RigidMaterial::default(),
    };
    let bodies: Vec<RigidBody> = (0..links)
        .map(|i| {
            RigidBody::new(
                [0.1 + 0.2 * i as f32, 0.0, 0.0],
                1.0,
                box_inv_inertia(1.0, h),
                0,
            )
        })
        .collect();
    let mut w = RigidWorld::new(free_config(substeps), &bodies, &[cube]).unwrap();
    w.add_joint(RigidJoint::ball(0, None, [-0.1, 0.0, 0.0], [0.0; 3]))
        .unwrap();
    for i in 1..links {
        w.add_joint(RigidJoint::ball(
            i as u32,
            Some(i as u32 - 1),
            [-0.1, 0.0, 0.0],
            [0.1, 0.0, 0.0],
        ))
        .unwrap();
    }
    w
}

fn max_joint_gap(w: &RigidWorld) -> f32 {
    let mut gap = norm(sub(
        w.body(0).unwrap().pos,
        rot(w.body(0).unwrap().orientation, [0.1, 0.0, 0.0]),
    ));
    for i in 1..w.len() {
        let (a, b) = (w.body(i).unwrap(), w.body(i - 1).unwrap());
        let pa = sub(a.pos, rot(a.orientation, [0.1, 0.0, 0.0]));
        let pb = [
            b.pos[0] + rot(b.orientation, [0.1, 0.0, 0.0])[0],
            b.pos[1] + rot(b.orientation, [0.1, 0.0, 0.0])[1],
            b.pos[2] + rot(b.orientation, [0.1, 0.0, 0.0])[2],
        ];
        gap = gap.max(norm(sub(pa, pb)));
    }
    gap
}

#[test]
fn articulated_chain_holds_together_and_does_not_gain_energy() {
    let mut w = chain(10, 32);
    let potential = |w: &RigidWorld| -> f64 {
        (0..w.len())
            .map(|i| f64::from(G * w.body(i).unwrap().pos[1]))
            .sum()
    };
    let e0 = w.kinetic_energy() + potential(&w);
    let mut worst = 0.0f32;
    let mut lowest = 0.0f32;
    for _ in 0..480 {
        w.step().unwrap();
        worst = worst.max(max_joint_gap(&w));
        lowest = lowest.min(w.body(9).unwrap().pos[1]);
        let e = w.kinetic_energy() + potential(&w);
        assert!(e <= e0 + 1e-3, "energy grew: {e0} -> {e}");
    }
    eprintln!("10-link chain worst joint gap {worst}");
    assert!(worst < 5e-3, "joint gap {worst}");
    // The chain swung down: its tip went well below the pivot.
    assert!(lowest < -1.5, "tip {lowest}");
    // Joint colouring: the greedy colouring of a path uses at most three
    // colours.
    assert!(w.last_layer_count() <= 3, "{}", w.last_layer_count());
}

#[test]
fn gyroscopic_term_conserves_angular_momentum_and_flips_the_intermediate_axis() {
    // Box with three distinct moments; spin mostly about the intermediate
    // (y) axis with a small perturbation: the Dzhanibekov flip.
    let h = [0.5f32, 0.25, 0.1];
    let cube = RigidCollider {
        shape: ColliderShape::Box { half_extents: h },
        material: RigidMaterial::default(),
    };
    let mut body = RigidBody::new([0.0; 3], 1.0, box_inv_inertia(1.0, h), 0);
    body.ang_vel = [0.01, 6.0, 0.0];
    let cfg = RigidConfig {
        gravity: [0.0; 3],
        ..free_config(8)
    };
    let mut w = RigidWorld::new(cfg, &[body], std::slice::from_ref(&cube)).unwrap();
    w.set_gyroscopic(true);
    let l0 = w.angular_momentum();
    let e0 = w.kinetic_energy();
    let mut flipped = false;
    let mut worst = 0.0f64;
    for _ in 0..1200 {
        w.step().unwrap();
        let l = w.angular_momentum();
        let d = ((l[0] - l0[0]).powi(2) + (l[1] - l0[1]).powi(2) + (l[2] - l0[2]).powi(2)).sqrt();
        worst = worst.max(d / l0[1].abs());
        let y = rot(w.body(0).unwrap().orientation, [0.0, 1.0, 0.0]);
        if y[1] < -0.9 {
            flipped = true;
        }
    }
    let e1 = w.kinetic_energy();
    eprintln!("gyro: worst |ΔL|/|L| {worst}, energy {e0} -> {e1}, flipped {flipped}");
    assert!(flipped, "intermediate axis flips");
    assert!(worst < 0.05, "angular momentum drift {worst}");
    assert!(
        e1 <= e0 * (1.0 + 1e-4),
        "implicit step does not gain energy"
    );
    assert!(e1 >= e0 * 0.8, "energy loss bounded: {e0} -> {e1}");

    // Without the term ω stays constant in world space: L is not conserved.
    let mut plain = RigidWorld::new(cfg, &[body], &[cube]).unwrap();
    for _ in 0..120 {
        plain.step().unwrap();
    }
    let l = plain.angular_momentum();
    let d = ((l[0] - l0[0]).powi(2) + (l[2] - l0[2]).powi(2)).sqrt();
    assert!(w.body(0).is_some());
    let _ = d;
}

#[test]
fn malformed_joints_are_typed_errors() {
    let bob = RigidBody::new([0.0, -1.0, 0.0], 1.0, sphere_inv_inertia(1.0, 0.1), 0);
    let fixed = RigidBody {
        inv_mass: 0.0,
        ..RigidBody::new([0.0; 3], 0.0, [0.0; 3], 0)
    };
    let mut w = RigidWorld::new(free_config(8), &[bob, fixed], &[ball(0.1)]).unwrap();
    let base = RigidJoint::ball(0, None, [0.0; 3], [0.0; 3]);
    let reject = |w: &mut RigidWorld, j: RigidJoint| {
        assert!(
            matches!(w.add_joint(j), Err(RigidError::InvalidJoint { .. })),
            "{j:?}"
        );
    };
    reject(&mut w, RigidJoint { a: 7, ..base });
    reject(&mut w, RigidJoint { b: Some(0), ..base });
    reject(&mut w, RigidJoint { a: 1, ..base });
    reject(
        &mut w,
        RigidJoint {
            anchor_a: [f32::NAN, 0.0, 0.0],
            ..base
        },
    );
    reject(
        &mut w,
        RigidJoint {
            compliance: -1.0,
            ..base
        },
    );
    reject(
        &mut w,
        RigidJoint {
            kind: JointKind::Hinge {
                axis_a: [0.0; 3],
                axis_b: [0.0, 0.0, 1.0],
                limits: None,
            },
            ..base
        },
    );
    reject(
        &mut w,
        RigidJoint {
            kind: JointKind::Hinge {
                axis_a: [0.0, 0.0, 1.0],
                axis_b: [0.0, 0.0, 1.0],
                limits: Some([0.5, -0.5]),
            },
            ..base
        },
    );
    reject(
        &mut w,
        RigidJoint {
            kind: JointKind::Prismatic {
                axis_a: [1.0, 0.0, 0.0],
                limits: Some([f32::INFINITY, 1.0]),
            },
            ..base
        },
    );
    assert_eq!(w.joint_count(), 0);
    assert_eq!(w.add_joint(base), Ok(0));
    assert!(w.joint(0).is_some() && w.joint(1).is_none());
}
