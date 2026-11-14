//! XPBD soft bodies: volume preservation, a hanging rope, thread-count
//! independence, the colouring check and typed errors.

use vesper_kinetic::coloring::sets_are_disjoint;
use vesper_kinetic::soft::{SoftConfig, SoftConstraint, SoftError, SoftParticle, SoftWorld};

fn jelly(threads: usize, threshold: usize) -> SoftWorld {
    jelly_cells(threads, threshold, 4)
}

fn jelly_cells(threads: usize, threshold: usize, cells: u32) -> SoftWorld {
    let cell = 1.0 / cells as f32;
    let (particles, constraints) =
        SoftWorld::tetrahedral_box([-0.5, 0.4, -0.5], cell, [cells; 3], 2.0, 1e-6, 0.0).unwrap();
    let mut w = SoftWorld::new(SoftConfig::default(), &particles, &constraints).unwrap();
    w.set_solver_threads(threads).unwrap();
    w.set_parallel_threshold(threshold);
    w
}

#[test]
fn a_jelly_cube_lands_and_keeps_its_volume() {
    let mut w = jelly(1, 64);
    let rest = w.rest_volume();
    assert!((rest - 1.0).abs() < 1e-5, "rest volume {rest}");
    assert!((w.volume() - rest).abs() < 1e-5);
    let mut worst = 0.0f64;
    for _ in 0..360 {
        w.step().unwrap();
        worst = worst.max((w.volume() - rest).abs() / rest);
    }
    let lowest = (0..w.len())
        .map(|i| w.particle(i).unwrap().pos[1])
        .fold(f32::MAX, f32::min);
    let fastest = (0..w.len())
        .map(|i| {
            let v = w.particle(i).unwrap().vel;
            (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
        })
        .fold(0.0, f32::max);
    eprintln!(
        "jelly: worst volume error {worst}, strain {}, lowest {lowest}, fastest {fastest}",
        w.max_strain()
    );
    assert!(worst < 0.02, "volume error {worst}");
    assert!(lowest > -1e-4, "particles stay above the ground: {lowest}");
    assert!(fastest < 0.05, "comes to rest: {fastest}");
    assert!(w.max_strain() < 0.05, "edges within 5%: {}", w.max_strain());
}

#[test]
fn soft_solve_is_bit_identical_for_every_thread_count() {
    let reference = {
        let mut w = jelly_cells(1, 1, 3);
        for _ in 0..30 {
            w.step().unwrap();
        }
        w.hash()
    };
    for threads in [2, 3, 5] {
        let mut w = jelly_cells(threads, 1, 3);
        for _ in 0..30 {
            w.step().unwrap();
        }
        assert_eq!(w.hash(), reference, "{threads} threads");
    }
    let hex: String = reference
        .sha256
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    eprintln!("arch={} soft sha256={hex}", std::env::consts::ARCH);
    assert_eq!(hex, GOLDEN_SOFT_SHA256);
}

const GOLDEN_SOFT_SHA256: &str = "5ac5aba4a4e6dda7b718217e5b9d98cdfee90e7cc781b6415c39603b2129a8b3";

#[test]
fn colours_of_the_tetrahedral_mesh_are_disjoint() {
    let w = jelly(1, 64);
    let (bodies, offsets) = w.constraint_sets();
    assert!(sets_are_disjoint(w.colorer(), bodies, offsets));
    assert!(
        w.layer_count() > 4 && w.layer_count() <= 64,
        "{}",
        w.layer_count()
    );
}

#[test]
fn a_rope_hangs_straight_at_its_length() {
    let links = 20;
    let seg = 0.05f32;
    let mut particles = vec![SoftParticle {
        pos: [0.0, 2.0, 0.0],
        vel: [0.0; 3],
        inv_mass: 0.0,
    }];
    let mut constraints = Vec::new();
    for i in 1..=links {
        particles.push(SoftParticle {
            pos: [seg * i as f32, 2.0, 0.0],
            vel: [0.0; 3],
            inv_mass: 1.0,
        });
        constraints.push(SoftConstraint::Distance {
            a: i as u32 - 1,
            b: i as u32,
            rest: seg,
            compliance: 0.0,
        });
    }
    let cfg = SoftConfig {
        ground: None,
        substeps: 32,
        ..SoftConfig::default()
    };
    let mut w = SoftWorld::new(cfg, &particles, &constraints).unwrap();
    let energy = |w: &SoftWorld| -> f64 {
        (1..=links)
            .map(|i| {
                let p = w.particle(i).unwrap();
                let v2: f32 = p.vel.iter().map(|v| v * v).sum();
                f64::from(0.5 * v2 + 9.81 * p.pos[1])
            })
            .sum()
    };
    let e0 = energy(&w);
    let mut lowest = f32::MAX;
    for _ in 0..600 {
        w.step().unwrap();
        // The energy read from the last substep's velocities Δx/h swings
        // while the tip whips (by up to ~2% here) and trends down with
        // XPBD's damping.
        let e = energy(&w);
        assert!(e <= e0 * 1.03, "rope gained energy {e0} -> {e}");
        assert!(w.max_strain() < 1e-2, "strain {}", w.max_strain());
        lowest = lowest.min(w.particle(links).unwrap().pos[1]);
    }
    eprintln!("rope lowest tip {lowest}, strain {}", w.max_strain());
    assert!(energy(&w) < e0, "damped overall");
    // The whip stretches a link by under 1% at worst; at rest the strain
    // is ~2e-4. It swung down to nearly its full length below the pivot.
    assert!(lowest < 2.0 - 0.9 * seg * links as f32, "{lowest}");
}

#[test]
fn malformed_soft_worlds_are_typed_errors() {
    let p = |x: f32| SoftParticle {
        pos: [x, 0.0, 0.0],
        vel: [0.0; 3],
        inv_mass: 1.0,
    };
    let ps = [p(0.0), p(1.0), p(2.0), p(3.0)];
    let cfg = SoftConfig::default();
    let bad = |cs: &[SoftConstraint]| {
        matches!(
            SoftWorld::new(cfg, &ps, cs),
            Err(SoftError::InvalidConstraint { .. })
        )
    };
    assert!(bad(&[SoftConstraint::Distance {
        a: 0,
        b: 9,
        rest: 1.0,
        compliance: 0.0
    }]));
    assert!(bad(&[SoftConstraint::Distance {
        a: 1,
        b: 1,
        rest: 1.0,
        compliance: 0.0
    }]));
    assert!(bad(&[SoftConstraint::Distance {
        a: 0,
        b: 1,
        rest: f32::NAN,
        compliance: 0.0
    }]));
    assert!(bad(&[SoftConstraint::Volume {
        tet: [0, 1, 2, 2],
        rest_volume: 1.0,
        compliance: 0.0
    }]));
    assert!(bad(&[SoftConstraint::Volume {
        tet: [0, 1, 2, 3],
        rest_volume: 0.0,
        compliance: 0.0
    }]));
    let mut nan = ps;
    nan[2].pos[1] = f32::INFINITY;
    assert!(matches!(
        SoftWorld::new(cfg, &nan, &[]),
        Err(SoftError::InvalidParticle { particle: 2 })
    ));
    assert!(matches!(
        SoftWorld::new(SoftConfig { substeps: 0, ..cfg }, &ps, &[]),
        Err(SoftError::InvalidConfig(_))
    ));
    let mut w = SoftWorld::new(cfg, &ps, &[]).unwrap();
    assert_eq!(w.set_solver_threads(0), Err(SoftError::InvalidThreads(0)));
    assert_eq!(w.set_solver_threads(65), Err(SoftError::InvalidThreads(65)));
    assert!(SoftWorld::tetrahedral_box([0.0; 3], 0.0, [1, 1, 1], 1.0, 0.0, 0.0).is_err());
    assert!(SoftWorld::tetrahedral_box([0.0; 3], 1.0, [0, 1, 1], 1.0, 0.0, 0.0).is_err());
}
