//! Featherstone articulations: the articulated-body algorithm against
//! closed-form dynamics (double pendulum, cart-pole), pendulum periods
//! against the exact elliptic-integral period, energy conservation, drives,
//! joint friction, determinism and typed errors.

use vesper_kinetic::articulation::{
    Articulation, ArticulationConfig, ArticulationDrive, ArticulationError, ArticulationJoint,
    ArticulationLink, rod_link_inertia,
};

const G: f64 = 9.81;
const Z: [f64; 3] = [0.0, 0.0, 1.0];
const ZERO3: [[f64; 3]; 3] = [[0.0; 3]; 3];

fn config(dt: f64, substeps: u32) -> ArticulationConfig {
    ArticulationConfig {
        dt,
        substeps,
        gravity: [0.0, -G, 0.0],
    }
}

/// Point masses `m1` at `l1` below the world pivot and `m2` at `l2` below
/// link 1's tip, both on hinges about z.
fn double_pendulum(m1: f64, l1: f64, m2: f64, l2: f64) -> Vec<ArticulationLink> {
    let rev = ArticulationJoint::Revolute { axis: Z };
    vec![
        ArticulationLink::new(None, rev, [0.0; 3], m1, [0.0, -l1, 0.0], ZERO3),
        ArticulationLink::new(Some(0), rev, [0.0, -l1, 0.0], m2, [0.0, -l2, 0.0], ZERO3),
    ]
}

/// The textbook double-pendulum equations in absolute angles from the
/// downward vertical.
fn double_pendulum_closed_form(
    (m1, l1, m2, l2): (f64, f64, f64, f64),
    (t1, t2, w1, w2): (f64, f64, f64, f64),
) -> (f64, f64) {
    let den = 2.0 * m1 + m2 - m2 * (2.0 * t1 - 2.0 * t2).cos();
    let a1 = (-G * (2.0 * m1 + m2) * t1.sin()
        - m2 * G * (t1 - 2.0 * t2).sin()
        - 2.0 * (t1 - t2).sin() * m2 * (w2 * w2 * l2 + w1 * w1 * l1 * (t1 - t2).cos()))
        / (l1 * den);
    let a2 = (2.0
        * (t1 - t2).sin()
        * (w1 * w1 * l1 * (m1 + m2)
            + G * (m1 + m2) * t1.cos()
            + w2 * w2 * l2 * m2 * (t1 - t2).cos()))
        / (l2 * den);
    (a1, a2)
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

#[test]
fn aba_matches_closed_form_double_pendulum() {
    let mut rng = Lcg(7);
    for _ in 0..200 {
        let p = (
            rng.range(0.2, 3.0),
            rng.range(0.3, 2.0),
            rng.range(0.2, 3.0),
            rng.range(0.3, 2.0),
        );
        let mut art = Articulation::new(config(0.01, 1), &double_pendulum(p.0, p.1, p.2, p.3))
            .expect("valid");
        let (q1, q2) = (rng.range(-3.0, 3.0), rng.range(-3.0, 3.0));
        let (u1, u2) = (rng.range(-4.0, 4.0), rng.range(-4.0, 4.0));
        art.set_state(0, q1, u1).unwrap();
        art.set_state(1, q2, u2).unwrap();
        let qdd = art.forward_dynamics().unwrap().to_vec();
        // Absolute angle of link 2 is q1 + q2.
        let (a1, a2) = double_pendulum_closed_form(p, (q1, q1 + q2, u1, u1 + u2));
        let scale = 1.0 + a1.abs() + a2.abs();
        assert!((qdd[0] - a1).abs() < 1e-9 * scale, "{} vs {a1}", qdd[0]);
        assert!((qdd[0] + qdd[1] - a2).abs() < 1e-9 * scale);
    }
}

#[test]
fn aba_matches_closed_form_cart_pole() {
    // Cart of mass M on a prismatic x joint, pole of point mass m at length
    // l on a hinge about z. Lagrange:
    //   (M+m) ẍ + m l cosθ θ̈ − m l sinθ θ̇² = F
    //   m l cosθ ẍ + m l² θ̈ + m g l sinθ = τ
    let mut rng = Lcg(11);
    for _ in 0..200 {
        let (big_m, m, l) = (
            rng.range(0.5, 5.0),
            rng.range(0.1, 2.0),
            rng.range(0.2, 2.0),
        );
        let links = [
            ArticulationLink::new(
                None,
                ArticulationJoint::Prismatic {
                    axis: [1.0, 0.0, 0.0],
                },
                [0.0; 3],
                big_m,
                [0.0; 3],
                ZERO3,
            ),
            ArticulationLink::new(
                Some(0),
                ArticulationJoint::Revolute { axis: Z },
                [0.0; 3],
                m,
                [0.0, -l, 0.0],
                ZERO3,
            ),
        ];
        let mut art = Articulation::new(config(0.01, 1), &links).unwrap();
        let (x, th, xd, thd) = (
            rng.range(-1.0, 1.0),
            rng.range(-3.0, 3.0),
            rng.range(-2.0, 2.0),
            rng.range(-4.0, 4.0),
        );
        let (force, torque) = (rng.range(-5.0, 5.0), rng.range(-2.0, 2.0));
        art.set_state(0, x, xd).unwrap();
        art.set_state(1, th, thd).unwrap();
        art.set_joint_torque(0, force).unwrap();
        art.set_joint_torque(1, torque).unwrap();
        let qdd = art.forward_dynamics().unwrap().to_vec();
        let (s, c) = (th.sin(), th.cos());
        // [[M+m, m l c], [m l c, m l²]] [ẍ, θ̈] = [F + m l s θ̇², τ − m g l s]
        let (a, b, d) = (big_m + m, m * l * c, m * l * l);
        let (r1, r2) = (force + m * l * s * thd * thd, torque - m * G * l * s);
        let det = a * d - b * b;
        let xdd = (r1 * d - b * r2) / det;
        let thdd = (a * r2 - b * r1) / det;
        let scale = 1.0 + xdd.abs() + thdd.abs();
        assert!((qdd[0] - xdd).abs() < 1e-9 * scale, "{} vs {xdd}", qdd[0]);
        assert!((qdd[1] - thdd).abs() < 1e-9 * scale, "{} vs {thdd}", qdd[1]);
    }
}

/// Complete elliptic integral of the first kind by the arithmetic-geometric
/// mean.
fn ellip_k(k: f64) -> f64 {
    let (mut a, mut b) = (1.0f64, (1.0 - k * k).sqrt());
    for _ in 0..40 {
        let (na, nb) = (0.5 * (a + b), (a * b).sqrt());
        a = na;
        b = nb;
    }
    std::f64::consts::PI / (2.0 * a)
}

fn rod_pendulum(length: f64, mass: f64, theta0: f64) -> Articulation {
    let (com, inertia) = rod_link_inertia(mass, length, [0.0, -1.0, 0.0]);
    let mut link = ArticulationLink::new(
        None,
        ArticulationJoint::Revolute { axis: Z },
        [0.0; 3],
        mass,
        com,
        inertia,
    );
    link.q = theta0;
    Articulation::new(config(1.0 / 240.0, 8), &[link]).unwrap()
}

/// Period from successive downward zero crossings of `q`, interpolated.
fn measure_period(art: &mut Articulation, ticks: usize) -> f64 {
    let dt = art.config().dt;
    let mut crossings = Vec::new();
    let mut prev = art.q()[0];
    for t in 0..ticks {
        art.step().unwrap();
        let q = art.q()[0];
        if prev > 0.0 && q <= 0.0 {
            let frac = prev / (prev - q);
            crossings.push((t as f64 + frac) * dt);
        }
        prev = q;
    }
    assert!(crossings.len() >= 3, "too few crossings");
    (crossings[crossings.len() - 1] - crossings[0]) / (crossings.len() - 1) as f64
}

#[test]
fn compound_pendulum_period_matches_analytic() {
    // Uniform rod pivoted at one end: small-angle T = 2π √(2L / 3g); at
    // amplitude θ₀ the exact period is 4 √(2L/3g) K(sin(θ₀/2)).
    let length = 1.2;
    let l_eff = 2.0 * length / 3.0;
    for &theta0 in &[0.05f64, 1.0, 2.5] {
        let mut art = rod_pendulum(length, 2.0, theta0);
        let measured = measure_period(&mut art, 240 * 12);
        let exact = 4.0 * (l_eff / G).sqrt() * ellip_k((theta0 / 2.0).sin());
        let rel = (measured - exact).abs() / exact;
        assert!(rel < 2e-4, "θ0 {theta0}: {measured} vs {exact} ({rel})");
    }
    // And the small-angle formula at 0.05 rad.
    let mut art = rod_pendulum(length, 2.0, 0.05);
    let small = 2.0 * std::f64::consts::PI * (l_eff / G).sqrt();
    let measured = measure_period(&mut art, 240 * 12);
    assert!((measured - small).abs() / small < 5e-4);
}

#[test]
fn double_pendulum_conserves_energy() {
    // Semi-implicit Euler in joint coordinates is first order (the mass
    // matrix depends on q, so it is not symplectic here): the worst energy
    // error over 2 s of chaotic motion is small and halves with the step.
    let drift = |substeps: u32| {
        let mut art = Articulation::new(
            config(1.0 / 240.0, substeps),
            &double_pendulum(1.0, 1.0, 0.7, 0.8),
        )
        .unwrap();
        art.set_state(0, 2.0, 0.0).unwrap();
        art.set_state(1, -1.0, 0.0).unwrap();
        let (k0, p0) = art.energy();
        let e0 = k0 + p0;
        let mut worst = 0.0f64;
        for _ in 0..240 * 2 {
            art.step().unwrap();
            let (k, p) = art.energy();
            worst = worst.max((k + p - e0).abs());
        }
        worst
    };
    let scale = 1.7 * G * 1.8;
    let (coarse, fine) = (drift(16), drift(32));
    eprintln!("energy drift: h = 1/3840 s: {coarse:.3e} J, h = 1/7680 s: {fine:.3e} J");
    assert!(coarse / scale < 1e-2, "energy drift {coarse}");
    let ratio = coarse / fine;
    assert!(
        (1.6..2.5).contains(&ratio),
        "first-order convergence: ratio {ratio}"
    );
}

fn disk_wheel(mass: f64, radius: f64) -> ArticulationLink {
    // A disk spinning about the vertical axis through its centre: gravity
    // has no torque about the joint.
    let i = 0.5 * mass * radius * radius;
    let side = 0.25 * mass * radius * radius;
    ArticulationLink::new(
        None,
        ArticulationJoint::Revolute {
            axis: [0.0, 1.0, 0.0],
        },
        [0.0; 3],
        mass,
        [0.0; 3],
        [[side, 0.0, 0.0], [0.0, i, 0.0], [0.0, 0.0, side]],
    )
}

#[test]
fn velocity_motor_reaches_target_and_is_stable_at_huge_gain() {
    for &gain in &[10.0f64, 1e3, 1e9] {
        let mut wheel = disk_wheel(2.0, 0.5);
        wheel.drive = Some(ArticulationDrive {
            target_position: 0.0,
            target_velocity: 5.0,
            stiffness: 0.0,
            damping: gain,
            max_effort: f64::INFINITY,
        });
        let mut art = Articulation::new(config(1.0 / 60.0, 4), &[wheel]).unwrap();
        for _ in 0..600 {
            art.step().unwrap();
        }
        let w = art.qd()[0];
        assert!((w - 5.0).abs() < 1e-6, "gain {gain}: ω = {w}");
    }
}

#[test]
fn saturated_motor_accelerates_at_max_effort_over_inertia() {
    let (mass, radius, tau) = (2.0, 0.5, 0.3);
    let inertia = 0.5 * mass * radius * radius;
    let mut wheel = disk_wheel(mass, radius);
    wheel.drive = Some(ArticulationDrive {
        target_position: 0.0,
        target_velocity: 1000.0,
        stiffness: 0.0,
        damping: 1e6,
        max_effort: tau,
    });
    let mut art = Articulation::new(config(0.01, 1), &[wheel]).unwrap();
    for _ in 0..100 {
        art.step().unwrap();
    }
    let expected = tau / inertia * 1.0;
    assert!((art.qd()[0] - expected).abs() < 1e-9, "{}", art.qd()[0]);
}

#[test]
fn position_servo_holds_pendulum_at_target() {
    let (length, mass) = (1.0, 1.5);
    let (com, inertia) = rod_link_inertia(mass, length, [0.0, -1.0, 0.0]);
    let mut link = ArticulationLink::new(
        None,
        ArticulationJoint::Revolute { axis: Z },
        [0.0; 3],
        mass,
        com,
        inertia,
    );
    let (k, target) = (400.0, 0.6f64);
    link.drive = Some(ArticulationDrive {
        target_position: target,
        target_velocity: 0.0,
        stiffness: k,
        damping: 40.0,
        max_effort: f64::INFINITY,
    });
    let mut art = Articulation::new(config(1.0 / 120.0, 4), &[link]).unwrap();
    for _ in 0..1200 {
        art.step().unwrap();
    }
    // Static equilibrium: k (target − q) = m g (L/2) sin q.
    let q = art.q()[0];
    let residual = k * (target - q) - mass * G * 0.5 * length * q.sin();
    assert!(residual.abs() < 1e-6, "q {q}, residual {residual}");
    assert!(art.qd()[0].abs() < 1e-9);
}

#[test]
fn coulomb_friction_decelerates_at_friction_over_inertia_then_sticks() {
    let (mass, radius, f, w0) = (2.0, 0.5, 0.1, 3.0);
    let inertia = 0.5 * mass * radius * radius;
    let mut wheel = disk_wheel(mass, radius);
    wheel.friction = f;
    wheel.qd = w0;
    let dt = 0.01;
    let mut art = Articulation::new(config(dt, 1), &[wheel]).unwrap();
    // Before stopping: ω(t) = ω0 − (f / I) t.
    for _ in 0..50 {
        art.step().unwrap();
    }
    let expected = w0 - f / inertia * 0.5;
    assert!((art.qd()[0] - expected).abs() < 1e-9, "{}", art.qd()[0]);
    // It stops at t = ω0 I / f = 7.5 s and stays stopped.
    for _ in 0..800 {
        art.step().unwrap();
    }
    assert!(art.qd()[0].abs() < 1e-12, "{}", art.qd()[0]);
    let q_stop = art.q()[0];
    let stop_time = w0 * inertia / f;
    let travelled = w0 * stop_time - 0.5 * f / inertia * stop_time * stop_time;
    assert!((q_stop - travelled).abs() < 0.02, "{q_stop} vs {travelled}");
    for _ in 0..100 {
        art.step().unwrap();
    }
    assert!((art.q()[0] - q_stop).abs() < 1e-12);
}

#[test]
fn friction_above_gravity_torque_holds_pendulum() {
    let (length, mass) = (1.0, 1.0);
    let (com, inertia) = rod_link_inertia(mass, length, [0.0, -1.0, 0.0]);
    let mut link = ArticulationLink::new(
        None,
        ArticulationJoint::Revolute { axis: Z },
        [0.0; 3],
        mass,
        com,
        inertia,
    );
    link.q = 0.8;
    let gravity_torque = mass * G * 0.5 * length * 0.8f64.sin();
    link.friction = 1.01 * gravity_torque;
    let mut art = Articulation::new(config(1.0 / 120.0, 4), &[link]).unwrap();
    for _ in 0..240 {
        art.step().unwrap();
    }
    assert!((art.q()[0] - 0.8).abs() < 1e-12, "{}", art.q()[0]);
    // Below the gravity torque it slides.
    link.friction = 0.9 * gravity_torque;
    let mut art = Articulation::new(config(1.0 / 120.0, 4), &[link]).unwrap();
    for _ in 0..60 {
        art.step().unwrap();
    }
    assert!(art.q()[0] < 0.79);
}

fn chain(n: usize) -> Articulation {
    let mut links = Vec::new();
    for i in 0..n {
        let axis = if i % 2 == 0 { Z } else { [1.0, 0.0, 0.0] };
        let (com, inertia) = rod_link_inertia(0.5 + 0.1 * i as f64, 0.4, [0.0, -1.0, 0.0]);
        let mut l = ArticulationLink::new(
            i.checked_sub(1),
            ArticulationJoint::Revolute { axis },
            if i == 0 { [0.0; 3] } else { [0.0, -0.4, 0.0] },
            0.5 + 0.1 * i as f64,
            com,
            inertia,
        );
        l.q = 0.3 * ((i as f64) * 0.7).sin() + 0.2;
        l.damping = 0.01;
        links.push(l);
    }
    Articulation::new(config(1.0 / 120.0, 8), &links).unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Pinned on Linux x86_64; CI checks the same bits on macOS AArch64 and
/// Windows x86_64.
const CHAIN_GOLDEN: &str = "a25cf9cd1b37b150c431912583ed2e2c18b700e575d0f91625f722b429edc416";

#[test]
fn twelve_link_chain_is_bit_deterministic() {
    let mut a = chain(12);
    let mut b = chain(12);
    let mut ha = [0u8; 32];
    for _ in 0..600 {
        ha = a.step().unwrap();
        assert_eq!(ha, b.step().unwrap());
    }
    // The chain does not blow up and keeps its energy bounded (damped).
    let (k, _) = a.energy();
    assert!(k.is_finite());
    assert_eq!(hex(&ha), CHAIN_GOLDEN);
}

#[test]
fn articulation_errors_are_typed() {
    let rev = ArticulationJoint::Revolute { axis: Z };
    let good = ArticulationLink::new(None, rev, [0.0; 3], 1.0, [0.0, -1.0, 0.0], ZERO3);
    let check =
        |links: &[ArticulationLink], want: &str| match Articulation::new(config(0.01, 1), links) {
            Err(ArticulationError::InvalidLink { reason, .. }) => {
                assert!(reason.contains(want), "{reason}")
            }
            other => panic!("expected InvalidLink({want}), got {other:?}"),
        };
    check(
        &[ArticulationLink {
            parent: Some(0),
            ..good
        }],
        "earlier",
    );
    check(
        &[ArticulationLink {
            joint: ArticulationJoint::Revolute { axis: [0.0; 3] },
            ..good
        }],
        "axis",
    );
    check(&[ArticulationLink { mass: 0.0, ..good }], "mass");
    check(
        &[ArticulationLink {
            com: [f64::NAN, 0.0, 0.0],
            ..good
        }],
        "finite",
    );
    check(
        &[ArticulationLink {
            inertia: [[1.0, 0.5, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            ..good
        }],
        "symmetric",
    );
    check(
        &[ArticulationLink {
            friction: -1.0,
            ..good
        }],
        "friction",
    );
    check(
        &[ArticulationLink {
            drive: Some(ArticulationDrive {
                target_position: 0.0,
                target_velocity: 0.0,
                stiffness: 1.0,
                damping: 1.0,
                max_effort: 0.0,
            }),
            ..good
        }],
        "drive",
    );
    assert!(matches!(
        Articulation::new(config(0.0, 1), &[good]),
        Err(ArticulationError::InvalidConfig(_))
    ));
    // A point mass on its own hinge axis has no inertia about the joint.
    let on_axis = ArticulationLink::new(None, rev, [0.0; 3], 1.0, [0.0; 3], ZERO3);
    let mut art = Articulation::new(config(0.01, 1), &[on_axis]).unwrap();
    let hash = art.hash();
    assert_eq!(art.step(), Err(ArticulationError::Singular { link: 0 }));
    assert_eq!(art.hash(), hash, "a failed step leaves the state unchanged");
    assert!(matches!(
        art.set_state(3, 0.0, 0.0),
        Err(ArticulationError::UnknownLink { link: 3 })
    ));
}

#[test]
fn implicit_drive_effort_stays_within_max_effort() {
    // At its target but moving fast: the explicit PD term is 0, the
    // implicit spring is not. The drive may change the rate by at most
    // `max_effort / m · h`.
    let links = [ArticulationLink::new(
        None,
        ArticulationJoint::Prismatic {
            axis: [1.0, 0.0, 0.0],
        },
        [0.0; 3],
        1.0,
        [0.0; 3],
        ZERO3,
    )];
    let mut art = Articulation::new(config(0.1, 1), &links).unwrap();
    art.set_state(0, 0.0, 100.0).unwrap();
    art.set_drive(
        0,
        Some(ArticulationDrive {
            target_position: 0.0,
            target_velocity: 0.0,
            stiffness: 100.0,
            damping: 0.0,
            max_effort: 1.0,
        }),
    )
    .unwrap();
    art.step().unwrap();
    let qd = art.qd()[0];
    assert!((qd - (100.0 - 0.1)).abs() < 1e-9, "rate {qd}");
}
