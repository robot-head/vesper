//! Contact gradients and gradients in the World's colour order: the adjoint
//! against central finite differences, the forward model against the
//! binary32 World, and typed errors.

#![allow(clippy::needless_range_loop)]

use vesper_kinetic::diff::{
    DiffConstraint, DiffContacts, DiffError, DiffOptions, DiffOrder, DiffParticle, DiffScene,
    gradients_with, simulate_with,
};
use vesper_kinetic::{Body, DistanceConstraint, SphereCollider, World, WorldConfig};

const RADIUS: f64 = 0.25;

/// A ball dropping onto the ground, a ball dropping off-centre onto a
/// static ball (it rests, slides off and falls), a two-ball stack settling
/// on the ground, and a two-link pendulum. Every contact is pressed by
/// gravity along its normal, so no contact sits at its activation boundary
/// (an inelastic sideways collision leaves the pair touching at `C ≈ 0`,
/// where rounding picks the branch and the loss is not differentiable).
fn scene() -> (DiffScene, DiffContacts) {
    let p = |pos: [f64; 3], vel: [f64; 3], inv_mass: f64| DiffParticle { pos, vel, inv_mass };
    let particles = vec![
        p([0.0, 0.9, 0.0], [0.2, 0.0, 0.1], 1.0),
        p([2.05, 1.05, 0.03], [0.0, -0.5, 0.0], 1.0),
        p([2.0, 0.4, 0.0], [0.0; 3], 0.0),
        p([5.0, 1.6, 0.0], [0.0; 3], 0.0),
        p([5.6, 1.6, 0.0], [0.0, 0.0, 0.3], 1.0),
        p([6.2, 1.6, 0.0], [0.0, 0.0, 0.0], 0.8),
        p([8.0, 0.26, 0.0], [0.05, 0.0, 0.0], 2.0),
        p([8.02, 0.9, -0.01], [0.0, -0.2, 0.0], 1.0),
    ];
    // Listed tip first, so list order and colour order differ.
    let constraints = vec![
        DiffConstraint {
            a: 4,
            b: 5,
            rest_length: 0.6,
            compliance: 2e-4,
        },
        DiffConstraint {
            a: 3,
            b: 4,
            rest_length: 0.6,
            compliance: 1e-4,
        },
    ];
    let n = particles.len();
    let scene = DiffScene {
        particles,
        constraints,
        gravity: [0.0, -9.81, 0.0],
        dt: 1.0 / 120.0,
        substeps: 4,
        ticks: 48,
    };
    let contacts = DiffContacts {
        radii: vec![RADIUS; n],
        compliance: (0..n).map(|i| 1e-5 * (1 + i % 3) as f64).collect(),
        ground_y: Some(0.0),
        margin: 0.05,
    };
    (scene, contacts)
}

fn opts(order: DiffOrder, contacts: &DiffContacts) -> DiffOptions {
    DiffOptions {
        order,
        contacts: Some(contacts.clone()),
    }
}

fn seeds(n: usize) -> (Vec<[f64; 3]>, Vec<[f64; 3]>) {
    let sp = (0..n)
        .map(|i| [0.3 + 0.1 * i as f64, -0.7, 0.2 * (i % 3) as f64])
        .collect();
    let sv = (0..n)
        .map(|i| [0.05 * i as f64, 0.4, -0.3 + 0.1 * (i % 2) as f64])
        .collect();
    (sp, sv)
}

fn loss(scene: &DiffScene, o: &DiffOptions, sp: &[[f64; 3]], sv: &[[f64; 3]]) -> f64 {
    let (x, v) = simulate_with(scene, o).unwrap();
    let mut l = 0.0;
    for i in 0..x.len() {
        for k in 0..3 {
            l += sp[i][k] * x[i][k] + sv[i][k] * v[i][k];
        }
    }
    l
}

fn check(analytic: f64, numeric: f64, what: &str) {
    // Central differences have O(eps²) truncation error, larger where a
    // contact is stiff.
    let tol = 5e-6 + 5e-5 * analytic.abs().max(numeric.abs());
    assert!(
        (analytic - numeric).abs() <= tol,
        "{what}: adjoint {analytic} vs finite difference {numeric}"
    );
}

#[test]
fn contact_adjoint_in_world_colour_order_matches_finite_differences() {
    let (scene, contacts) = scene();
    let n = scene.particles.len();
    let (sp, sv) = seeds(n);
    for order in [DiffOrder::WorldColors, DiffOrder::List] {
        let o = opts(order, &contacts);
        let g = gradients_with(&scene, &o, &sp, &sv).unwrap();
        assert!(
            g.contact_activations > 100,
            "{order:?}: {} contact projections",
            g.contact_activations
        );
        assert!((g.gradients.loss - loss(&scene, &o, &sp, &sv)).abs() < 1e-12);
        let eps = 1e-6;
        let fd = |f: &dyn Fn(&mut DiffScene, &mut DiffContacts, f64)| {
            let (mut sp_scene, mut sp_c) = (scene.clone(), contacts.clone());
            f(&mut sp_scene, &mut sp_c, eps);
            let (mut sm_scene, mut sm_c) = (scene.clone(), contacts.clone());
            f(&mut sm_scene, &mut sm_c, -eps);
            (loss(&sp_scene, &opts(order, &sp_c), &sp, &sv)
                - loss(&sm_scene, &opts(order, &sm_c), &sp, &sv))
                / (2.0 * eps)
        };
        let mut nonzero = 0;
        for i in 0..n {
            for k in 0..3 {
                let num = fd(&|s, _, e| s.particles[i].pos[k] += e);
                check(
                    g.gradients.d_pos0[i][k],
                    num,
                    &format!("{order:?} dL/dx0[{i}][{k}]"),
                );
                let num = fd(&|s, _, e| s.particles[i].vel[k] += e);
                check(
                    g.gradients.d_vel0[i][k],
                    num,
                    &format!("{order:?} dL/dv0[{i}][{k}]"),
                );
                nonzero += usize::from(g.gradients.d_pos0[i][k].abs() > 1e-3);
            }
            let num = fd(&|_, c, e| c.radii[i] += e);
            check(g.d_radius[i], num, &format!("{order:?} dL/dr[{i}]"));
            let num = fd(&|_, c, e| c.compliance[i] += e * 1e-5);
            check(
                g.d_contact_compliance[i] * 1e-5,
                num,
                &format!("{order:?} dL/dα_c[{i}]"),
            );
        }
        for j in 0..scene.constraints.len() {
            let num = fd(&|s, _, e| s.constraints[j].compliance += e * 1e-4);
            check(
                g.gradients.d_compliance[j] * 1e-4,
                num,
                &format!("{order:?} dL/dα[{j}]"),
            );
        }
        assert!(nonzero > 8, "{order:?}: gradients are not trivially zero");
        // Contacts matter: radius gradients are nonzero for touching balls.
        assert!(g.d_radius.iter().filter(|d| d.abs() > 1e-3).count() >= 3);
    }
}

#[test]
fn colour_order_changes_the_result_and_matches_the_world() {
    let (scene, contacts) = scene();
    let world_order = simulate_with(&scene, &opts(DiffOrder::WorldColors, &contacts)).unwrap();
    let list_order = simulate_with(&scene, &opts(DiffOrder::List, &contacts)).unwrap();
    let max_diff = |a: &[[f64; 3]], b: &[[f64; 3]]| {
        a.iter()
            .zip(b)
            .flat_map(|(p, q)| (0..3).map(move |k| (p[k] - q[k]).abs()))
            .fold(0.0f64, f64::max)
    };
    assert!(
        max_diff(&world_order.0, &list_order.0) > 1e-9,
        "order is observable"
    );

    // The same scene in the binary32 World (one collider, per-body contact
    // compliance).
    let bodies: Vec<Body> = scene
        .particles
        .iter()
        .enumerate()
        .map(|(i, p)| Body {
            pos: p.pos.map(|v| v as f32),
            vel: p.vel.map(|v| v as f32),
            inv_mass: p.inv_mass as f32,
            compliance_alpha: contacts.compliance[i] as f32,
            collider_asset_id: 0,
            collision_layer_mask: 1,
        })
        .collect();
    let joints: Vec<DistanceConstraint> = scene
        .constraints
        .iter()
        .map(|c| DistanceConstraint {
            a: c.a as u32,
            b: c.b as u32,
            rest_length: c.rest_length as f32,
            compliance: c.compliance as f32,
        })
        .collect();
    let config = WorldConfig {
        dt: scene.dt as f32,
        substeps: scene.substeps,
        contact_margin: contacts.margin as f32,
        ..WorldConfig::default()
    };
    let mut world = World::new(
        config,
        &bodies,
        &[SphereCollider {
            radius: RADIUS as f32,
        }],
        &joints,
    )
    .unwrap();
    for _ in 0..scene.ticks {
        world.step().unwrap();
    }
    let got: Vec<[f64; 3]> = (0..bodies.len())
        .map(|i| world.bodies().get(i).unwrap().pos.map(f64::from))
        .collect();
    let d = max_diff(&got, &world_order.0);
    assert!(d < 2e-3, "binary64 colour-order model vs World: {d}");
}

#[test]
fn malformed_contact_options_are_typed_errors() {
    let (scene, contacts) = scene();
    let n = scene.particles.len();
    let (sp, sv) = seeds(n);
    let mut short = contacts.clone();
    short.radii.pop();
    let mut negative = contacts.clone();
    negative.compliance[2] = -1.0;
    let mut nan_ground = contacts.clone();
    nan_ground.ground_y = Some(f64::NAN);
    for bad in [short, negative, nan_ground] {
        let o = opts(DiffOrder::WorldColors, &bad);
        assert!(matches!(
            simulate_with(&scene, &o),
            Err(DiffError::InvalidScene(_))
        ));
        assert!(matches!(
            gradients_with(&scene, &o, &sp, &sv),
            Err(DiffError::InvalidScene(_))
        ));
    }
    // Coincident contact centres have no derivative.
    let mut coincident = scene.clone();
    coincident.particles[7].pos = coincident.particles[6].pos;
    coincident.particles[7].vel = coincident.particles[6].vel;
    coincident.particles[7].inv_mass = coincident.particles[6].inv_mass;
    let o = opts(DiffOrder::WorldColors, &contacts);
    assert!(matches!(
        simulate_with(&coincident, &o),
        Err(DiffError::Degenerate { .. })
    ));
}
