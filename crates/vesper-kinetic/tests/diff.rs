//! Reverse-mode XPBD gradients against central finite differences, and the
//! forward model against the World solver.

use vesper_kinetic::diff::{
    DiffConstraint, DiffError, DiffParticle, DiffScene, gradients, simulate,
};
use vesper_kinetic::{Body, DistanceConstraint, SphereCollider, World, WorldConfig};
use vesper_tensor::host::Storage;

fn chain() -> DiffScene {
    let mut particles = vec![DiffParticle {
        pos: [0.0, 2.0, 0.0],
        vel: [0.0; 3],
        inv_mass: 0.0,
    }];
    let mut constraints = Vec::new();
    for i in 1..=4 {
        particles.push(DiffParticle {
            pos: [0.45 * i as f64, 2.0 - 0.1 * i as f64, 0.05 * i as f64],
            vel: [0.1 * i as f64, 0.0, -0.2],
            inv_mass: 1.0 / (1.0 + 0.5 * i as f64),
        });
        constraints.push(DiffConstraint {
            a: i - 1,
            b: i,
            rest_length: 0.5,
            compliance: 1.0e-4 * i as f64,
        });
    }
    // A free pair as well.
    particles.push(DiffParticle {
        pos: [3.0, 1.0, 0.0],
        vel: [0.0, 1.0, 0.5],
        inv_mass: 1.0,
    });
    particles.push(DiffParticle {
        pos: [3.4, 1.3, 0.1],
        vel: [-0.5, 0.0, 0.0],
        inv_mass: 0.5,
    });
    constraints.push(DiffConstraint {
        a: 5,
        b: 6,
        rest_length: 0.6,
        compliance: 2.0e-3,
    });
    DiffScene {
        particles,
        constraints,
        gravity: [0.0, -9.81, 0.0],
        dt: 1.0 / 60.0,
        substeps: 4,
        ticks: 30,
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

fn loss(scene: &DiffScene, sp: &[[f64; 3]], sv: &[[f64; 3]]) -> f64 {
    let (x, v) = simulate(scene).unwrap();
    let mut l = 0.0;
    for i in 0..x.len() {
        for k in 0..3 {
            l += sp[i][k] * x[i][k] + sv[i][k] * v[i][k];
        }
    }
    l
}

fn check(analytic: f64, numeric: f64, what: &str) {
    let tol = 1e-6 + 1e-5 * analytic.abs().max(numeric.abs());
    assert!(
        (analytic - numeric).abs() <= tol,
        "{what}: adjoint {analytic} vs finite difference {numeric}"
    );
}

#[test]
fn adjoint_matches_central_finite_differences() {
    let scene = chain();
    let n = scene.particles.len();
    let (sp, sv) = seeds(n);
    let g = gradients(&scene, &sp, &sv).unwrap();
    assert!((g.loss - loss(&scene, &sp, &sv)).abs() < 1e-12);
    let eps = 1e-6;
    let mut nonzero = 0;
    for i in 0..n {
        for k in 0..3 {
            let mut plus = scene.clone();
            let mut minus = scene.clone();
            plus.particles[i].pos[k] += eps;
            minus.particles[i].pos[k] -= eps;
            let fd = (loss(&plus, &sp, &sv) - loss(&minus, &sp, &sv)) / (2.0 * eps);
            check(g.d_pos0[i][k], fd, &format!("dL/dx0[{i}][{k}]"));
            let mut plus = scene.clone();
            let mut minus = scene.clone();
            plus.particles[i].vel[k] += eps;
            minus.particles[i].vel[k] -= eps;
            let fd = (loss(&plus, &sp, &sv) - loss(&minus, &sp, &sv)) / (2.0 * eps);
            check(g.d_vel0[i][k], fd, &format!("dL/dv0[{i}][{k}]"));
            if g.d_pos0[i][k].abs() > 1e-3 {
                nonzero += 1;
            }
        }
    }
    for j in 0..scene.constraints.len() {
        let h = 1e-5 * scene.constraints[j].compliance;
        let mut plus = scene.clone();
        let mut minus = scene.clone();
        plus.constraints[j].compliance += h;
        minus.constraints[j].compliance -= h;
        let fd = (loss(&plus, &sp, &sv) - loss(&minus, &sp, &sv)) / (2.0 * h);
        check(g.d_compliance[j], fd, &format!("dL/dalpha[{j}]"));
        assert!(g.d_compliance[j].abs() > 1e-6, "compliance {j} matters");
    }
    assert!(nonzero > 10, "gradients are not trivially zero");
}

#[test]
fn gradients_export_as_tensor_host_vectors() {
    let scene = chain();
    let n = scene.particles.len();
    let (sp, sv) = seeds(n);
    let g = gradients(&scene, &sp, &sv).unwrap();
    let t = g.to_tensors();
    let names: Vec<&str> = t.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        ["d_loss_d_compliance", "d_loss_d_pos0", "d_loss_d_vel0"]
    );
    let Storage::F32(pos) = &t["d_loss_d_pos0"].storage else {
        panic!("f32 storage")
    };
    assert_eq!(pos.len(), 3 * n);
    assert_eq!(pos[4], g.d_pos0[1][1] as f32);
    let Storage::F32(alpha) = &t["d_loss_d_compliance"].storage else {
        panic!("f32 storage")
    };
    assert_eq!(alpha.len(), scene.constraints.len());
}

#[test]
fn forward_model_matches_the_world_solver_for_one_joint() {
    // One joint: no ordering differences, so binary64 and the binary32 World
    // agree up to binary32 rounding drift over 80 substeps.
    let p0 = [0.0f32, 1.0, 0.0];
    let p1 = [0.7f32, 1.1, 0.2];
    let v1 = [0.3f32, 0.0, -0.4];
    let scene = DiffScene {
        particles: vec![
            DiffParticle {
                pos: p0.map(f64::from),
                vel: [0.0; 3],
                inv_mass: 0.0,
            },
            DiffParticle {
                pos: p1.map(f64::from),
                vel: v1.map(f64::from),
                inv_mass: 1.0,
            },
        ],
        constraints: vec![DiffConstraint {
            a: 0,
            b: 1,
            rest_length: 0.5,
            compliance: 1.0e-4,
        }],
        gravity: [0.0, -9.81, 0.0],
        dt: 1.0 / 120.0,
        substeps: 4,
        ticks: 20,
    };
    let body = |pos, vel, inv_mass| Body {
        pos,
        vel,
        inv_mass,
        compliance_alpha: 0.0,
        collider_asset_id: 0,
        collision_layer_mask: 0,
    };
    let mut world = World::new(
        WorldConfig {
            ground_y: None,
            ..WorldConfig::default()
        },
        &[body(p0, [0.0; 3], 0.0), body(p1, v1, 1.0)],
        &[SphereCollider { radius: 0.1 }],
        &[DistanceConstraint {
            a: 0,
            b: 1,
            rest_length: 0.5,
            compliance: 1.0e-4,
        }],
    )
    .unwrap();
    for _ in 0..20 {
        world.step().unwrap();
    }
    let (x, v) = simulate(&scene).unwrap();
    let b = world.bodies().get(1).unwrap();
    for k in 0..3 {
        assert!(
            (x[1][k] - f64::from(b.pos[k])).abs() < 1e-4,
            "pos {k}: {:?} vs {:?}",
            x[1],
            b.pos
        );
        assert!((v[1][k] - f64::from(b.vel[k])).abs() < 1e-2, "vel {k}");
    }
}

#[test]
fn malformed_scenes_are_typed_errors() {
    let mut s = chain();
    s.constraints[0].b = 99;
    assert!(matches!(simulate(&s), Err(DiffError::InvalidScene(_))));
    let mut s = chain();
    s.dt = 0.0;
    assert!(matches!(simulate(&s), Err(DiffError::InvalidScene(_))));
    // Two coincident dynamic particles at rest: the joint has no direction.
    let mut s = chain();
    s.particles[1].pos = s.particles[0].pos;
    s.particles[1].vel = [0.0; 3];
    s.particles[0].inv_mass = 1.0;
    s.gravity = [0.0; 3];
    assert_eq!(
        simulate(&s),
        Err(DiffError::Degenerate {
            tick: 0,
            constraint: 0
        })
    );
    let s = chain();
    assert_eq!(
        gradients(&s, &[], &[]),
        Err(DiffError::SeedLength)
    );
}
