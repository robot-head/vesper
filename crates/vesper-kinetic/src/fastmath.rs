//! Fast deterministic `sin`, `cos`, `atan2`, `exp` and `log` (§36.1 item 1
//! fast path) with a checked error bound against [`crate::crmath`].
//!
//! [`crate::crmath`] is correctly rounded but takes microseconds per call.
//! These functions evaluate the classic Sun fdlibm algorithms (Cody–Waite
//! argument reduction with split constants, minimax polynomials evaluated
//! by Horner's rule, and the fdlibm reconstruction steps) using only IEEE-754
//! binary64 `+ − × ÷`, comparisons, `floor`-free integer conversions and bit
//! manipulation. Rust never contracts `a * b + c` into a fused multiply-add
//! and this module never calls `mul_add`, so every operation is one
//! correctly rounded binary64 operation in a fixed order: the result is the
//! same bits on every IEEE-754 target (x86_64, AArch64, any optimisation
//! level). They are *not* correctly rounded; [`F64_MAX_ULP`] and
//! [`F32_MAX_ULP`] state the bound, and `tests/fastmath.rs` checks it
//! against the correctly rounded path on a dense sample (plus a pinned
//! cross-architecture output hash).
//!
//! Arguments outside the fast reduction's range (`|x| ≥ 2^20·π/2` for `sin`
//! and `cos`) fall back to the correctly rounded path, so the bound holds for
//! every finite input. Special values follow C99 Annex F like
//! [`crate::crmath`].
//!
//! The binary32 functions evaluate the binary64 algorithm on the exactly
//! widened input and round once to binary32, so their error is at most
//! half a binary32 ULP plus the (much smaller) binary64 error; they almost
//! always return the correctly rounded value.

// The port-backed build calls the Vesper port (vesper/src/fastmath.ves);
// the seed build calls the frozen Rust engine in
// seed/src/engine/fastmath.rs.

/// Largest distance, in binary64 ULPs (steps between adjacent floats), from
/// a binary64 function's result to the correctly rounded result.
pub const F64_MAX_ULP: u64 = 1;

/// Largest distance, in binary32 ULPs, from a binary32 function's result to
/// the correctly rounded result.
pub const F32_MAX_ULP: u64 = 1;

/// Symbols under which Vesper code calls these functions (the DetFloat
/// auditor accepts them, see
/// [`crate::detfloat::DetFloatPolicy::with_fast_deterministic_math`]).
pub const SYMBOLS: &[&str] = &[
    "vesper_fastmath_sin",
    "vesper_fastmath_cos",
    "vesper_fastmath_atan2",
    "vesper_fastmath_exp",
    "vesper_fastmath_log",
    "vesper_fastmath_sinf",
    "vesper_fastmath_cosf",
    "vesper_fastmath_atan2f",
    "vesper_fastmath_expf",
    "vesper_fastmath_logf",
];

/// `e^x` (binary64), within [`F64_MAX_ULP`] of the correctly rounded value.
pub fn exp(x: f64) -> f64 {
    #[cfg(vesper_port)]
    {
        crate::port::fastmath_exp(x)
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::fastmath::exp(x)
    }
}

/// Natural logarithm (binary64), within [`F64_MAX_ULP`] of the correctly
/// rounded value.
pub fn log(x: f64) -> f64 {
    #[cfg(vesper_port)]
    {
        crate::port::fastmath_log(x)
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::fastmath::log(x)
    }
}

/// Sine (binary64), within [`F64_MAX_ULP`] of the correctly rounded value.
pub fn sin(x: f64) -> f64 {
    #[cfg(vesper_port)]
    {
        crate::port::fastmath_sin(x)
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::fastmath::sin(x)
    }
}

/// Cosine (binary64), within [`F64_MAX_ULP`] of the correctly rounded value.
pub fn cos(x: f64) -> f64 {
    #[cfg(vesper_port)]
    {
        crate::port::fastmath_cos(x)
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::fastmath::cos(x)
    }
}

/// `atan2(y, x)` (binary64), within [`F64_MAX_ULP`] of the correctly rounded
/// value, C99 Annex F special values.
pub fn atan2(y: f64, x: f64) -> f64 {
    #[cfg(vesper_port)]
    {
        crate::port::fastmath_atan2(y, x)
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::fastmath::atan2(y, x)
    }
}

// ----- binary32 --------------------------------------------------------------

/// `e^x` (binary32), within [`F32_MAX_ULP`] of the correctly rounded value.
pub fn expf(x: f32) -> f32 {
    #[cfg(vesper_port)]
    {
        f32::from_bits(crate::port::fastmath_expf(x.to_bits()))
    }
    #[cfg(not(vesper_port))]
    {
        exp(f64::from(x)) as f32
    }
}

/// Natural logarithm (binary32).
pub fn logf(x: f32) -> f32 {
    #[cfg(vesper_port)]
    {
        f32::from_bits(crate::port::fastmath_logf(x.to_bits()))
    }
    #[cfg(not(vesper_port))]
    {
        log(f64::from(x)) as f32
    }
}

/// Sine (binary32).
pub fn sinf(x: f32) -> f32 {
    #[cfg(vesper_port)]
    {
        f32::from_bits(crate::port::fastmath_sinf(x.to_bits()))
    }
    #[cfg(not(vesper_port))]
    {
        sin(f64::from(x)) as f32
    }
}

/// Cosine (binary32).
pub fn cosf(x: f32) -> f32 {
    #[cfg(vesper_port)]
    {
        f32::from_bits(crate::port::fastmath_cosf(x.to_bits()))
    }
    #[cfg(not(vesper_port))]
    {
        cos(f64::from(x)) as f32
    }
}

/// `atan2(y, x)` (binary32).
pub fn atan2f(y: f32, x: f32) -> f32 {
    #[cfg(vesper_port)]
    {
        f32::from_bits(crate::port::fastmath_atan2f(y.to_bits(), x.to_bits()))
    }
    #[cfg(not(vesper_port))]
    {
        atan2(f64::from(y), f64::from(x)) as f32
    }
}

/// Distance between `a` and `b` in steps between adjacent binary64 values
/// (`0` for equal bits or `±0`; `u64::MAX` if exactly one is NaN).
pub fn ulp_distance(a: f64, b: f64) -> u64 {
    if a.is_nan() || b.is_nan() {
        return if a.is_nan() && b.is_nan() {
            0
        } else {
            u64::MAX
        };
    }
    let ord = |v: f64| -> i128 {
        let bits = v.to_bits() as i64;
        i128::from(if bits < 0 { i64::MIN - bits } else { bits })
    };
    (ord(a) - ord(b)).unsigned_abs() as u64
}

/// [`ulp_distance`] for binary32.
pub fn ulp_distance_f32(a: f32, b: f32) -> u64 {
    if a.is_nan() || b.is_nan() {
        return if a.is_nan() && b.is_nan() {
            0
        } else {
            u64::MAX
        };
    }
    let ord = |v: f32| -> i64 {
        let bits = v.to_bits() as i32;
        i64::from(if bits < 0 { i32::MIN - bits } else { bits })
    };
    (ord(a) - ord(b)).unsigned_abs()
}
