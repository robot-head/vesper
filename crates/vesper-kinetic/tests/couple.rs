//! Two-way soft-rigid coupling: a jelly resting on a static box, a rigid
//! box supported by a jelly, momentum conservation in free flight,
//! determinism across thread counts and typed errors.

use vesper_kinetic::rigid::{
    ColliderShape, RigidBody, RigidCollider, RigidConfig, RigidMaterial, RigidWorld,
    box_inv_inertia,
};
use vesper_kinetic::soft::{SoftConfig, SoftWorld};
use vesper_kinetic::{CoupledError, CoupledWorld, CouplingConfig};

const DT: f32 = 1.0 / 120.0;

fn cube(h: [f32; 3]) -> RigidCollider {
    RigidCollider {
        shape: ColliderShape::Box { half_extents: h },
        material: RigidMaterial::default(),
    }
}

fn jelly(origin: [f32; 3], gravity: [f32; 3], ground: Option<f32>) -> SoftWorld {
    let (particles, constraints) =
        SoftWorld::tetrahedral_box(origin, 0.1, [3, 3, 3], 1.0, 1e-6, 0.0).unwrap();
    SoftWorld::new(
        SoftConfig {
            dt: DT,
            substeps: 16,
            gravity,
            ground,
            ground_friction: 0.5,
        },
        &particles,
        &constraints,
    )
    .unwrap()
}

fn rigid(bodies: &[RigidBody], colliders: &[RigidCollider], gravity: [f32; 3]) -> RigidWorld {
    RigidWorld::new(
        RigidConfig {
            dt: DT,
            gravity,
            ground: None,
            ..RigidConfig::default()
        },
        bodies,
        colliders,
    )
    .unwrap()
}

fn lowest(soft: &SoftWorld) -> f32 {
    (0..soft.len())
        .map(|i| soft.particle(i).unwrap().pos[1])
        .fold(f32::INFINITY, f32::min)
}

const G: [f32; 3] = [0.0, -9.81, 0.0];

#[test]
fn a_jelly_rests_on_a_static_box() {
    // Static box with its top at y = 0.5; the jelly starts 0.2 above it.
    let table = RigidBody::new([0.0, 0.25, 0.0], 0.0, [0.0; 3], 0);
    let r = rigid(&[table], &[cube([1.0, 0.25, 1.0])], G);
    let s = jelly([-0.15, 0.7, -0.15], G, Some(-10.0));
    let radius = 0.02;
    let mut w = CoupledWorld::new(
        r,
        s,
        CouplingConfig {
            particle_radius: radius,
            friction: 0.5,
        },
    )
    .unwrap();
    let mut contacts = 0;
    for _ in 0..240 {
        w.step().unwrap();
        contacts = contacts.max(w.last_contacts());
    }
    assert!(contacts >= 16, "the bottom face touches: {contacts}");
    let low = lowest(w.soft());
    // Resting on the top face, sagging at most about g dt² per tick.
    assert!(
        (low - (0.5 + radius)).abs() < 5e-3,
        "lowest particle at {low}"
    );
    assert!(w.soft().max_strain() < 0.05, "{}", w.soft().max_strain());
}

#[test]
fn a_jelly_holds_up_a_rigid_box() {
    // The jelly stands on the soft ground; a 0.2 kg rigid box sits on it.
    let s = jelly([-0.15, 0.0, -0.15], G, Some(0.0));
    let h = [0.12, 0.05, 0.12];
    let lid = RigidBody::new([0.0, 0.3 + 0.05 + 0.03, 0.0], 5.0, box_inv_inertia(0.2, h), 0);
    let r = rigid(&[lid], &[cube(h)], G);
    let mut w = CoupledWorld::new(r, s, CouplingConfig::default()).unwrap();
    for _ in 0..360 {
        w.step().unwrap();
    }
    let b = w.rigid().body(0).unwrap();
    // Supported: it neither falls through the jelly (top at 0.3) nor
    // bounces away, and it comes to rest.
    assert!(b.pos[1] > 0.3 + 0.05 - 0.03 && b.pos[1] < 0.42, "{:?}", b.pos);
    assert!(b.vel.iter().all(|v| v.abs() < 0.05), "{:?}", b.vel);
    let low = lowest(w.soft());
    assert!(low > -0.01, "the jelly stays on the ground: {low}");
}

fn momentum(w: &CoupledWorld) -> [f64; 3] {
    let mut p = [0.0f64; 3];
    for i in 0..w.soft().len() {
        let q = w.soft().particle(i).unwrap();
        for k in 0..3 {
            p[k] += f64::from(q.vel[k]) / f64::from(q.inv_mass);
        }
    }
    for i in 0..w.rigid().len() {
        let b = w.rigid().body(i).unwrap();
        for k in 0..3 {
            p[k] += f64::from(b.vel[k]) / f64::from(b.inv_mass);
        }
    }
    p
}

fn collision_scene(soft_threads: usize, rigid_threads: usize) -> CoupledWorld {
    let zero = [0.0; 3];
    let mut s = jelly([-0.15, -0.15, -0.15], zero, None);
    s.set_solver_threads(soft_threads).unwrap();
    s.set_parallel_threshold(0);
    // A free 2 kg box thrown at the jelly.
    let h = [0.2, 0.2, 0.2];
    let mut boxy = RigidBody::new([0.6, 0.05, 0.0], 0.5, box_inv_inertia(2.0, h), 0);
    boxy.vel = [-3.0, 0.0, 0.0];
    let mut r = rigid(&[boxy], &[cube(h)], zero);
    r.set_solver_threads(rigid_threads).unwrap();
    r.set_parallel_threshold(0);
    CoupledWorld::new(
        r,
        s,
        CouplingConfig {
            particle_radius: 0.02,
            friction: 0.3,
        },
    )
    .unwrap()
}

#[test]
fn coupling_conserves_linear_momentum() {
    let mut w = collision_scene(1, 1);
    let p0 = momentum(&w);
    let mut touched = 0;
    for _ in 0..120 {
        w.step().unwrap();
        touched += w.last_contacts();
    }
    assert!(touched > 50, "they collided: {touched}");
    let p1 = momentum(&w);
    for k in 0..3 {
        assert!((p1[k] - p0[k]).abs() < 2e-3 * 6.0, "{p0:?} -> {p1:?}");
    }
    // The box was slowed and the jelly pushed.
    let b = w.rigid().body(0).unwrap();
    assert!(b.vel[0] > -2.5, "{:?}", b.vel);
}

#[test]
fn coupling_is_bit_identical_across_thread_counts() {
    let mut reference = collision_scene(1, 1);
    let mut hashes = Vec::new();
    for _ in 0..40 {
        hashes.push(reference.step().unwrap());
    }
    for (st, rt) in [(2, 3), (4, 1)] {
        let mut w = collision_scene(st, rt);
        for (t, h) in hashes.iter().enumerate() {
            assert_eq!(&w.step().unwrap(), h, "tick {t}, threads {st}/{rt}");
        }
    }
}

#[test]
fn coupling_errors_are_typed() {
    let r = || rigid(&[], &[], G);
    let s = || jelly([0.0; 3], G, Some(0.0));
    let bad = |c: CouplingConfig| {
        assert!(matches!(
            CoupledWorld::new(r(), s(), c),
            Err(CoupledError::InvalidConfig(_))
        ));
    };
    bad(CouplingConfig {
        particle_radius: 0.0,
        friction: 0.0,
    });
    bad(CouplingConfig {
        particle_radius: 0.01,
        friction: -1.0,
    });
    bad(CouplingConfig {
        particle_radius: f32::NAN,
        friction: 0.0,
    });
    let other_dt = RigidWorld::new(
        RigidConfig {
            dt: 1.0 / 60.0,
            ..RigidConfig::default()
        },
        &[],
        &[],
    )
    .unwrap();
    assert!(matches!(
        CoupledWorld::new(other_dt, s(), CouplingConfig::default()),
        Err(CoupledError::InvalidConfig(_))
    ));
}
