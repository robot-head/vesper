//! ABBA comparison of the Rust build (the seed copy, `vesper_kinetic_rust`,
//! baseline) and the Vesper port (this package, candidate), in one process:
//! each epoch times every workload in Rust, port, port, Rust order, and
//! vesper-bench analyzes the paired totals. Port wave P3b lane 8 (#1070).
//!
//! `abba compare <epochs> <ops>` prints one line per workload.
//! `abba flip-gate <workload> <ops>` prints the flip gate's pairs line for
//! one workload (`ABBA_EPOCHS` epochs, default 30).
//! `abba one <rust|port> <workload> <ops>` runs one workload `ops` times
//! (for `valgrind --tool=callgrind`).
//!
//! Workloads, one run each:
//! - `morton`: 4,096 encodes and decodes of 30-bit codes.
//! - `fastmath`: `sin`, `cos`, `exp`, `log` and `atan2` of 512 arguments
//!   each, plus the five binary32 functions.
//! - `crmath`: the correctly rounded `sin`, `cos`, `exp`, `log` and `atan2`
//!   of 64 arguments each (part 3).
//! - `gjk`: `gjk::separation` of 256 random shape pairs (sphere, capsule
//!   and box hulls, separated and overlapping; part 4).
//! - `colorer`: `Colorer::color` of 8,192 two-body constraints over 2,048
//!   bodies.
//! - `multi-colorer`: `MultiColorer::color` of 4,096 four-body sets over
//!   4,096 bodies.
//! - `lbvh`: `Lbvh::build` of 4,096 random boxes and `overlapping_pairs`.
//! - `sap`: `SweepAndPrune::build` of the same boxes and
//!   `overlapping_pairs`.
//! - `scene`: the golden rigid-body scene (tests/common `scene`: 180 spheres,
//!   32 joints, Morton-cell broadphase), built and stepped 16 ticks.
//! - `pile-lbvh`: a 16 × 16 × 4 pile (1,024 spheres) with the LBVH
//!   broadphase, 4 ticks on one solver thread; `pile-lbvh-4t` the same on 4
//!   threads (the solver's fan-out stays Rust in part 1).

use std::time::Instant;
use vesper_bench::{PairedSample, analyze_pairs};

macro_rules! workloads {
    ($m:ident, $s:ident) => {
        mod $m {
            use $s::lbvh::{Aabb, Lbvh};
            use $s::rigid::{
                CcdConfig, ColliderShape, RigidBody, RigidCollider, RigidConfig, RigidMaterial,
                RigidWorld, box_inv_inertia, sphere_inv_inertia,
            };
            use $s::{
                Body, BroadphaseMode, Colorer, ConstraintBodies, DistanceConstraint, MultiColorer,
                SphereCollider, SweepAndPrune, World, WorldConfig, crmath, fastmath, gjk, morton,
            };

            fn rng(state: &mut u64) -> u64 {
                *state ^= *state << 13;
                *state ^= *state >> 7;
                *state ^= *state << 17;
                *state
            }

            pub fn morton(seed: u64) -> u64 {
                let mut x = seed | 1;
                let mut acc = 0u64;
                for _ in 0..4096 {
                    let r = rng(&mut x);
                    let c = morton::encode(
                        (r & 0x3ff) as u16,
                        ((r >> 10) & 0x3ff) as u16,
                        ((r >> 20) & 0x3ff) as u16,
                    )
                    .unwrap();
                    let (a, b, d) = morton::decode(c).unwrap();
                    acc = acc.wrapping_add(u64::from(c) ^ u64::from(a ^ b ^ d));
                }
                acc
            }

            pub fn fastmath(seed: u64) -> u64 {
                let mut x = seed | 1;
                let mut acc = 0u64;
                for _ in 0..512 {
                    let r = rng(&mut x);
                    let v = (r >> 11) as f64 / (1u64 << 53) as f64 * 200.0 - 100.0;
                    for y in [
                        fastmath::sin(v),
                        fastmath::cos(v),
                        fastmath::exp(v * 0.25),
                        fastmath::log(v.abs() + 1e-3),
                        fastmath::atan2(v, 1.5),
                    ] {
                        acc = acc.wrapping_add(y.to_bits());
                    }
                    let f = v as f32;
                    for y in [
                        fastmath::sinf(f),
                        fastmath::cosf(f),
                        fastmath::expf(f * 0.25),
                        fastmath::logf(f.abs() + 1e-3),
                        fastmath::atan2f(f, 1.5),
                    ] {
                        acc = acc.wrapping_add(u64::from(y.to_bits()));
                    }
                }
                acc
            }

            pub fn crmath(seed: u64) -> u64 {
                let mut x = seed | 1;
                let mut acc = 0u64;
                for _ in 0..64 {
                    let r = rng(&mut x);
                    let v = (r >> 11) as f64 / (1u64 << 53) as f64 * 200.0 - 100.0;
                    for y in [
                        crmath::sin(v),
                        crmath::cos(v),
                        crmath::exp(v * 0.25),
                        crmath::log(v.abs() + 1e-3),
                        crmath::atan2(v, 1.5),
                    ] {
                        acc = acc.wrapping_add(y.to_bits());
                    }
                }
                acc
            }

            pub fn gjk(seed: u64) -> u64 {
                let mut x = seed | 1;
                let mut acc = 0u64;
                let unit = |x: &mut u64| (rng(x) >> 11) as f64 / (1u64 << 53) as f64;
                for _ in 0..256 {
                    let shape = |x: &mut u64, hull: &mut [[f64; 3]; 8]| {
                        let c = [unit(x) * 3.0, unit(x) * 3.0, unit(x) * 3.0];
                        let r = 0.2 + unit(x);
                        match rng(x) % 3 {
                            0 => (0, c, r),
                            1 => (1, c, r * 0.5),
                            _ => {
                                for (i, v) in hull.iter_mut().enumerate() {
                                    for (a, e) in v.iter_mut().enumerate() {
                                        let sign = if i >> a & 1 == 1 { 1.0 } else { -1.0 };
                                        *e = c[a] + sign * r * (0.7 + 0.3 * unit(x));
                                    }
                                }
                                (2, c, r)
                            }
                        }
                    };
                    let (mut ha, mut hb) = ([[0.0; 3]; 8], [[0.0; 3]; 8]);
                    let a = shape(&mut x, &mut ha);
                    let b = shape(&mut x, &mut hb);
                    fn make((k, c, r): (u32, [f64; 3], f64), h: &[[f64; 3]; 8]) -> gjk::Shape<'_> {
                        match k {
                            0 => gjk::Shape::Sphere {
                                center: c,
                                radius: r,
                            },
                            1 => gjk::Shape::Capsule {
                                a: c,
                                b: [c[0] + 0.8, c[1] - 0.3, c[2] + 0.5],
                                radius: r,
                            },
                            _ => gjk::Shape::Polytope {
                                vertices: h.as_slice(),
                            },
                        }
                    }
                    let (sa, sb) = (make(a, &ha), make(b, &hb));
                    match gjk::separation(&sa, &sb) {
                        Ok(s) => {
                            acc = acc.wrapping_add(s.distance.to_bits() ^ s.normal[1].to_bits())
                        }
                        Err(e) => acc = acc.wrapping_add(e as u64),
                    }
                }
                acc
            }

            pub fn colorer(seed: u64) -> u64 {
                let mut x = seed | 1;
                let constraints: Vec<ConstraintBodies> = (0..8192)
                    .map(|_| {
                        let r = rng(&mut x);
                        let a = (r % 2048) as u32;
                        let b = ((r >> 16) % 2048) as u32;
                        ConstraintBodies {
                            a,
                            b: (b != a && r & (1 << 40) != 0).then_some(b),
                        }
                    })
                    .collect();
                let mut c = Colorer::new(2048, 8192);
                c.color(&constraints).unwrap();
                c.layer_count() as u64 + u64::from(c.visiting_order()[17])
            }

            pub fn multi_colorer(seed: u64) -> u64 {
                let mut x = seed | 1;
                let mut bodies = Vec::new();
                let mut offsets = vec![0u32];
                for _ in 0..4096 {
                    let base = (rng(&mut x) % 4092) as u32;
                    bodies.extend([base, base + 1, base + 2, base + 3]);
                    offsets.push(bodies.len() as u32);
                }
                let mut c = MultiColorer::new(4096, 4096);
                c.color(&bodies, &offsets).unwrap();
                c.layer_count() as u64 + u64::from(c.visiting_order()[17])
            }

            fn boxes(seed: u64) -> Vec<Aabb> {
                let mut x = seed | 1;
                let mut unit = || (rng(&mut x) >> 40) as f32 / (1u64 << 24) as f32;
                (0..4096)
                    .map(|_| {
                        let c = [unit() * 64.0, unit() * 64.0, unit() * 64.0];
                        let h = unit() * 0.75 + 0.05;
                        Aabb {
                            min: [c[0] - h, c[1] - h, c[2] - h],
                            max: [c[0] + h, c[1] + h, c[2] + h],
                        }
                    })
                    .collect()
            }

            pub fn lbvh(seed: u64) -> u64 {
                let b = boxes(seed);
                let mut t = Lbvh::with_capacity(b.len());
                t.build(&b).unwrap();
                let mut out = Vec::new();
                t.overlapping_pairs(&mut out, usize::MAX).unwrap();
                out.len() as u64 + u64::from(out.last().map_or(0, |p| p.1))
            }

            pub fn sap(seed: u64) -> u64 {
                let b = boxes(seed);
                let mut s = SweepAndPrune::with_capacity(b.len());
                s.build(&b).unwrap();
                let mut out = Vec::new();
                s.overlapping_pairs(&mut out, usize::MAX).unwrap();
                out.len() as u64 + u64::from(out.last().map_or(0, |p| p.1))
            }

            fn dynamic(pos: [f32; 3], vel: [f32; 3]) -> Body {
                Body {
                    pos,
                    vel,
                    inv_mass: 1.0,
                    compliance_alpha: 0.0,
                    collider_asset_id: 0,
                    collision_layer_mask: 1,
                }
            }

            /// tests/common `scene_parts`.
            pub fn scene(_: u64) -> u64 {
                let mut bodies = Vec::new();
                for gx in 0..6i32 {
                    for gz in 0..6i32 {
                        for level in 0..4i32 {
                            let seed = (gx * 31 + gz * 17 + level * 7) % 11 - 5;
                            bodies.push(dynamic(
                                [
                                    gx as f32 * 0.625,
                                    0.5 + level as f32 * 0.75 + (gx + gz) as f32 * 0.0625,
                                    gz as f32 * 0.625,
                                ],
                                [seed as f32 * 0.125, 0.0, -(seed as f32) * 0.0625],
                            ));
                        }
                    }
                }
                let mut joints = Vec::new();
                for chain in 0..4i32 {
                    let base = [chain as f32 * 1.25 - 0.5, 6.0, -1.5 - chain as f32 * 0.75];
                    let first = bodies.len() as u32;
                    bodies.push(Body {
                        inv_mass: 0.0,
                        ..dynamic(base, [0.0; 3])
                    });
                    for link in 1..=8i32 {
                        bodies.push(dynamic(
                            [base[0] + link as f32 * 0.5, base[1], base[2]],
                            [0.0; 3],
                        ));
                        joints.push(DistanceConstraint {
                            a: first + link as u32 - 1,
                            b: first + link as u32,
                            rest_length: 0.5,
                            compliance: if chain % 2 == 0 { 0.0 } else { 1.0e-4 },
                        });
                    }
                }
                let config = WorldConfig {
                    cell_size: 1.0,
                    contact_margin: 0.125,
                    max_contacts: 2048,
                    ..WorldConfig::default()
                };
                let mut w =
                    World::new(config, &bodies, &[SphereCollider { radius: 0.25 }], &joints)
                        .unwrap();
                for _ in 0..16 {
                    w.step().unwrap();
                }
                u64::from(w.header().world_state_blake3[0])
            }

            fn pile(threads: usize) -> u64 {
                let mut bodies = Vec::new();
                for gx in 0..16 {
                    for gz in 0..16 {
                        for level in 0..4 {
                            let seed = (gx * 13 + gz * 7 + level * 5) % 9 - 4;
                            bodies.push(dynamic(
                                [
                                    gx as f32 * 0.5,
                                    0.25 + level as f32 * 0.5,
                                    gz as f32 * 0.5 + (level % 2) as f32 * 0.125,
                                ],
                                [seed as f32 * 0.0625, 0.0, -(seed as f32) * 0.03125],
                            ));
                        }
                    }
                }
                let config = WorldConfig {
                    cell_size: 1.0,
                    contact_margin: 0.0625,
                    max_contacts: bodies.len() * 8,
                    ..WorldConfig::default()
                };
                let mut w =
                    World::new(config, &bodies, &[SphereCollider { radius: 0.25 }], &[]).unwrap();
                w.set_broadphase(BroadphaseMode::Lbvh);
                w.set_solver_threads(threads).unwrap();
                for _ in 0..4 {
                    w.step().unwrap();
                }
                u64::from(w.header().world_state_blake3[0])
            }

            pub fn pile_lbvh(_: u64) -> u64 {
                pile(1)
            }

            pub fn pile_lbvh_4t(_: u64) -> u64 {
                pile(4)
            }

            pub fn ccd(seed: u64) -> u64 {
                let mat = RigidMaterial {
                    static_friction: 0.4,
                    dynamic_friction: 0.3,
                    restitution: 0.2,
                    rolling_friction: 0.0,
                };
                let colliders = [
                    RigidCollider {
                        shape: ColliderShape::Box {
                            half_extents: [0.05, 4.0, 4.0],
                        },
                        material: mat,
                    },
                    RigidCollider {
                        shape: ColliderShape::Sphere { radius: 0.2 },
                        material: mat,
                    },
                    RigidCollider {
                        shape: ColliderShape::Capsule {
                            half_height: 0.25,
                            radius: 0.15,
                        },
                        material: mat,
                    },
                    RigidCollider {
                        shape: ColliderShape::Box {
                            half_extents: [0.2, 0.2, 0.2],
                        },
                        material: mat,
                    },
                ];
                let mut bodies = vec![RigidBody::new([0.0, 0.0, 0.0], 0.0, [0.0; 3], 0)];
                let mut s = seed ^ 0x9e37_79b9_7f4a_7c15;
                for i in 0..16 {
                    let y = ((i / 4) as f32 - 1.5) * 1.5;
                    let z = ((i % 4) as f32 - 1.5) * 1.5;
                    let col = 1 + (i % 3);
                    let mut b = RigidBody::new(
                        [-1.5 - (rng(&mut s) % 100) as f32 * 0.01, y, z],
                        1.0,
                        if col == 1 {
                            sphere_inv_inertia(1.0, 0.2)
                        } else {
                            box_inv_inertia(1.0, [0.2; 3])
                        },
                        col as u32,
                    );
                    b.vel = [120.0 + (rng(&mut s) % 40) as f32, 0.0, 0.0];
                    b.ang_vel = [1.0, 2.0, 0.5];
                    bodies.push(b);
                }
                let cfg = RigidConfig {
                    dt: 1.0 / 60.0,
                    substeps: 1,
                    gravity: [0.0, -9.81, 0.0],
                    ..RigidConfig::default()
                };
                let mut w = RigidWorld::new(cfg, &bodies, &colliders).unwrap();
                w.set_ccd(Some(CcdConfig::default())).unwrap();
                for _ in 0..4 {
                    w.step().unwrap();
                }
                let mut h = 0u64;
                for i in 1..bodies.len() {
                    h ^= w.body(i).unwrap().pos[0].to_bits() as u64;
                }
                h
            }
        }
    };
}

workloads!(rust_build, vesper_kinetic_rust);
workloads!(port_build, vesper_kinetic);

type Work = fn(u64) -> u64;

const WORKLOADS: [(&str, Work, Work); 12] = [
    ("morton", rust_build::morton, port_build::morton),
    ("fastmath", rust_build::fastmath, port_build::fastmath),
    ("crmath", rust_build::crmath, port_build::crmath),
    ("gjk", rust_build::gjk, port_build::gjk),
    ("colorer", rust_build::colorer, port_build::colorer),
    (
        "multi-colorer",
        rust_build::multi_colorer,
        port_build::multi_colorer,
    ),
    ("lbvh", rust_build::lbvh, port_build::lbvh),
    ("sap", rust_build::sap, port_build::sap),
    ("scene", rust_build::scene, port_build::scene),
    ("pile-lbvh", rust_build::pile_lbvh, port_build::pile_lbvh),
    (
        "pile-lbvh-4t",
        rust_build::pile_lbvh_4t,
        port_build::pile_lbvh_4t,
    ),
    ("ccd", rust_build::ccd, port_build::ccd),
];

/// Total nanoseconds for `ops` timed runs after `ops / 4` warm-up ones.
fn measure(work: Work, ops: u64) -> u64 {
    let mut sink = 0;
    for seed in 0..ops / 4 {
        sink ^= work(seed);
    }
    let start = Instant::now();
    for seed in 0..ops {
        sink ^= work(seed);
    }
    let elapsed = start.elapsed().as_nanos() as u64;
    std::hint::black_box(sink);
    elapsed
}

fn compare(epochs: usize, ops: u64, only: Option<&str>, gate: bool) {
    assert_eq!(vesper_kinetic::IMPLEMENTATION, "vesper");
    assert_eq!(vesper_kinetic_rust::IMPLEMENTATION, "rust");
    for (name, rust, port) in WORKLOADS {
        if only.is_some_and(|o| o != name) {
            continue;
        }
        // Both builds compute the same answers.
        for seed in 0..4 {
            assert_eq!(rust(seed), port(seed), "{name} differs");
        }
        let mut pairs = Vec::new();
        for _ in 0..epochs {
            let a1 = measure(rust, ops);
            let b1 = measure(port, ops);
            let b2 = measure(port, ops);
            let a2 = measure(rust, ops);
            pairs.push(PairedSample::new(a1 + a2, b1 + b2, 2 * ops).unwrap());
        }
        if gate {
            let line: Vec<String> = pairs
                .iter()
                .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
                .collect();
            println!("flip-gate pairs {name} {}", line.join(" "));
            continue;
        }
        let per_op = |t: u64| t as f64 / (2 * ops) as f64;
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let rust_ns = median(pairs.iter().map(|p| per_op(p.baseline_total())).collect());
        let port_ns = median(pairs.iter().map(|p| per_op(p.candidate_total())).collect());
        let ratios: Vec<f64> = pairs
            .iter()
            .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
            .collect();
        let lo = ratios.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = ratios.iter().copied().fold(0.0, f64::max);
        let ratio = median(ratios);
        let analysis = analyze_pairs(&pairs, 0).unwrap();
        let bp = |s: PairedSample| s.regression_bp_bounds().floor as f64 / 100.0;
        println!(
            "{name:<13} rust {rust_ns:>11.0} ns/run  port {port_ns:>11.0} ns/run  port/rust {ratio:.2}x \
             (range {lo:.2}x..{hi:.2}x, sample median regression {:+.0}%..{:+.0}%, {epochs} epochs)",
            bp(analysis.median_lower),
            bp(analysis.median_upper),
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["compare", epochs, ops] => {
            compare(epochs.parse().unwrap(), ops.parse().unwrap(), None, false)
        }
        ["compare", epochs, ops, only] => compare(
            epochs.parse().unwrap(),
            ops.parse().unwrap(),
            Some(only),
            false,
        ),
        // The flip gate's harness protocol (scripts/ci/flip_gate.py):
        // `ABBA_EPOCHS` (default 30) epochs of one workload, one pairs line.
        ["flip-gate", workload, ops] => {
            let epochs = std::env::var("ABBA_EPOCHS")
                .ok()
                .and_then(|e| e.parse().ok())
                .unwrap_or(30);
            compare(epochs, ops.parse().unwrap(), Some(workload), true)
        }
        ["one", build, workload, ops] => {
            let (_, rust, port) = WORKLOADS
                .iter()
                .find(|w| w.0 == *workload)
                .expect("a workload name");
            let work = if *build == "rust" { rust } else { port };
            let mut sink = 0;
            for seed in 0..ops.parse::<u64>().unwrap() {
                sink ^= work(seed);
            }
            println!("{sink}");
        }
        _ => {
            eprintln!(
                "usage: abba compare <epochs> <ops> [workload] | abba flip-gate <workload> <ops> | abba one <rust|port> <workload> <ops>"
            );
            std::process::exit(2);
        }
    }
}
