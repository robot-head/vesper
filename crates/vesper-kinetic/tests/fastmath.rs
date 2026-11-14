//! The fast deterministic transcendentals against the correctly rounded
//! path: the documented ULP bound on a dense sample, Annex F specials, the
//! DetFloat allowlist, and a pinned cross-architecture output hash.

use sha2::{Digest, Sha256};
use vesper_kinetic::{crmath, fastmath};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Log-uniform magnitude in `[2^lo, 2^hi)` with a random sign, built
    /// from bits (no platform math, so the inputs are identical everywhere).
    fn magnitude(&mut self, lo: i32, hi: i32) -> f64 {
        let e = lo + (self.next() % (hi - lo) as u64) as i32;
        let mantissa = self.next() >> 12;
        let v = if e < -1022 {
            // Subnormal: shift the significand down.
            f64::from_bits(((1u64 << 52) | mantissa) >> (-1022 - e).min(63))
        } else {
            f64::from_bits(((e + 1023) as u64) << 52 | mantissa)
        };
        if self.next() & 1 == 1 { -v } else { v }
    }
}

/// Points per function: dense near the interesting regions plus a
/// log-uniform sweep. `VESPER_FASTMATH_SAMPLES` raises it for local runs.
fn samples() -> usize {
    std::env::var("VESPER_FASTMATH_SAMPLES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1500)
}

type F64Pair = (
    &'static str,
    fn(f64) -> f64,
    fn(f64) -> f64,
    fn(&mut Rng) -> f64,
);

fn unary_cases() -> Vec<F64Pair> {
    vec![
        ("sin", fastmath::sin, crmath::sin, |r| match r.next() % 4 {
            0 => r.magnitude(-30, 1),
            1 => (r.unit() * 2.0 - 1.0) * 8.0,
            2 => r.magnitude(1, 21),
            _ => {
                // Near multiples of π/2, where reduction cancels.
                let k = (r.next() % 4000) as f64;
                k * std::f64::consts::FRAC_PI_2 + (r.unit() - 0.5) * 1e-6
            }
        }),
        ("cos", fastmath::cos, crmath::cos, |r| match r.next() % 4 {
            0 => r.magnitude(-30, 1),
            1 => (r.unit() * 2.0 - 1.0) * 8.0,
            2 => r.magnitude(1, 21),
            _ => {
                let k = (r.next() % 4000) as f64;
                k * std::f64::consts::FRAC_PI_2 + (r.unit() - 0.5) * 1e-6
            }
        }),
        ("exp", fastmath::exp, crmath::exp, |r| match r.next() % 3 {
            0 => (r.unit() * 2.0 - 1.0) * 745.0,
            1 => (r.unit() * 2.0 - 1.0) * 2.0,
            _ => r.magnitude(-40, -1),
        }),
        ("log", fastmath::log, crmath::log, |r| match r.next() % 3 {
            0 => r.magnitude(-1074, 1024).abs(),
            1 => 1.0 + (r.unit() - 0.5) * 1e-3,
            _ => r.unit() * 4.0 + 1e-300,
        }),
    ]
}

#[test]
fn binary64_functions_stay_within_the_documented_ulp_bound() {
    let n = samples();
    let mut rng = Rng(0xfa57_0001);
    for (name, fast, exact, gen_x) in unary_cases() {
        let mut worst = (0u64, 0.0f64);
        let mut exact_hits = 0usize;
        for _ in 0..n {
            let x = gen_x(&mut rng);
            let d = fastmath::ulp_distance(fast(x), exact(x));
            if d == 0 {
                exact_hits += 1;
            }
            if d > worst.0 {
                worst = (d, x);
            }
        }
        eprintln!(
            "{name}: max {} ulp at {:e}, correctly rounded {exact_hits}/{n}",
            worst.0, worst.1
        );
        assert!(
            worst.0 <= fastmath::F64_MAX_ULP,
            "{name}: {} ulp at {:e}",
            worst.0,
            worst.1
        );
        assert!(
            exact_hits * 10 >= n * 9,
            "{name}: only {exact_hits}/{n} exact"
        );
    }
    let mut worst = (0u64, 0.0f64, 0.0f64);
    let mut exact_hits = 0usize;
    for _ in 0..n {
        let (y, x) = match rng.next() % 3 {
            0 => (rng.unit() * 2.0 - 1.0, rng.unit() * 2.0 - 1.0),
            1 => (rng.magnitude(-60, 60), rng.magnitude(-60, 60)),
            _ => (rng.magnitude(-3, 3), -(rng.magnitude(-3, 3).abs())),
        };
        let d = fastmath::ulp_distance(fastmath::atan2(y, x), crmath::atan2(y, x));
        if d == 0 {
            exact_hits += 1;
        }
        if d > worst.0 {
            worst = (d, y, x);
        }
    }
    eprintln!(
        "atan2: max {} ulp at ({:e}, {:e}), correctly rounded {exact_hits}/{n}",
        worst.0, worst.1, worst.2
    );
    assert!(worst.0 <= fastmath::F64_MAX_ULP, "atan2 {worst:?}");
}

#[test]
fn binary32_functions_stay_within_the_documented_ulp_bound() {
    let n = samples();
    let mut rng = Rng(0xfa57_0002);
    type F32Pair = (&'static str, fn(f32) -> f32, fn(f32) -> f32, f32, f32);
    let cases: [F32Pair; 4] = [
        ("sinf", fastmath::sinf, crmath::sinf, -200.0, 200.0),
        ("cosf", fastmath::cosf, crmath::cosf, -200.0, 200.0),
        ("expf", fastmath::expf, crmath::expf, -103.0, 88.7),
        ("logf", fastmath::logf, crmath::logf, 0.0, 1000.0),
    ];
    for (name, fast, exact, lo, hi) in cases {
        let mut worst = 0u64;
        let mut misrounded = 0usize;
        for _ in 0..n {
            let x = (f64::from(lo) + rng.unit() * f64::from(hi - lo)) as f32;
            let d = fastmath::ulp_distance_f32(fast(x), exact(x));
            misrounded += usize::from(d != 0);
            worst = worst.max(d);
        }
        eprintln!("{name}: max {worst} ulp, {misrounded}/{n} not correctly rounded");
        assert!(worst <= fastmath::F32_MAX_ULP, "{name}: {worst} ulp");
    }
    let mut worst = 0u64;
    for _ in 0..n {
        let y = (rng.unit() * 20.0 - 10.0) as f32;
        let x = (rng.unit() * 20.0 - 10.0) as f32;
        worst = worst.max(fastmath::ulp_distance_f32(
            fastmath::atan2f(y, x),
            crmath::atan2f(y, x),
        ));
    }
    assert!(worst <= fastmath::F32_MAX_ULP, "atan2f: {worst} ulp");
}

#[test]
fn annex_f_special_values_match_the_correctly_rounded_path() {
    let specials = [
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MIN_POSITIVE,
        5e-324,
        -5e-324,
        f64::MAX,
        -f64::MAX,
        1.0,
        -1.0,
        709.782712893384,
        709.79,
        -745.1332191019411,
        -745.14,
        1e300,
        2.0f64.powi(20) * std::f64::consts::FRAC_PI_2,
        1e22,
    ];
    for x in specials {
        for (name, fast, exact) in [
            (
                "sin",
                fastmath::sin as fn(f64) -> f64,
                crmath::sin as fn(f64) -> f64,
            ),
            ("cos", fastmath::cos, crmath::cos),
            ("exp", fastmath::exp, crmath::exp),
            ("log", fastmath::log, crmath::log),
        ] {
            let (f, e) = (fast(x), exact(x));
            assert!(
                fastmath::ulp_distance(f, e) <= fastmath::F64_MAX_ULP,
                "{name}({x:e}) = {f:e}, correctly rounded {e:e}"
            );
            if e.is_nan() {
                assert!(f.is_nan(), "{name}({x:e})");
            } else if e.is_infinite() || e == 0.0 {
                assert_eq!(f.to_bits(), e.to_bits(), "{name}({x:e})");
            }
        }
        for y in specials {
            let (f, e) = (fastmath::atan2(y, x), crmath::atan2(y, x));
            if e.is_nan() {
                assert!(f.is_nan());
            } else {
                assert!(
                    fastmath::ulp_distance(f, e) <= 1,
                    "atan2({y:e}, {x:e}) = {f:e} vs {e:e}"
                );
                if e == 0.0 {
                    assert_eq!(f.to_bits(), e.to_bits(), "atan2({y:e}, {x:e}) zero sign");
                }
            }
        }
    }
    assert!(fastmath::log(-1.0).is_nan());
    assert!(fastmath::sin(f64::NAN).is_nan());
    assert!(fastmath::exp(f64::NAN).is_nan());
    assert_eq!(fastmath::log(0.0), f64::NEG_INFINITY);
    assert_eq!(fastmath::sin(-0.0).to_bits(), (-0.0f64).to_bits());
}

#[test]
fn detfloat_policy_allows_the_fast_symbols() {
    let policy = vesper_kinetic::DetFloatPolicy::default().with_fast_deterministic_math();
    for s in fastmath::SYMBOLS {
        assert!(policy.allowed_float_calls.iter().any(|a| a == s), "{s}");
    }
}

/// SHA-256 of the outputs over a fixed sweep; CI checks it on Linux
/// x86_64, macOS AArch64 and Windows x86_64.
const SWEEP_SHA256: &str = "28519723ef9864287c7eb635c1db175a6aed195a7fe3bb75850bb1e065e0fac3";

#[test]
fn output_sweep_hash_is_identical_on_every_architecture() {
    let mut rng = Rng(0x5eed_fa57);
    let mut h = Sha256::new();
    for _ in 0..20_000 {
        let x = match rng.next() % 3 {
            0 => (rng.unit() * 2.0 - 1.0) * 100.0,
            1 => rng.magnitude(-20, 20),
            _ => (rng.unit() * 2.0 - 1.0) * 700.0,
        };
        let y = rng.magnitude(-8, 8);
        for v in [
            fastmath::sin(x),
            fastmath::cos(x),
            fastmath::exp(x),
            fastmath::log(x.abs()),
            fastmath::atan2(y, x),
        ] {
            h.update(v.to_bits().to_le_bytes());
        }
        let xf = x as f32;
        for v in [
            fastmath::sinf(xf),
            fastmath::cosf(xf),
            fastmath::expf(xf),
            fastmath::logf(xf.abs()),
            fastmath::atan2f(y as f32, xf),
        ] {
            h.update(v.to_bits().to_le_bytes());
        }
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    eprintln!(
        "arch={} fastmath sweep sha256={hex}",
        std::env::consts::ARCH
    );
    assert_eq!(hex, SWEEP_SHA256);
}
