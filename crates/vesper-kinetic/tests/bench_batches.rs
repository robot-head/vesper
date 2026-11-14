//! Timing runs for the report (ignored by default; run with
//! `cargo test --release -p vesper-kinetic --test bench_batches -- --ignored --nocapture`).

mod common;

use std::hint::black_box;
use std::time::Instant;
use vesper_kinetic::simd::{self, LANES, PairLanes};
use vesper_kinetic::{BroadphaseMode, SimdBackend, SolverMode};

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
#[ignore = "timing run for the report"]
fn kernel_throughput() {
    let mut tiles = Vec::new();
    for t in 0..1024u32 {
        let mut p = PairLanes::default();
        for l in 0..LANES {
            let x = (t * 16 + l as u32) as f32;
            p.pa[0][l] = x * 0.001;
            p.pa[1][l] = 0.5 + (l as f32) * 0.01;
            p.pb[0][l] = x * 0.001 + 0.4;
            p.pb[2][l] = 0.05;
            p.wa[l] = 1.0;
            p.wb[l] = 1.0;
            p.target[l] = 0.5;
        }
        p.inequality = 0xffff;
        tiles.push(p);
    }
    for b in SimdBackend::available() {
        let mut runs = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            for _ in 0..100 {
                for t in &tiles {
                    black_box(simd::solve_pair(b, black_box(t), 921_600.0));
                }
            }
            runs.push(start.elapsed().as_secs_f64() * 1e9 / (100.0 * tiles.len() as f64));
        }
        println!(
            "kernel {b:?}: {:.1} ns per 16-lane pair batch (median of 9)",
            median(runs)
        );
    }
}

#[test]
#[ignore = "timing run for the report"]
fn world_step_time() {
    let lbvh = BroadphaseMode::Lbvh;
    let cases: Vec<(SolverMode, SimdBackend, usize, BroadphaseMode)> = {
        let mut v = vec![(SolverMode::Scalar, SimdBackend::Portable, 1, lbvh)];
        for b in SimdBackend::available() {
            v.push((SolverMode::Batched16, b, 1, lbvh));
        }
        let best = SimdBackend::detect();
        for t in [2, 4] {
            v.push((SolverMode::Batched16, best, t, lbvh));
        }
        v.push((
            SolverMode::Batched16,
            best,
            1,
            BroadphaseMode::SweepAndPrune,
        ));
        v.push((SolverMode::Batched16, best, 1, BroadphaseMode::MortonCells));
        v
    };
    for side in [32, 64] {
        for &(solver, backend, threads, broadphase) in &cases {
            let mut world = common::pile(side, 4);
            world.set_solver_mode(solver);
            world.set_simd_backend(backend).unwrap();
            world.set_solver_threads(threads).unwrap();
            world.set_broadphase(broadphase);
            for _ in 0..20 {
                world.step().unwrap();
            }
            let (mut runs, mut solve, mut broad) = (Vec::new(), Vec::new(), Vec::new());
            for _ in 0..40 {
                let start = Instant::now();
                world.step().unwrap();
                runs.push(start.elapsed().as_secs_f64() * 1e3);
                let t = world.last_timings();
                solve.push(t.solve.as_secs_f64() * 1e3);
                broad.push(t.broadphase.as_secs_f64() * 1e3);
            }
            println!(
                "world {} spheres {solver:?} {backend:?} threads={threads} {broadphase:?}: {:.3} ms/tick, solve {:.3} ms, broadphase {:.3} ms (medians of 40), {} contacts, {} colours",
                world.bodies().len(),
                median(runs),
                median(solve),
                median(broad),
                world.header().manifold_count,
                world.last_layer_count()
            );
        }
    }
}

#[test]
#[ignore = "timing run for the report"]
fn transcendental_speed() {
    use vesper_kinetic::{crmath, fastmath};
    let xs: Vec<f64> = (0..4096).map(|i| (i as f64 - 2048.0) * 0.0137).collect();
    type F = (&'static str, fn(f64) -> f64);
    let sets: [(&str, [F; 3]); 4] = [
        (
            "sin",
            [
                ("fast", fastmath::sin),
                ("crmath", crmath::sin),
                ("std", f64::sin),
            ],
        ),
        (
            "cos",
            [
                ("fast", fastmath::cos),
                ("crmath", crmath::cos),
                ("std", f64::cos),
            ],
        ),
        (
            "exp",
            [
                ("fast", fastmath::exp),
                ("crmath", crmath::exp),
                ("std", f64::exp),
            ],
        ),
        (
            "log",
            [
                ("fast", |x: f64| fastmath::log(x.abs())),
                ("crmath", |x: f64| crmath::log(x.abs())),
                ("std", |x: f64| x.abs().ln()),
            ],
        ),
    ];
    for (name, impls) in sets {
        for (which, f) in impls {
            let reps = if which == "crmath" { 1 } else { 200 };
            let start = Instant::now();
            for _ in 0..reps {
                for &x in &xs {
                    black_box(f(black_box(x)));
                }
            }
            let ns = start.elapsed().as_secs_f64() * 1e9 / (reps * xs.len()) as f64;
            println!("{name} {which}: {ns:.1} ns/call");
        }
    }
    let start = Instant::now();
    for _ in 0..200 {
        for &x in &xs {
            black_box(fastmath::atan2(black_box(x), 0.7));
        }
    }
    println!(
        "atan2 fast: {:.1} ns/call",
        start.elapsed().as_secs_f64() * 1e9 / (200.0 * 4096.0)
    );
}
