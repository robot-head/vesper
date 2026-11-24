//! Correctly rounded `sin`, `cos`, `atan2`, `exp` and `log` for binary32 and
//! binary64 (§36.5 DetFloat transcendental allowlist).
//!
//! Every function returns the floating-point number nearest to the exact
//! mathematical result (round to nearest, ties to even), with the C99
//! Annex F special values. Correct rounding fixes the answer for every input,
//! so the results are the same bits on every ISA, compiler and optimisation
//! level.
//!
//! # Method
//!
//! Each call evaluates the function in exact integer fixed-point arithmetic
//! (vesper/src/bignum.ves in the port build, `num-bigint` in the seed
//! build) with a rigorous absolute error bound, then rounds both ends of the
//! error interval. When both ends round to the same float, that float is the
//! correctly rounded result (rounding is monotone). Otherwise the
//! precision is doubled and the evaluation repeated (Ziv's strategy, as in
//! MPFR). No hardware floating-point instruction influences the result: the
//! inputs are decoded into integers, every intermediate is an integer, and the
//! final rounding is integer code. The only exact cases (`exp(0)`, `log(1)`,
//! `sin(0)`, `cos(0)` and the Annex F specials) are handled before the loop;
//! by the Lindemann–Weierstrass theorem every other finite input has a
//! transcendental, hence non-dyadic, result, so the loop terminates.
//!
//! This is a multiprecision evaluator, not a port of CORE-MATH: binding
//! CORE-MATH's C sources needs a C toolchain in every build (and MSVC lacks the
//! GCC builtins it uses), and the pure-Rust ports we found do not claim
//! correct rounding on every input. The cost is speed (microseconds per
//! call, allocation of big integers); see the crate report for measured
//! timings. Tests compare against 400-bit `mpmath` references and pin a
//! cross-architecture output hash.

// The port-backed build calls the Vesper port (vesper/src/crmath.ves over
// vesper/src/bignum.ves); the seed build calls the frozen Rust evaluator in
// seed/src/engine/crmath.rs (SH6 lane 9, #1404).

#[cfg(vesper_port)]
use std::cell::RefCell;

/// Symbols under which Vesper code calls these functions. The DetFloat
/// auditor accepts calls to them (see
/// [`crate::detfloat::DetFloatPolicy::with_correctly_rounded_math`]).
pub const SYMBOLS: &[&str] = &[
    "vesper_crmath_sin",
    "vesper_crmath_cos",
    "vesper_crmath_atan2",
    "vesper_crmath_exp",
    "vesper_crmath_log",
    "vesper_crmath_sinf",
    "vesper_crmath_cosf",
    "vesper_crmath_atan2f",
    "vesper_crmath_expf",
    "vesper_crmath_logf",
];

/// How often this thread's calls hit the evaluator's precision cap
/// (expected: never). The port does not count, so its build reports 0.
pub fn precision_cap_hits() -> u64 {
    #[cfg(vesper_port)]
    {
        0
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::crmath::precision_cap_hits()
    }
}

// The port keeps pi and ln 2 in a per-thread cache this module lends it
// (the layout is crmath.ves `constant`'s).
#[cfg(vesper_port)]
thread_local! {
    // 3 header words, then pi and ln 2 at up to 1,100 limbs each: enough for
    // the widest reduction at the precision cap (a 32,768-bit constant).
    static PORT_CACHE: RefCell<Vec<u32>> = RefCell::new(vec![0; 3 + 2 * 1100]);
}

#[cfg(vesper_port)]
fn with_cache<T>(f: impl FnOnce(&mut [u32]) -> T) -> T {
    PORT_CACHE.with(|c| f(&mut c.borrow_mut()))
}

macro_rules! pair {
    ($f64name:ident, $f32name:ident, $port64:ident, $port32:ident, $doc:literal) => {
        #[doc = concat!("Correctly rounded ", $doc, " (binary64).")]
        pub fn $f64name(x: f64) -> f64 {
            #[cfg(vesper_port)]
            {
                with_cache(|c| crate::port::$port64(x, c))
            }
            #[cfg(not(vesper_port))]
            {
                crate::engine::crmath::$f64name(x)
            }
        }
        #[doc = concat!("Correctly rounded ", $doc, " (binary32).")]
        pub fn $f32name(x: f32) -> f32 {
            #[cfg(vesper_port)]
            {
                f32::from_bits(with_cache(|c| crate::port::$port32(x.to_bits(), c)))
            }
            #[cfg(not(vesper_port))]
            {
                crate::engine::crmath::$f32name(x)
            }
        }
    };
}

pair!(exp, expf, crmath_exp, crmath_expf, "`e^x`");
pair!(log, logf, crmath_log, crmath_logf, "natural logarithm");
pair!(sin, sinf, crmath_sin, crmath_sinf, "sine");
pair!(cos, cosf, crmath_cos, crmath_cosf, "cosine");

/// Correctly rounded `atan2(y, x)` (binary64), C99 Annex F special values.
pub fn atan2(y: f64, x: f64) -> f64 {
    #[cfg(vesper_port)]
    {
        with_cache(|c| crate::port::crmath_atan2(y, x, c))
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::crmath::atan2(y, x)
    }
}

/// Correctly rounded `atan2(y, x)` (binary32), C99 Annex F special values.
pub fn atan2f(y: f32, x: f32) -> f32 {
    #[cfg(vesper_port)]
    {
        f32::from_bits(with_cache(|c| {
            crate::port::crmath_atan2f(y.to_bits(), x.to_bits(), c)
        }))
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::crmath::atan2f(y, x)
    }
}
