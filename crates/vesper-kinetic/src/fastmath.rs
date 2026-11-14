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

// The polynomial coefficients are fdlibm's published values, written with
// all their digits.
#![allow(clippy::excessive_precision, clippy::approx_constant)]

use crate::crmath;

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

#[inline]
fn high(x: f64) -> u32 {
    (x.to_bits() >> 32) as u32
}

#[inline]
fn with_high(x: f64, hi: u32) -> f64 {
    f64::from_bits((u64::from(hi) << 32) | (x.to_bits() & 0xffff_ffff))
}

/// `2^k` for `-1022 <= k <= 1023`.
#[inline]
fn pow2(k: i32) -> f64 {
    f64::from_bits(((k + 1023) as u64) << 52)
}

/// Round to the nearest integer, ties to even, for `|x| < 2^51`: the
/// classic `x + 1.5·2^52 − 1.5·2^52`, two correctly rounded additions.
#[inline]
fn rint(x: f64) -> f64 {
    const SHIFT: f64 = 6_755_399_441_055_744.0;
    (x + SHIFT) - SHIFT
}

// ----- exp -----------------------------------------------------------------

const LN2_HI: f64 = 6.93147180369123816490e-01;
const LN2_LO: f64 = 1.90821492927058770002e-10;
const INV_LN2: f64 = 1.44269504088896338700e+00;

/// `e^x` (binary64), within [`F64_MAX_ULP`] of the correctly rounded value.
pub fn exp(x: f64) -> f64 {
    const P1: f64 = 1.66666666666666019037e-01;
    const P2: f64 = -2.77777777770155933842e-03;
    const P3: f64 = 6.61375632143793436117e-05;
    const P4: f64 = -1.65339022054652515390e-06;
    const P5: f64 = 4.13813679705723846039e-08;
    const O_THRESHOLD: f64 = 7.09782712893383973096e+02;
    const U_THRESHOLD: f64 = -7.45133219101941108420e+02;
    if x.is_nan() {
        return x + x;
    }
    if x > O_THRESHOLD {
        return f64::INFINITY;
    }
    if x < U_THRESHOLD {
        return 0.0;
    }
    let ax = x.abs();
    if ax < 3.725_290_298_461_914e-9 {
        // |x| < 2^-28: e^x = 1 + x to within half an ULP.
        return 1.0 + x;
    }
    let (hi, lo, k) = if ax <= 0.5 * LN2_HI {
        (x, 0.0, 0i32)
    } else {
        let k = rint(x * INV_LN2);
        // k·LN2_HI is exact: LN2_HI has 32 trailing zero bits and |k| < 2^11.
        (x - k * LN2_HI, k * LN2_LO, k as i32)
    };
    let r = hi - lo;
    let t = r * r;
    let c = r - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    let y = 1.0 - ((lo - (r * c) / (2.0 - c)) - hi);
    if k == 0 {
        y
    } else if k >= -1021 {
        if k == 1024 {
            y * 2.0 * pow2(1023)
        } else {
            y * pow2(k)
        }
    } else {
        // Subnormal result: scale in two steps.
        y * pow2(k + 1000) * pow2(-1000)
    }
}

// ----- log -----------------------------------------------------------------

/// Natural logarithm (binary64), within [`F64_MAX_ULP`] of the correctly
/// rounded value.
pub fn log(x: f64) -> f64 {
    const TWO54: f64 = 1.80143985094819840000e+16;
    const LG1: f64 = 6.666666666666735130e-01;
    const LG2: f64 = 3.999999999940941908e-01;
    const LG3: f64 = 2.857142874366239149e-01;
    const LG4: f64 = 2.222219843214978396e-01;
    const LG5: f64 = 1.818357216161805012e-01;
    const LG6: f64 = 1.531383769920937332e-01;
    const LG7: f64 = 1.479819860511658591e-01;
    if x.is_nan() {
        return x + x;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x < 0.0 {
        return f64::NAN;
    }
    if x == f64::INFINITY {
        return x;
    }
    let mut x = x;
    let mut k: i32 = 0;
    let mut hx = high(x) as i32;
    if hx < 0x0010_0000 {
        // Subnormal: scale up by 2^54.
        k -= 54;
        x *= TWO54;
        hx = high(x) as i32;
    }
    k += (hx >> 20) - 1023;
    hx &= 0x000f_ffff;
    let i = (hx + 0x95f64) & 0x10_0000;
    // Normalise x or x/2 into [sqrt(2)/2, sqrt(2)).
    x = with_high(x, (hx | (i ^ 0x3ff0_0000)) as u32);
    k += i >> 20;
    let f = x - 1.0;
    let dk = f64::from(k);
    if (0x000f_ffff & (2 + hx)) < 3 {
        // -2^-20 <= f < 2^-20.
        if f == 0.0 {
            return if k == 0 {
                0.0
            } else {
                dk * LN2_HI + dk * LN2_LO
            };
        }
        let r = f * f * (0.5 - 0.333_333_333_333_333_33 * f);
        return if k == 0 {
            f - r
        } else {
            dk * LN2_HI - ((r - dk * LN2_LO) - f)
        };
    }
    let s = f / (2.0 + f);
    let z = s * s;
    let mut i = hx - 0x6147a;
    let w = z * z;
    let j = 0x6b851 - hx;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    i |= j;
    let r = t2 + t1;
    if i > 0 {
        let hfsq = 0.5 * f * f;
        if k == 0 {
            f - (hfsq - s * (hfsq + r))
        } else {
            dk * LN2_HI - ((hfsq - (s * (hfsq + r) + dk * LN2_LO)) - f)
        }
    } else if k == 0 {
        f - s * (f - r)
    } else {
        dk * LN2_HI - ((s * (f - r) - dk * LN2_LO) - f)
    }
}

// ----- sin, cos --------------------------------------------------------------

/// fdlibm `__kernel_sin` on `[-π/4, π/4]`; `x + y` is the reduced argument.
#[inline]
fn k_sin(x: f64, y: f64, has_tail: bool) -> f64 {
    const S1: f64 = -1.66666666666666324348e-01;
    const S2: f64 = 8.33333333332248946124e-03;
    const S3: f64 = -1.98412698298579493134e-04;
    const S4: f64 = 2.75573137070700676789e-06;
    const S5: f64 = -2.50507602534068634195e-08;
    const S6: f64 = 1.58969099521155010221e-10;
    let z = x * x;
    let w = z * z;
    let r = S2 + z * (S3 + z * S4) + z * w * (S5 + z * S6);
    let v = z * x;
    if !has_tail {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (0.5 * y - v * r) - y) - v * S1)
    }
}

/// fdlibm `__kernel_cos` on `[-π/4, π/4]`.
#[inline]
fn k_cos(x: f64, y: f64) -> f64 {
    const C1: f64 = 4.16666666666666019037e-02;
    const C2: f64 = -1.38888888888741095749e-03;
    const C3: f64 = 2.48015872894767294178e-05;
    const C4: f64 = -2.75573143513906633035e-07;
    const C5: f64 = 2.08757232129817482790e-09;
    const C6: f64 = -1.13596475577881948265e-11;
    let z = x * x;
    let w = z * z;
    let r = z * (C1 + z * (C2 + z * C3)) + w * w * (C4 + z * (C5 + z * C6));
    let hz = 0.5 * z;
    let w = 1.0 - hz;
    w + (((1.0 - w) - hz) + (z * r - x * y))
}

/// Cody–Waite reduction `x = n·π/2 + (y0 + y1)` for `|x| < 2^20·π/2`
/// (fdlibm's "medium" `__ieee754_rem_pio2`, three-part π/2).
fn rem_pio2_medium(x: f64) -> (i32, f64, f64) {
    const INV_PIO2: f64 = 6.36619772367581382433e-01;
    const PIO2_1: f64 = 1.57079632673412561417e+00;
    const PIO2_1T: f64 = 6.07710050650619224932e-11;
    const PIO2_2: f64 = 6.07710050630396597660e-11;
    const PIO2_2T: f64 = 2.02226624879595063154e-21;
    const PIO2_3: f64 = 2.02226624871116645580e-21;
    const PIO2_3T: f64 = 8.47842766036889956997e-32;
    let fnum = rint(x * INV_PIO2);
    let n = fnum as i32;
    let mut r = x - fnum * PIO2_1;
    let mut w = fnum * PIO2_1T;
    let j = ((high(x) >> 20) & 0x7ff) as i32;
    let mut y0 = r - w;
    let exp_of = |v: f64| ((high(v) >> 20) & 0x7ff) as i32;
    if j - exp_of(y0) > 16 {
        // Second iteration, good to 118 bits.
        let t = r;
        w = fnum * PIO2_2;
        r = t - w;
        w = fnum * PIO2_2T - ((t - r) - w);
        y0 = r - w;
        if j - exp_of(y0) > 49 {
            // Third iteration, 151 bits.
            let t = r;
            w = fnum * PIO2_3;
            r = t - w;
            w = fnum * PIO2_3T - ((t - r) - w);
            y0 = r - w;
        }
    }
    let y1 = (r - y0) - w;
    (n, y0, y1)
}

/// `|x|` below which the fast reduction is used (`2^20·π/2`).
const MEDIUM_LIMIT: f64 = 1_647_099.0;

/// Sine (binary64), within [`F64_MAX_ULP`] of the correctly rounded value.
pub fn sin(x: f64) -> f64 {
    if !x.is_finite() {
        // NaN for ±∞, the input's own NaN otherwise (as fdlibm does).
        #[allow(clippy::eq_op)]
        return x - x;
    }
    let ax = x.abs();
    if ax <= 0.785_398_163_397_448_3 {
        if ax < 7.450_580_596_923_828e-9 {
            // |x| < 2^-27: sin x = x to within half an ULP.
            return x;
        }
        return k_sin(x, 0.0, false);
    }
    if ax >= MEDIUM_LIMIT {
        return crmath::sin(x);
    }
    let (n, y0, y1) = rem_pio2_medium(x);
    match n & 3 {
        0 => k_sin(y0, y1, true),
        1 => k_cos(y0, y1),
        2 => -k_sin(y0, y1, true),
        _ => -k_cos(y0, y1),
    }
}

/// Cosine (binary64), within [`F64_MAX_ULP`] of the correctly rounded value.
pub fn cos(x: f64) -> f64 {
    if !x.is_finite() {
        // NaN for ±∞, the input's own NaN otherwise (as fdlibm does).
        #[allow(clippy::eq_op)]
        return x - x;
    }
    let ax = x.abs();
    if ax <= 0.785_398_163_397_448_3 {
        if ax < 7.450_580_596_923_828e-9 {
            return 1.0;
        }
        return k_cos(x, 0.0);
    }
    if ax >= MEDIUM_LIMIT {
        return crmath::cos(x);
    }
    let (n, y0, y1) = rem_pio2_medium(x);
    match n & 3 {
        0 => k_cos(y0, y1),
        1 => -k_sin(y0, y1, true),
        2 => -k_cos(y0, y1),
        _ => k_sin(y0, y1, true),
    }
}

// ----- atan, atan2 -------------------------------------------------------------

const ATAN_HI: [f64; 4] = [
    4.63647609000806093515e-01,
    7.85398163397448278999e-01,
    9.82793723247329054082e-01,
    1.57079632679489655800e+00,
];
const ATAN_LO: [f64; 4] = [
    2.26987774529616870924e-17,
    3.06161699786838301793e-17,
    1.39033110312309984516e-17,
    6.12323399573676603587e-17,
];
const AT: [f64; 11] = [
    3.33333333333329318027e-01,
    -1.99999999998764832476e-01,
    1.42857142725034663711e-01,
    -1.11111104054623557880e-01,
    9.09088713343650656196e-02,
    -7.69187620504482999495e-02,
    6.66107313738753120669e-02,
    -5.83357013379057348645e-02,
    4.97687799461593236017e-02,
    -3.65315727442169155270e-02,
    1.62858201153657823623e-02,
];

/// fdlibm `atan` (binary64).
fn atan(x: f64) -> f64 {
    let hx = high(x);
    let ix = hx & 0x7fff_ffff;
    let negative = hx >> 31 == 1;
    if ix >= 0x4410_0000 {
        // |x| >= 2^66.
        if x.is_nan() {
            return x + x;
        }
        let z = ATAN_HI[3] + ATAN_LO[3];
        return if negative { -z } else { z };
    }
    let (id, x) = if ix < 0x3fdc_0000 {
        // |x| < 0.4375.
        if ix < 0x3e40_0000 {
            // |x| < 2^-27.
            return x;
        }
        (None, x)
    } else {
        let x = x.abs();
        if ix < 0x3ff3_0000 {
            if ix < 0x3fe6_0000 {
                (Some(0), (2.0 * x - 1.0) / (2.0 + x))
            } else {
                (Some(1), (x - 1.0) / (x + 1.0))
            }
        } else if ix < 0x4003_8000 {
            (Some(2), (x - 1.5) / (1.0 + 1.5 * x))
        } else {
            (Some(3), -1.0 / x)
        }
    };
    let z = x * x;
    let w = z * z;
    let s1 = z * (AT[0] + w * (AT[2] + w * (AT[4] + w * (AT[6] + w * (AT[8] + w * AT[10])))));
    let s2 = w * (AT[1] + w * (AT[3] + w * (AT[5] + w * (AT[7] + w * AT[9]))));
    match id {
        None => x - x * (s1 + s2),
        Some(id) => {
            let z = ATAN_HI[id] - ((x * (s1 + s2) - ATAN_LO[id]) - x);
            if negative { -z } else { z }
        }
    }
}

/// `atan2(y, x)` (binary64), within [`F64_MAX_ULP`] of the correctly rounded
/// value, C99 Annex F special values.
pub fn atan2(y: f64, x: f64) -> f64 {
    const PI_O_4: f64 = 7.8539816339744827900e-01;
    const PI_O_2: f64 = 1.57079632679489655800e+00;
    const PI: f64 = 3.14159265358979311600e+00;
    const PI_LO: f64 = 1.2246467991473531772e-16;
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x == 1.0 {
        return atan(y);
    }
    let y_neg = y.is_sign_negative();
    let x_neg = x.is_sign_negative();
    let m = u8::from(y_neg) | (u8::from(x_neg) << 1);
    if y == 0.0 {
        return match m {
            0 | 1 => y,
            2 => PI,
            _ => -PI,
        };
    }
    if x == 0.0 {
        return if y_neg { -PI_O_2 } else { PI_O_2 };
    }
    if x.is_infinite() {
        return if y.is_infinite() {
            match m {
                0 => PI_O_4,
                1 => -PI_O_4,
                2 => 3.0 * PI_O_4,
                _ => -3.0 * PI_O_4,
            }
        } else {
            match m {
                0 => 0.0,
                1 => -0.0,
                2 => PI,
                _ => -PI,
            }
        };
    }
    if y.is_infinite() {
        return if y_neg { -PI_O_2 } else { PI_O_2 };
    }
    let k = ((high(y) & 0x7fff_ffff) as i32 - (high(x) & 0x7fff_ffff) as i32) >> 20;
    let (z, m) = if k > 60 {
        // |y/x| > 2^60.
        (PI_O_2 + 0.5 * PI_LO, m & 1)
    } else if x_neg && k < -60 {
        // |y|/x < -2^-60.
        (0.0, m)
    } else {
        (atan((y / x).abs()), m)
    };
    match m {
        0 => z,
        1 => -z,
        2 => PI - (z - PI_LO),
        _ => (z - PI_LO) - PI,
    }
}

// ----- binary32 --------------------------------------------------------------

/// `e^x` (binary32), within [`F32_MAX_ULP`] of the correctly rounded value.
pub fn expf(x: f32) -> f32 {
    exp(f64::from(x)) as f32
}

/// Natural logarithm (binary32).
pub fn logf(x: f32) -> f32 {
    log(f64::from(x)) as f32
}

/// Sine (binary32).
pub fn sinf(x: f32) -> f32 {
    sin(f64::from(x)) as f32
}

/// Cosine (binary32).
pub fn cosf(x: f32) -> f32 {
    cos(f64::from(x)) as f32
}

/// `atan2(y, x)` (binary32).
pub fn atan2f(y: f32, x: f32) -> f32 {
    atan2(f64::from(y), f64::from(x)) as f32
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
