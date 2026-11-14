//! SIMD batch kernels and colour-parallel solving: every backend and every
//! thread count gives the scalar solver's bits.

mod common;

use vesper_kinetic::simd::{self, GroundLanes, LANES, PairLanes};
use vesper_kinetic::{
    BroadphaseMode, SimdBackend, SimdUnavailable, SolverMode, StepError, hash_world,
};

/// Small deterministic generator (SplitMix64).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn f(&mut self, lo: f32, hi: f32) -> f32 {
        let u = (self.next() >> 40) as f32 / (1u64 << 24) as f32;
        lo + (hi - lo) * u
    }
    /// A value from a mix of ordinary, tiny, zero and exactly-equal cases.
    fn coord(&mut self) -> f32 {
        match self.next() % 8 {
            0 => 0.0,
            1 => -0.0,
            2 => self.f(-1e-6, 1e-6),
            3 => self.f(-1e4, 1e4),
            _ => self.f(-4.0, 4.0),
        }
    }
    fn weight(&mut self) -> f32 {
        match self.next() % 4 {
            0 => 0.0,
            _ => self.f(0.0, 3.0),
        }
    }
}

fn random_pair(rng: &mut Rng) -> PairLanes {
    let mut p = PairLanes::default();
    for l in 0..LANES {
        for k in 0..3 {
            p.pa[k][l] = rng.coord();
            // Sometimes coincident or nearly coincident with a.
            p.pb[k][l] = match rng.next() % 5 {
                0 => p.pa[k][l],
                1 => p.pa[k][l] + rng.f(-1e-5, 1e-5),
                _ => rng.coord(),
            };
        }
        p.wa[l] = rng.weight();
        p.wb[l] = rng.weight();
        p.target[l] = match rng.next() % 3 {
            0 => 0.0,
            _ => rng.f(0.0, 6.0),
        };
        p.alpha[l] = match rng.next() % 3 {
            0 => 0.0,
            _ => rng.f(0.0, 1e-3),
        };
        if rng.next().is_multiple_of(2) {
            p.inequality |= 1 << l;
        }
    }
    p
}

fn bits3(v: &[[f32; LANES]; 3]) -> Vec<u32> {
    v.iter().flatten().map(|x| x.to_bits()).collect()
}
fn bits(v: &[f32; LANES]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

#[test]
fn every_available_backend_matches_the_portable_kernel_bit_for_bit() {
    let backends: Vec<_> = SimdBackend::available().collect();
    eprintln!("arch={} backends={backends:?}", std::env::consts::ARCH);
    assert!(backends.contains(&SimdBackend::Portable));
    #[cfg(target_arch = "aarch64")]
    assert!(
        backends.contains(&SimdBackend::Neon),
        "NEON is baseline on AArch64"
    );
    let mut rng = Rng(0x5eed);
    let inv_h2s = [57_600.0f32 * 16.0, 1.0, 0.0, 1e12];
    for round in 0..4000 {
        let input = random_pair(&mut rng);
        let inv_h2 = inv_h2s[round % inv_h2s.len()];
        let want = simd::solve_pair(SimdBackend::Portable, &input, inv_h2);
        let mut g = GroundLanes::default();
        for l in 0..LANES {
            g.py[l] = rng.coord();
            g.radius[l] = rng.f(0.0, 1.0);
            g.wa[l] = rng.weight();
            g.alpha[l] = input.alpha[l];
        }
        let ground = rng.coord();
        let want_g = simd::solve_ground(SimdBackend::Portable, &g, ground, inv_h2);
        for &b in &backends {
            let got = simd::solve_pair(b, &input, inv_h2);
            assert_eq!(got.active, want.active, "{b:?} round {round} active");
            assert_eq!(bits3(&got.na), bits3(&want.na), "{b:?} round {round} na");
            assert_eq!(bits3(&got.nb), bits3(&want.nb), "{b:?} round {round} nb");
            assert_eq!(bits3(&got.normal), bits3(&want.normal), "{b:?} normal");
            assert_eq!(bits(&got.sa), bits(&want.sa), "{b:?} sa");
            assert_eq!(bits(&got.sb), bits(&want.sb), "{b:?} sb");
            assert_eq!(bits(&got.w), bits(&want.w), "{b:?} w");
            let got_g = simd::solve_ground(b, &g, ground, inv_h2);
            assert_eq!(got_g.active, want_g.active, "{b:?} ground active");
            assert_eq!(bits(&got_g.y), bits(&want_g.y), "{b:?} ground y");
            assert_eq!(bits(&got_g.corr), bits(&want_g.corr), "{b:?} ground corr");
        }
    }
}

#[test]
fn coincident_lanes_take_the_scalar_fallback_direction() {
    let mut p = PairLanes::default();
    for l in 0..LANES {
        p.pa[1][l] = 1.0;
        p.pb[1][l] = 1.0;
        p.wa[l] = 1.0;
        p.wb[l] = 1.0;
        p.target[l] = 0.5;
    }
    for b in SimdBackend::available() {
        let s = simd::solve_pair(b, &p, 1.0);
        assert_eq!(s.active, 0xffff, "{b:?}");
        for l in 0..LANES {
            assert_eq!(
                [s.normal[0][l], s.normal[1][l], s.normal[2][l]],
                [0.0, 1.0, 0.0]
            );
            assert_eq!(s.na[1][l], 1.25, "{b:?}");
            assert_eq!(s.nb[1][l], 0.75, "{b:?}");
        }
    }
}

#[test]
fn unavailable_backends_and_bad_thread_counts_are_typed_errors() {
    let mut world = common::scene();
    for b in SimdBackend::ALL {
        let r = world.set_simd_backend(b);
        if b.is_available() {
            assert_eq!(r, Ok(()));
            assert_eq!(world.simd_backend(), b);
        } else {
            assert_eq!(r, Err(SimdUnavailable(b)));
            assert_ne!(world.simd_backend(), b);
        }
    }
    assert!(matches!(
        world.set_solver_threads(0),
        Err(StepError::InvalidConfig(_))
    ));
    assert!(matches!(
        world.set_solver_threads(65),
        Err(StepError::InvalidConfig(_))
    ));
    assert_eq!(world.solver_threads(), 1);
    world.set_solver_threads(4).unwrap();
    assert_eq!(world.solver_threads(), 4);
}

const PILE_TICKS: u64 = 40;
const GOLDEN_PILE_SHA256: &str = "6a6bd49148e3f7d46d06a5d49de605c2833c987e669fce59a830391fd2804314";

fn pile_hash(
    solver: SolverMode,
    backend: SimdBackend,
    threads: usize,
    broadphase: BroadphaseMode,
) -> (String, usize) {
    let mut world = common::pile(12, 3);
    world.set_solver_mode(solver);
    world.set_simd_backend(backend).unwrap();
    world.set_solver_threads(threads).unwrap();
    world.set_parallel_threshold(16);
    world.set_broadphase(broadphase);
    let mut max_layer = 0;
    for _ in 0..PILE_TICKS {
        world.step().expect("step");
        max_layer = max_layer.max(world.last_layer_count());
    }
    let h = hash_world(world.tick(), world.bodies());
    (common::hex(&h.sha256), max_layer)
}

/// A dense pile (432 spheres, hundreds of contacts per colour) gives the
/// same bits for the scalar solver, every SIMD backend and 1–8 threads,
/// and the result is pinned across architectures.
#[test]
fn dense_pile_is_bit_identical_across_backends_and_threads() {
    let (want, layers) = pile_hash(
        SolverMode::Scalar,
        SimdBackend::Portable,
        1,
        BroadphaseMode::MortonCells,
    );
    eprintln!(
        "arch={} pile sha256={want} colours={layers}",
        std::env::consts::ARCH
    );
    for backend in SimdBackend::available() {
        for threads in [1, 2, 3, 8] {
            let (got, _) = pile_hash(
                SolverMode::Batched16,
                backend,
                threads,
                BroadphaseMode::Lbvh,
            );
            assert_eq!(got, want, "{backend:?} threads={threads}");
        }
    }
    assert_eq!(want, GOLDEN_PILE_SHA256);
}
