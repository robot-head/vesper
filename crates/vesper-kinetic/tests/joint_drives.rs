//! XPBD joint motors, servos and Coulomb joint friction on hinges and
//! prismatic joints of `RigidWorld`, against the analytic rates, holding
//! torques and accelerations.

use vesper_kinetic::rigid::{
    ColliderShape, JointDrive, JointKind, RigidBody, RigidCollider, RigidConfig, RigidError,
    RigidJoint, RigidMaterial, RigidWorld, box_inv_inertia,
};

const G: f32 = 9.81;

fn config(substeps: u32) -> RigidConfig {
    RigidConfig {
        ground: None,
        substeps,
        ..RigidConfig::default()
    }
}

fn cube(h: [f32; 3]) -> RigidCollider {
    RigidCollider {
        shape: ColliderShape::Box { half_extents: h },
        material: RigidMaterial::default(),
    }
}

/// A box hinged to the world at its centre about z: gravity has no torque.
fn wheel() -> (RigidWorld, f32) {
    let h = [0.4, 0.4, 0.1];
    let inv_i = box_inv_inertia(2.0, h);
    let body = RigidBody::new([0.0, 1.0, 0.0], 0.5, inv_i, 0);
    let mut w = RigidWorld::new(config(8), &[body], &[cube(h)]).unwrap();
    w.add_joint(RigidJoint {
        kind: JointKind::Hinge {
            axis_a: [0.0, 0.0, 1.0],
            axis_b: [0.0, 0.0, 1.0],
            limits: None,
        },
        ..RigidJoint::ball(0, None, [0.0; 3], [0.0, 1.0, 0.0])
    })
    .unwrap();
    (w, 1.0 / inv_i[2])
}

#[test]
fn hinge_motor_reaches_its_target_rate() {
    let (mut w, _) = wheel();
    w.set_joint_drive(
        0,
        Some(JointDrive::Velocity {
            target: 4.0,
            max_force: f32::INFINITY,
        }),
    )
    .unwrap();
    for _ in 0..60 {
        w.step().unwrap();
    }
    let rate = w.joint_rate(0).unwrap();
    assert!((rate - 4.0).abs() < 1e-3, "rate {rate}");
    // The coordinate is the world relative to the body, so the body spins
    // the other way.
    let b = w.body(0).unwrap();
    assert!((b.ang_vel[2] + 4.0).abs() < 1e-3, "{:?}", b.ang_vel);
    assert!(b.ang_vel[0].abs() < 1e-4 && b.ang_vel[1].abs() < 1e-4);
}

#[test]
fn saturated_hinge_motor_accelerates_at_max_torque_over_inertia() {
    let (mut w, inertia) = wheel();
    let tau = 0.05f32;
    w.set_joint_drive(
        0,
        Some(JointDrive::Velocity {
            target: 100.0,
            max_force: tau,
        }),
    )
    .unwrap();
    let ticks = 120;
    for _ in 0..ticks {
        w.step().unwrap();
    }
    let t = ticks as f32 * w.config().dt;
    let expected = tau / inertia * t;
    let rate = w.joint_rate(0).unwrap();
    assert!(
        (rate - expected).abs() < 1e-3 * expected,
        "{rate} vs {expected}"
    );
}

const ROD: [f32; 3] = [0.05, 0.5, 0.05];

/// A rod hanging by its top end from a world hinge about z at the origin.
fn hinged_rod(theta0: f32) -> RigidWorld {
    let d = ROD[1];
    let mut body = RigidBody::new(
        [d * theta0.sin(), -d * theta0.cos(), 0.0],
        1.0,
        box_inv_inertia(1.0, ROD),
        0,
    );
    body.orientation = [0.0, 0.0, (theta0 * 0.5).sin(), (theta0 * 0.5).cos()];
    let mut w = RigidWorld::new(config(16), &[body], &[cube(ROD)]).unwrap();
    // Creating the hinge at the displaced pose makes that pose angle 0;
    // build it at rest instead so the coordinate is −θ (world relative to
    // the body).
    w.add_joint(RigidJoint {
        kind: JointKind::Hinge {
            axis_a: [0.0, 0.0, 1.0],
            axis_b: [0.0, 0.0, 1.0],
            limits: None,
        },
        ..RigidJoint::ball(0, None, [0.0, d, 0.0], [0.0; 3])
    })
    .unwrap();
    w
}

#[test]
fn hinge_servo_holds_the_target_angle_against_gravity() {
    let mut w = hinged_rod(0.0);
    let target = -0.5f32; // the rod swings +0.5 rad about z
    w.set_joint_drive(
        0,
        Some(JointDrive::Position {
            target,
            compliance: 0.0,
            damping: 0.0,
            max_force: f32::INFINITY,
        }),
    )
    .unwrap();
    for _ in 0..240 {
        w.step().unwrap();
    }
    let phi = w.joint_coordinate(0).unwrap();
    assert!((phi - target).abs() < 2e-3, "angle {phi}");
    assert!(w.joint_rate(0).unwrap().abs() < 1e-2);
    let b = w.body(0).unwrap();
    assert!((b.pos[0] - 0.5 * 0.5f32.sin()).abs() < 2e-3, "{:?}", b.pos);
}

#[test]
fn hinge_servo_holds_only_up_to_its_torque() {
    // Holding the rod at 0.5 rad needs m g d sin 0.5 ≈ 2.35 N·m. The hinge
    // is built at that pose, so the servo's target 0 is "stay here".
    let hold = 1.0 * G * 0.5 * 0.5f32.sin();
    for (factor, holds) in [(1.5f32, true), (0.5, false)] {
        let mut w = hinged_rod(0.5);
        w.set_joint_drive(
            0,
            Some(JointDrive::Position {
                target: 0.0,
                compliance: 0.0,
                damping: 0.0,
                max_force: factor * hold,
            }),
        )
        .unwrap();
        for _ in 0..60 {
            w.step().unwrap();
        }
        let b = w.body(0).unwrap();
        let angle = b.pos[0].atan2(-b.pos[1]);
        if holds {
            assert!((angle - 0.5).abs() < 2e-3, "held at {angle}");
        } else {
            assert!(angle < 0.4, "a half-strength servo held at {angle}");
        }
    }
}

#[test]
fn hinge_friction_holds_below_and_slips_above_the_gravity_torque() {
    let theta = 0.6f32;
    let torque = 1.0 * G * 0.5 * theta.sin();
    for (factor, holds) in [(1.2f32, true), (0.6, false)] {
        let mut w = hinged_rod(theta);
        w.set_joint_friction(0, factor * torque).unwrap();
        for _ in 0..120 {
            w.step().unwrap();
        }
        let b = w.body(0).unwrap();
        let angle = b.pos[0].atan2(-b.pos[1]);
        if holds {
            // Residual creep of the positional hinge friction.
            assert!((angle - theta).abs() < 5e-3, "held at {angle}");
        } else {
            assert!(angle < theta - 0.1, "slipped to {angle}");
        }
    }
}

/// A cube on a world prismatic joint down a 45° incline in the x-y plane.
fn incline_slider() -> RigidWorld {
    let h = [0.2f32; 3];
    let slider = RigidBody::new([0.0; 3], 1.0, box_inv_inertia(1.0, h), 0);
    let mut w = RigidWorld::new(config(16), &[slider], &[cube(h)]).unwrap();
    let s = std::f32::consts::FRAC_1_SQRT_2;
    w.add_joint(RigidJoint {
        kind: JointKind::Prismatic {
            axis_a: [s, -s, 0.0],
            limits: None,
        },
        ..RigidJoint::ball(0, None, [0.0; 3], [0.0; 3])
    })
    .unwrap();
    w
}

#[test]
fn prismatic_friction_on_an_incline() {
    let along_g = G * std::f32::consts::FRAC_1_SQRT_2;
    // Friction above m g sin 45° holds the cube.
    let mut w = incline_slider();
    w.set_joint_friction(0, 1.1 * along_g).unwrap();
    for _ in 0..120 {
        w.step().unwrap();
    }
    let p = w.body(0).unwrap().pos;
    assert!(p[0].abs() < 1e-4 && p[1].abs() < 1e-4, "{p:?}");
    // Below it the cube slides with a = g sin 45° − f / m.
    let f = 0.5 * along_g;
    let mut w = incline_slider();
    w.set_joint_friction(0, f).unwrap();
    let ticks = 60;
    for _ in 0..ticks {
        w.step().unwrap();
    }
    let t = ticks as f32 * w.config().dt;
    let speed = -w.joint_rate(0).unwrap();
    let expected = (along_g - f) * t;
    assert!(
        (speed - expected).abs() < 2e-3 * expected,
        "{speed} vs {expected}"
    );
}

#[test]
fn prismatic_motor_and_servo() {
    // Motor: the coordinate rate goes to the target against gravity.
    let mut w = incline_slider();
    w.set_joint_drive(
        0,
        Some(JointDrive::Velocity {
            target: 1.5,
            max_force: 100.0,
        }),
    )
    .unwrap();
    for _ in 0..60 {
        w.step().unwrap();
    }
    let rate = w.joint_rate(0).unwrap();
    assert!((rate - 1.5).abs() < 1e-3, "rate {rate}");
    // Servo: the coordinate settles at the target.
    let mut w = incline_slider();
    w.set_joint_drive(
        0,
        Some(JointDrive::Position {
            target: -0.3,
            compliance: 0.0,
            damping: 20.0,
            max_force: 100.0,
        }),
    )
    .unwrap();
    for _ in 0..360 {
        w.step().unwrap();
    }
    let c = w.joint_coordinate(0).unwrap();
    assert!((c + 0.3).abs() < 1e-3, "coordinate {c}");
    assert!(w.joint_rate(0).unwrap().abs() < 1e-2);
}

#[test]
fn two_body_hinge_motor_conserves_angular_momentum() {
    // A motor between two free bodies is an internal torque.
    let h = [0.3, 0.3, 0.1];
    let mut a = RigidBody::new([0.0; 3], 1.0, box_inv_inertia(1.0, h), 0);
    a.ang_vel = [0.0, 0.0, 0.5];
    let b = RigidBody::new([0.0, 0.0, 0.5], 0.5, box_inv_inertia(2.0, h), 0);
    let zero_g = RigidConfig {
        gravity: [0.0; 3],
        ..config(8)
    };
    let mut w = RigidWorld::new(zero_g, &[a, b], &[cube(h)]).unwrap();
    w.add_joint(RigidJoint {
        kind: JointKind::Hinge {
            axis_a: [0.0, 0.0, 1.0],
            axis_b: [0.0, 0.0, 1.0],
            limits: None,
        },
        ..RigidJoint::ball(0, Some(1), [0.0, 0.0, 0.25], [0.0, 0.0, -0.25])
    })
    .unwrap();
    w.set_joint_drive(
        0,
        Some(JointDrive::Velocity {
            target: 3.0,
            max_force: 1.0,
        }),
    )
    .unwrap();
    let l0 = w.angular_momentum();
    for _ in 0..120 {
        w.step().unwrap();
    }
    let l1 = w.angular_momentum();
    for k in 0..3 {
        assert!((l1[k] - l0[k]).abs() < 1e-3, "{l0:?} -> {l1:?}");
    }
    assert!((w.joint_rate(0).unwrap() - 3.0).abs() < 1e-2);
}

#[test]
fn drive_and_friction_errors_are_typed() {
    let (mut w, _) = wheel();
    let bad = |e: Result<(), RigidError>| {
        assert!(matches!(e, Err(RigidError::InvalidJoint { .. })), "{e:?}");
    };
    bad(w.set_joint_drive(
        3,
        Some(JointDrive::Velocity {
            target: 1.0,
            max_force: 1.0,
        }),
    ));
    bad(w.set_joint_drive(
        0,
        Some(JointDrive::Velocity {
            target: f32::NAN,
            max_force: 1.0,
        }),
    ));
    bad(w.set_joint_drive(
        0,
        Some(JointDrive::Position {
            target: 0.0,
            compliance: -1.0,
            damping: 0.0,
            max_force: 1.0,
        }),
    ));
    bad(w.set_joint_drive(
        0,
        Some(JointDrive::Velocity {
            target: 0.0,
            max_force: 0.0,
        }),
    ));
    bad(w.set_joint_friction(0, -1.0));
    bad(w.set_joint_friction(0, f32::INFINITY));
    // Ball joints have no free coordinate to drive.
    let h = [0.2f32; 3];
    let body = RigidBody::new([0.0; 3], 1.0, box_inv_inertia(1.0, h), 0);
    let mut w = RigidWorld::new(config(4), &[body], &[cube(h)]).unwrap();
    w.add_joint(RigidJoint::ball(0, None, [0.0; 3], [0.0; 3]))
        .unwrap();
    bad(w.set_joint_drive(
        0,
        Some(JointDrive::Velocity {
            target: 1.0,
            max_force: 1.0,
        }),
    ));
    bad(w.set_joint_friction(0, 1.0));
    assert_eq!(w.joint_coordinate(0), None);
    // Clearing is always allowed.
    w.set_joint_drive(0, None).unwrap();
    w.set_joint_friction(0, 0.0).unwrap();
}
