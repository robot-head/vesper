//! Contact gradients for rigid boxes, capsules (any orientation) and convex
//! hulls, checked against central finite differences.

use vesper_kinetic::rigid_diff::{
    DiffRigidBody, DiffRigidScene, DiffRigidState, DiffShape, RigidDiffError, gradients, simulate,
};

fn quat(axis: [f64; 3], angle: f64) -> [f64; 4] {
    let n = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    let s = (angle * 0.5).sin() / n;
    [axis[0] * s, axis[1] * s, axis[2] * s, (angle * 0.5).cos()]
}

fn body(pos: [f64; 3], shape: usize, inv_inertia: [f64; 3]) -> DiffRigidBody {
    DiffRigidBody {
        pos,
        orientation: [0.0, 0.0, 0.0, 1.0],
        vel: [0.0; 3],
        ang_vel: [0.0; 3],
        inv_mass: 1.0,
        inv_inertia,
        shape,
    }
}

fn fixed(pos: [f64; 3], shape: usize) -> DiffRigidBody {
    DiffRigidBody {
        inv_mass: 0.0,
        inv_inertia: [0.0; 3],
        ..body(pos, shape, [0.0; 3])
    }
}

fn scene(bodies: Vec<DiffRigidBody>, shapes: Vec<DiffShape>, ticks: u32) -> DiffRigidScene {
    DiffRigidScene {
        bodies,
        shapes,
        gravity: [0.0, -9.81, 0.0],
        dt: 1.0 / 60.0,
        substeps: 8,
        ticks,
        ground: Some(0.0),
        compliance: 1e-5,
        margin: 0.05,
    }
}

/// A seed that weights every component differently.
fn seeds(n: usize) -> Vec<DiffRigidState> {
    (0..n)
        .map(|i| {
            let k = 1.0 + i as f64;
            DiffRigidState {
                pos: [0.7 * k, 1.3, -0.4 * k],
                orientation: [0.5, -0.8 * k, 0.3, 0.2],
                vel: [0.11, -0.07 * k, 0.05],
                ang_vel: [0.03 * k, 0.02, -0.04],
            }
        })
        .collect()
}

fn loss(s: &DiffRigidScene, seeds: &[DiffRigidState]) -> f64 {
    let out = simulate(s).unwrap();
    let mut l = 0.0;
    for (b, k) in out.iter().zip(seeds) {
        for (x, w) in b
            .pos
            .iter()
            .chain(&b.orientation)
            .chain(&b.vel)
            .chain(&b.ang_vel)
            .zip(
                k.pos
                    .iter()
                    .chain(&k.orientation)
                    .chain(&k.vel)
                    .chain(&k.ang_vel),
            )
        {
            l += x * w;
        }
    }
    l
}

/// Compare every analytic derivative with a central difference.
fn check(s: &DiffRigidScene, what: &str, kinds: [bool; 3]) -> usize {
    let seeds = seeds(s.bodies.len());
    let g = gradients(s, &seeds).unwrap();
    assert!((g.loss - loss(s, &seeds)).abs() < 1e-12 * (1.0 + g.loss.abs()));
    for (k, want) in kinds.iter().enumerate() {
        assert_eq!(
            g.activations[k] > 0,
            *want,
            "{what}: contact kinds {:?}",
            g.activations
        );
    }
    let mut worst = 0.0f64;
    let mut checked = 0;
    let mut fine = 0;
    // Central differences at eps = 1e-6. A resting contact sits on its
    // activation surface (C ≈ 0 after each projection), so a perturbation
    // can switch a contact on or off within 1e-6; there the difference is
    // retaken at 1e-7, inside the branch the forward pass took.
    let mut compare = |name: String, analytic: f64, f: &dyn Fn(&mut DiffRigidScene, f64)| {
        let fd_at = |eps: f64| {
            let (mut plus, mut minus) = (s.clone(), s.clone());
            f(&mut plus, eps);
            f(&mut minus, -eps);
            (loss(&plus, &seeds) - loss(&minus, &seeds)) / (2.0 * eps)
        };
        let scaled = |fd: f64| (fd - analytic).abs() / (1e-4 + 1e-3 * fd.abs().max(analytic.abs()));
        let mut fd = fd_at(1e-6);
        let mut err = scaled(fd);
        if err >= 1.0 {
            fd = fd_at(1e-7);
            err = scaled(fd);
            fine += 1;
        }
        worst = worst.max(err);
        assert!(err < 1.0, "{what} {name}: analytic {analytic} vs fd {fd}");
        checked += 1;
    };
    for i in 0..s.bodies.len() {
        if s.bodies[i].inv_mass == 0.0 {
            continue;
        }
        for k in 0..3 {
            compare(
                format!("body {i} pos[{k}]"),
                g.initial[i].pos[k],
                &|s, e| s.bodies[i].pos[k] += e,
            );
            compare(
                format!("body {i} vel[{k}]"),
                g.initial[i].vel[k],
                &|s, e| s.bodies[i].vel[k] += e,
            );
            compare(
                format!("body {i} ang_vel[{k}]"),
                g.initial[i].ang_vel[k],
                &|s, e| s.bodies[i].ang_vel[k] += e,
            );
        }
        for k in 0..4 {
            compare(
                format!("body {i} orientation[{k}]"),
                g.initial[i].orientation[k],
                &|s, e| s.bodies[i].orientation[k] += e,
            );
        }
    }
    // d/dα is large for stiff contacts (α = 1e-5): perturb α by 1e-5·eps
    // and compare the derivative with respect to α/1e-5.
    compare("compliance".into(), g.compliance * 1e-5, &|s, e| {
        s.compliance += e * 1e-5
    });
    eprintln!(
        "{what}: {checked} derivatives ({fine} retaken at 1e-7), worst scaled error {worst:.3}, activations {:?}",
        g.activations
    );
    checked
}

#[test]
fn tumbling_box_on_the_ground() {
    let mut b = body([0.0, 0.62, 0.0], 0, [4.0, 6.0, 8.0]);
    b.orientation = quat([1.0, 0.3, 0.2], 0.4);
    b.vel = [0.8, -0.5, 0.1];
    b.ang_vel = [1.0, 0.5, -2.0];
    let s = scene(
        vec![b],
        vec![DiffShape::Box {
            half_extents: [0.4, 0.25, 0.3],
        }],
        30,
    );
    check(&s, "box", [true, false, false]);
}

#[test]
fn tilted_capsule_on_the_ground_and_on_a_box() {
    let mut cap = body([0.3, 0.9, 0.0], 0, [6.0, 2.0, 6.0]);
    cap.orientation = quat([0.0, 0.0, 1.0], 0.3);
    cap.vel = [0.2, -1.0, 0.0];
    cap.ang_vel = [0.0, 0.4, 1.5];
    let s = scene(
        vec![cap, fixed([0.6, 0.25, 0.0], 1)],
        vec![
            // An off-axis capsule: the segment runs along (1, 1, 0).
            DiffShape::Capsule {
                a: [-0.3, -0.3, 0.0],
                b: [0.3, 0.3, 0.0],
                radius: 0.1,
            },
            DiffShape::Box {
                half_extents: [0.3, 0.25, 0.4],
            },
        ],
        30,
    );
    check(&s, "capsule", [true, true, false]);
}

#[test]
fn hull_lands_on_a_box() {
    // An irregular octahedron.
    let hull = DiffShape::Hull {
        vertices: vec![
            [0.3, 0.0, 0.0],
            [-0.25, 0.0, 0.0],
            [0.0, 0.35, 0.0],
            [0.0, -0.3, 0.02],
            [0.0, 0.0, 0.28],
            [0.02, 0.0, -0.3],
        ],
    };
    let mut h = body([0.1, 0.9, 0.05], 0, [12.0, 10.0, 11.0]);
    h.orientation = quat([0.2, 1.0, 0.4], 0.7);
    h.vel = [0.0, -1.5, 0.0];
    h.ang_vel = [0.6, -0.3, 0.9];
    let s = scene(
        vec![h, fixed([0.0, 0.25, 0.0], 1)],
        vec![
            hull,
            DiffShape::Box {
                half_extents: [0.6, 0.25, 0.6],
            },
        ],
        30,
    );
    check(&s, "hull", [false, true, false]);
}

#[test]
fn colliding_capsules_and_box_on_box() {
    let mut a = body([-0.5, 1.0, 0.0], 0, [6.0, 2.0, 6.0]);
    a.vel = [2.0, 0.0, 0.1];
    a.ang_vel = [0.0, 0.0, 0.5];
    let mut b = body([0.5, 1.0, 0.05], 0, [6.0, 2.0, 6.0]);
    b.orientation = quat([1.0, 0.0, 0.0], 1.2);
    b.vel = [-2.0, 0.0, 0.0];
    let mut top = body([2.0, 0.9, 0.0], 1, [6.0, 6.0, 6.0]);
    top.orientation = quat([0.0, 1.0, 0.0], 0.3);
    top.vel = [0.0, -0.5, 0.0];
    let mut s = scene(
        vec![a, b, fixed([2.0, 0.25, 0.0], 1), top],
        vec![
            DiffShape::Capsule {
                a: [0.0, -0.3, 0.0],
                b: [0.0, 0.3, 0.0],
                radius: 0.15,
            },
            DiffShape::Box {
                half_extents: [0.25; 3],
            },
        ],
        24,
    );
    s.gravity = [0.0, -2.0, 0.0];
    check(&s, "capsules and boxes", [false, true, true]);
}

#[test]
fn malformed_diff_scenes_are_typed_errors() {
    let base = scene(
        vec![body([0.0, 1.0, 0.0], 0, [1.0; 3])],
        vec![DiffShape::Sphere { radius: 0.1 }],
        1,
    );
    let mut s = base.clone();
    s.dt = 0.0;
    assert!(matches!(simulate(&s), Err(RigidDiffError::InvalidScene(_))));
    let mut s = base.clone();
    s.bodies[0].shape = 3;
    assert!(matches!(
        simulate(&s),
        Err(RigidDiffError::InvalidBody { .. })
    ));
    let mut s = base.clone();
    s.shapes[0] = DiffShape::Hull {
        vertices: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
    };
    assert!(matches!(
        simulate(&s),
        Err(RigidDiffError::InvalidShape { .. })
    ));
    let mut s = base.clone();
    s.bodies[0].orientation = [0.0, 0.0, 0.0, 2.0];
    assert!(matches!(
        simulate(&s),
        Err(RigidDiffError::InvalidBody { .. })
    ));
    assert_eq!(gradients(&base, &[]), Err(RigidDiffError::SeedLength));
}
