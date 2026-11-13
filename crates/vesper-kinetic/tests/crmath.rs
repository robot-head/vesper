//! Correctly rounded math: every reference in `data/crmath_refs.txt`
//! (mpmath at 400 and 700 bits, rounded exactly, see
//! `data/gen_crmath_refs.py`) must match bit for bit, the C99 Annex F special
//! values must hold, and a fixed input sweep must hash to the committed value
//! on every CI architecture.

use sha2::{Digest, Sha256};
use vesper_kinetic::crmath;

const REFS: &str = include_str!("data/crmath_refs.txt");

fn f64_of(hex: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(hex, 16).unwrap())
}

fn f32_of(hex: &str) -> f32 {
    f32::from_bits(u32::from_str_radix(hex, 16).unwrap())
}

#[test]
fn matches_mpmath_references_bit_for_bit() {
    // `VESPER_CRMATH_EXTRA_REFS` names a larger generated file to check too.
    let extra = std::env::var_os("VESPER_CRMATH_EXTRA_REFS")
        .map(|p| std::fs::read_to_string(p).expect("read extra references"))
        .unwrap_or_default();
    let mut checked = std::collections::BTreeMap::<&str, usize>::new();
    let mut failures = Vec::new();
    for line in REFS.lines().chain(extra.lines()) {
        let parts: Vec<&str> = line.split(' ').collect();
        let (name, got, want) = match parts.as_slice() {
            [name @ ("sin" | "cos" | "exp" | "log"), x, r] => {
                let x = f64_of(x);
                let got = match *name {
                    "sin" => crmath::sin(x),
                    "cos" => crmath::cos(x),
                    "exp" => crmath::exp(x),
                    _ => crmath::log(x),
                };
                (*name, got.to_bits(), f64_of(r).to_bits())
            }
            [name @ ("sinf" | "cosf" | "expf" | "logf"), x, r] => {
                let x = f32_of(x);
                let got = match *name {
                    "sinf" => crmath::sinf(x),
                    "cosf" => crmath::cosf(x),
                    "expf" => crmath::expf(x),
                    _ => crmath::logf(x),
                };
                (
                    *name,
                    u64::from(got.to_bits()),
                    u64::from(f32_of(r).to_bits()),
                )
            }
            ["atan2", y, x, r] => (
                "atan2",
                crmath::atan2(f64_of(y), f64_of(x)).to_bits(),
                f64_of(r).to_bits(),
            ),
            ["atan2f", y, x, r] => (
                "atan2f",
                u64::from(crmath::atan2f(f32_of(y), f32_of(x)).to_bits()),
                u64::from(f32_of(r).to_bits()),
            ),
            _ => panic!("bad reference line {line}"),
        };
        *checked.entry(name).or_default() += 1;
        if got != want {
            failures.push(format!("{line}: got {got:x}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(checked.len(), 10, "{checked:?}");
    assert!(checked.values().all(|n| *n >= 100), "{checked:?}");
    assert_eq!(crmath::precision_cap_hits(), 0);
}

#[test]
fn annex_f_special_values() {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
    let inf = f64::INFINITY;
    let same = |a: f64, b: f64| a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan());
    for (got, want) in [
        (crmath::exp(0.0), 1.0),
        (crmath::exp(-0.0), 1.0),
        (crmath::exp(inf), inf),
        (crmath::exp(-inf), 0.0),
        (crmath::exp(1000.0), inf),
        (crmath::exp(-1000.0), 0.0),
        (crmath::exp(f64::NAN), f64::NAN),
        (crmath::log(1.0), 0.0),
        (crmath::log(0.0), -inf),
        (crmath::log(-0.0), -inf),
        (crmath::log(-1.0), f64::NAN),
        (crmath::log(inf), inf),
        (crmath::sin(0.0), 0.0),
        (crmath::sin(-0.0), -0.0),
        (crmath::sin(inf), f64::NAN),
        (crmath::cos(0.0), 1.0),
        (crmath::cos(-inf), f64::NAN),
        (crmath::atan2(0.0, 1.0), 0.0),
        (crmath::atan2(-0.0, 1.0), -0.0),
        (crmath::atan2(0.0, -0.0), PI),
        (crmath::atan2(-0.0, -1.0), -PI),
        (crmath::atan2(0.0, 0.0), 0.0),
        (crmath::atan2(1.0, 0.0), FRAC_PI_2),
        (crmath::atan2(-1.0, -0.0), -FRAC_PI_2),
        (crmath::atan2(inf, inf), FRAC_PI_4),
        (crmath::atan2(-inf, -inf), -3.0 * FRAC_PI_4),
        (crmath::atan2(inf, 5.0), FRAC_PI_2),
        (crmath::atan2(5.0, -inf), PI),
        (crmath::atan2(-5.0, inf), -0.0),
        (crmath::atan2(f64::NAN, 1.0), f64::NAN),
    ] {
        assert!(same(got, want), "got {got:e}, want {want:e}");
    }
    // 3π/4 is rounded from the exact value, not from 3 · FRAC_PI_4.
    assert_eq!(crmath::atan2(1.0, -1.0), 2.356194490192345);
    assert_eq!(crmath::atan2f(f32::INFINITY, f32::NEG_INFINITY), 2.3561945);
    assert_eq!(crmath::expf(0.0), 1.0);
    assert_eq!(crmath::logf(1.0), 0.0);
    assert!(crmath::logf(-1.0).is_nan());
    assert_eq!(crmath::sinf(-0.0).to_bits(), (-0.0f32).to_bits());
    assert_eq!(crmath::atan2f(0.0, -1.0), std::f32::consts::PI);
}

/// SHA-256 of the outputs for a fixed pseudo-random sweep. CI runs this on
/// Linux x86_64, macOS AArch64 and Windows x86_64; one committed value for
/// all of them is the cross-architecture bit-equality check.
#[test]
fn output_sweep_hash_is_identical_on_every_architecture() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut h = Sha256::new();
    for _ in 0..400 {
        let r = next();
        // Doubles with exponents spread over [-64, 64), and floats likewise.
        let x = f64::from_bits((r & 0x800f_ffff_ffff_ffff) | ((959 + (r >> 52) % 128) << 52));
        let y = f64::from_bits(next() & 0xbfff_ffff_ffff_ffff);
        let xf = x as f32;
        for v in [
            crmath::sin(x),
            crmath::cos(x),
            crmath::exp(x / 64.0),
            crmath::log(x.abs()),
            crmath::atan2(y, x),
        ] {
            h.update(v.to_bits().to_le_bytes());
        }
        for v in [
            crmath::sinf(xf),
            crmath::cosf(xf),
            crmath::expf(xf / 64.0),
            crmath::logf(xf.abs()),
            crmath::atan2f(y as f32, xf),
        ] {
            h.update(v.to_bits().to_le_bytes());
        }
    }
    let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    eprintln!(
        "arch={} crmath sweep sha256={digest}",
        std::env::consts::ARCH
    );
    assert_eq!(digest, GOLDEN_SWEEP);
}

const GOLDEN_SWEEP: &str = "31a7d9a5e658c80918e6440949c62124f304e7b3dcf33093bce5084f28daaef6";
