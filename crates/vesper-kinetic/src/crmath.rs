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
//! (`num-bigint`) with a rigorous absolute error bound, then rounds both ends
//! of the error interval. When both ends round to the same float, that float
//! is the correctly rounded result (rounding is monotone). Otherwise the
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
//! call, allocation through `num-bigint`); see the crate report for measured
//! timings. Tests compare against 400-bit `mpmath` references and pin a
//! cross-architecture output hash.

use num_bigint::{BigInt, Sign};
use num_traits::{One, Signed, Zero};
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

/// Largest working precision tried before giving up on separating the
/// rounding boundary. Known worst cases for binary64 need under 200 bits; a
/// call that reaches this cap returns the rounding of the interval midpoint
/// (deterministic, but then not proved correctly rounded) and bumps
/// [`precision_cap_hits`].
const MAX_PRECISION: u32 = 1 << 14;

thread_local! {
    static CAP_HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How often this thread's calls hit [`MAX_PRECISION`] (expected: never).
pub fn precision_cap_hits() -> u64 {
    CAP_HITS.with(|c| c.get())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    F32,
    F64,
}

impl Format {
    /// (precision bits incl. hidden bit, emin, emax)
    fn params(self) -> (i64, i64, i64) {
        match self {
            Format::F32 => (24, -126, 127),
            Format::F64 => (53, -1022, 1023),
        }
    }

    fn inf_bits(self) -> u64 {
        match self {
            Format::F32 => 0x7f80_0000,
            Format::F64 => 0x7ff0_0000_0000_0000,
        }
    }

    fn sign_bit(self) -> u64 {
        match self {
            Format::F32 => 0x8000_0000,
            Format::F64 => 0x8000_0000_0000_0000,
        }
    }

    fn nan_bits(self) -> u64 {
        match self {
            Format::F32 => 0x7fc0_0000,
            Format::F64 => 0x7ff8_0000_0000_0000,
        }
    }

    fn one_bits(self) -> u64 {
        match self {
            Format::F32 => 0x3f80_0000,
            Format::F64 => 0x3ff0_0000_0000_0000,
        }
    }
}

/// Correctly round `sign · m · 2^e2` (m ≥ 0) to `fmt`; returns the bits.
fn round_to(negative: bool, m: &BigInt, e2: i64, fmt: Format) -> u64 {
    let sign = if negative { fmt.sign_bit() } else { 0 };
    if m.is_zero() {
        return sign;
    }
    let (p, emin, emax) = fmt.params();
    let len = m.bits() as i64;
    let lead = len - 1 + e2;
    if lead > emax {
        return sign | fmt.inf_bits();
    }
    let mut q = lead.max(emin) - (p - 1);
    let shift = q - e2;
    let mut mant: BigInt = if shift <= 0 {
        m << ((-shift) as u64)
    } else {
        let shift = shift as u64;
        let kept: BigInt = m >> shift;
        let rem: BigInt = m - (&kept << shift);
        let half: BigInt = BigInt::one() << (shift - 1);
        let odd = kept.bit(0);
        if rem > half || (rem == half && odd) {
            kept + 1
        } else {
            kept
        }
    };
    if mant.bits() as i64 > p {
        mant >>= 1u32;
        q += 1;
    }
    let mant_len = mant.bits() as i64;
    if mant.is_zero() {
        return sign;
    }
    if mant_len - 1 + q > emax {
        return sign | fmt.inf_bits();
    }
    let mant_u = u64::try_from(&mant).expect("mantissa fits in 64 bits");
    let frac_bits = (p - 1) as u64;
    if mant_len < p {
        // Subnormal: biased exponent 0, q == emin - (p - 1).
        sign | mant_u
    } else {
        let bias = emax;
        let biased = (q + p - 1 + bias) as u64;
        sign | (biased << frac_bits) | (mant_u & ((1u64 << frac_bits) - 1))
    }
}

/// An exact finite nonzero binary64 value `(-1)^neg · mant · 2^exp`.
#[derive(Clone, Copy, Debug)]
struct Exact {
    neg: bool,
    mant: u64,
    exp: i64,
}

fn decode(x: f64) -> Exact {
    let bits = x.to_bits();
    let neg = bits >> 63 == 1;
    let biased = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    if biased == 0 {
        Exact {
            neg,
            mant: frac,
            exp: -1074,
        }
    } else {
        Exact {
            neg,
            mant: frac | (1u64 << 52),
            exp: biased - 1075,
        }
    }
}

impl Exact {
    /// Exponent of the leading bit: the value lies in `[2^lead, 2^(lead+1))`.
    fn lead(self) -> i64 {
        (64 - i64::from(self.mant.leading_zeros())) - 1 + self.exp
    }

    /// `floor(|x| · 2^f)` (error < 1 unit of `2^-f`).
    fn fixed_abs(self, f: u32) -> BigInt {
        let m = BigInt::from(self.mant);
        let s = self.exp + i64::from(f);
        if s >= 0 { m << (s as u64) } else { m >> ((-s) as u64) }
    }

    /// `|x|` as an exact rational `num / 2^den_shift`, both integers.
    fn magnitude(self) -> (BigInt, i64) {
        (BigInt::from(self.mant), self.exp)
    }
}

/// Floor shift that works for either sign of `s` (`v · 2^s`).
fn shift(v: &BigInt, s: i64) -> BigInt {
    if s >= 0 {
        v << (s as u64)
    } else {
        v >> ((-s) as u64)
    }
}

/// `a · b / 2^f`, truncated toward zero (so repeated products of small
/// values reach zero whatever their sign).
fn mul_fix(a: &BigInt, b: &BigInt, f: u32) -> BigInt {
    let p = a * b;
    if p.sign() == Sign::Minus {
        -((-p) >> f)
    } else {
        p >> f
    }
}

/// `atan(1/k)` scaled by `2^f`, error at most one unit.
fn atan_inv(k: u32, f: u32) -> BigInt {
    const GUARD: u32 = 40;
    let g = f + GUARD;
    let k2 = BigInt::from(u64::from(k) * u64::from(k));
    let mut term: BigInt = (BigInt::one() << g) / BigInt::from(k);
    let mut sum = BigInt::zero();
    let mut n: u64 = 0;
    while !term.is_zero() {
        let t = &term / BigInt::from(2 * n + 1);
        if n % 2 == 0 {
            sum += t;
        } else {
            sum -= t;
        }
        term /= &k2;
        n += 1;
    }
    sum >> GUARD
}

/// `atanh(1/k)` scaled by `2^f`, error at most one unit.
fn atanh_inv(k: u32, f: u32) -> BigInt {
    const GUARD: u32 = 40;
    let g = f + GUARD;
    let k2 = BigInt::from(u64::from(k) * u64::from(k));
    let mut term: BigInt = (BigInt::one() << g) / BigInt::from(k);
    let mut sum = BigInt::zero();
    let mut n: u64 = 0;
    while !term.is_zero() {
        sum += &term / BigInt::from(2 * n + 1);
        term /= &k2;
        n += 1;
    }
    sum >> GUARD
}

struct Constants {
    bits: u32,
    pi: BigInt,
    ln2: BigInt,
}

thread_local! {
    static CONSTANTS: RefCell<Constants> = RefCell::new(Constants {
        bits: 0,
        pi: BigInt::zero(),
        ln2: BigInt::zero(),
    });
}

/// `(π, ln 2)` scaled by `2^f`, each within 3 units.
fn constants(f: u32) -> (BigInt, BigInt) {
    CONSTANTS.with(|c| {
        let mut c = c.borrow_mut();
        if c.bits < f {
            let bits = f.max(256).next_power_of_two();
            // Machin: π = 16·atan(1/5) − 4·atan(1/239); ln 2 = 2·atanh(1/3).
            // Each series (≤ 2 units) is computed 8 bits wider, so after
            // scaling (≤ 40 units) and the shift the cached value is within 2
            // units; the shift below adds at most one more.
            let w = bits + 8;
            let pi = ((atan_inv(5, w) * 16u32) - (atan_inv(239, w) * 4u32)) >> 8u32;
            let ln2 = (atanh_inv(3, w) * 2u32) >> 8u32;
            *c = Constants { bits, pi, ln2 };
        }
        let drop = u64::from(c.bits - f);
        (&c.pi >> drop, &c.ln2 >> drop)
    })
}

/// One evaluation at fraction bits `f`: `(negative, value, error, exponent)`
/// meaning the exact result lies within `(-1)^neg · (value ± error) · 2^exp`.
struct Approx {
    negative: bool,
    value: BigInt,
    error: u64,
    exp: i64,
}

/// Run Ziv's loop: `eval(precision)` must return an interval that shrinks as
/// the precision grows.
fn ziv(fmt: Format, start: u32, mut eval: impl FnMut(u32) -> Approx) -> u64 {
    let mut precision = start;
    loop {
        let a = eval(precision);
        let err = BigInt::from(a.error);
        let lo = &a.value - &err;
        let hi = &a.value + &err;
        if lo.sign() != Sign::Minus {
            let r_lo = round_to(a.negative, &lo, a.exp, fmt);
            let r_hi = round_to(a.negative, &hi, a.exp, fmt);
            if r_lo == r_hi {
                return r_lo;
            }
        }
        if precision >= MAX_PRECISION {
            CAP_HITS.with(|c| c.set(c.get() + 1));
            return round_to(a.negative, &a.value.abs(), a.exp, fmt);
        }
        precision *= 2;
    }
}

fn start_precision(fmt: Format) -> u32 {
    match fmt {
        Format::F32 => 64,
        Format::F64 => 96,
    }
}

// ----- exp ----------------------------------------------------------------

fn exp_bits(x: f64, fmt: Format) -> u64 {
    if x.is_nan() {
        return fmt.nan_bits();
    }
    if x == f64::INFINITY {
        return fmt.inf_bits();
    }
    if x == f64::NEG_INFINITY || x < -1100.0 {
        return 0;
    }
    if x > 1100.0 {
        return fmt.inf_bits();
    }
    if x == 0.0 {
        return fmt.one_bits();
    }
    let e = decode(x);
    // k ≈ x / ln 2 with |x − k·ln 2| ≤ 0.35 + 2^-40; any integer close
    // enough works, so plain binary64 arithmetic (exactly rounded division
    // and `round`, the same on every IEEE-754 machine) chooses it.
    let k = (x / std::f64::consts::LN_2).round() as i64;
    ziv(fmt, start_precision(fmt), |p| {
        let f = p + 16;
        // r = x − k·ln 2 at fraction f. ln 2 is taken 16 bits wider so that
        // |k| ≤ 1600 scales its 3-unit error to under one unit.
        let (_, ln2_wide) = constants(f + 16);
        let mut x_fix = e.fixed_abs(f);
        if e.neg {
            x_fix = -x_fix;
        }
        let kln2 = (ln2_wide * BigInt::from(k)) >> 16u32;
        let r = x_fix - kln2; // error ≤ 1 + 1 + 1 units
        // Taylor series Σ r^n / n!, each term truncated.
        let one = BigInt::one() << f;
        let mut sum = one.clone();
        let mut term = one;
        let mut n: u64 = 1;
        loop {
            term = mul_fix(&term, &r, f) / BigInt::from(n);
            if term.is_zero() {
                break;
            }
            sum += &term;
            n += 1;
        }
        // Truncation: each term carries < 2.5 units (one per truncation plus
        // the shrinking inherited error); the tail after a term truncates to
        // zero is < 3 units; r's 3-unit error is amplified by exp(r) < 1.5.
        // The bound below doubles that.
        let error = 6 * n + 24;
        Approx {
            negative: false,
            value: sum,
            error,
            exp: k - i64::from(f),
        }
    })
}

// ----- log ----------------------------------------------------------------

fn log_bits(x: f64, fmt: Format) -> u64 {
    if x.is_nan() || x < 0.0 {
        return fmt.nan_bits();
    }
    if x == 0.0 {
        return fmt.sign_bit() | fmt.inf_bits();
    }
    if x == f64::INFINITY {
        return fmt.inf_bits();
    }
    if x == 1.0 {
        return 0;
    }
    let ex = decode(x);
    // x = M · 2^(lead − 52) with M ∈ [2^52, 2^53); fold into
    // m = M / 2^52 ∈ [1/√2, √2) and exponent e.
    let lead = ex.lead();
    let top = ex.mant << (ex.mant.leading_zeros() - 11); // normalised M
    let mut e = lead;
    // √2 · 2^52 = 6369051672525772.8…
    let (num, den_shift): (BigInt, u32) = if top > 6_369_051_672_525_772 {
        e += 1;
        (BigInt::from(top), 53)
    } else {
        (BigInt::from(top), 52)
    };
    ziv(fmt, start_precision(fmt), |p| {
        // |log x| ≥ 2^-54 when e == 0 (x within one ulp of 1 still differs
        // from 1 by 2^-53), so 64 extra fraction bits keep relative precision.
        let f = p + 80;
        // z = (m − 1)/(m + 1) = (num − 2^s)/(num + 2^s).
        let base = BigInt::one() << den_shift;
        let z = ((&num - &base) << f) / (&num + &base); // error ≤ 1
        let z2 = mul_fix(&z, &z, f); // error ≤ 2
        let mut power = z.clone();
        let mut sum = z;
        let mut n: u64 = 1;
        loop {
            power = mul_fix(&power, &z2, f);
            if power.is_zero() {
                break;
            }
            sum += &power / BigInt::from(2 * n + 1);
            n += 1;
        }
        let log_m = sum << 1u32;
        // e·ln 2 with ln 2 taken 16 bits wider (|e| ≤ 1100).
        let (_, ln2_wide) = constants(f + 16);
        let eln2 = (ln2_wide * BigInt::from(e)) >> 16u32;
        let total = eln2 + log_m;
        // Series: ≤ 3 units per term (power and division truncations plus
        // the propagated z² error) and ≤ 5 for the tail, all doubled; ln 2
        // term ≤ 2 units. The bound below doubles that again.
        let error = 12 * n + 32;
        Approx {
            negative: total.sign() == Sign::Minus,
            value: total.abs(),
            error,
            exp: -i64::from(f),
        }
    })
}

// ----- sin / cos ----------------------------------------------------------

/// `(sin r, cos r, terms)` by Taylor series at fraction `f`, `|r| ≤ 0.8`.
fn sin_cos_series(r: &BigInt, f: u32) -> (BigInt, BigInt, u64) {
    let one = BigInt::one() << f;
    let r2 = mul_fix(r, r, f);
    // sin
    let mut term = r.clone();
    let mut s = r.clone();
    let mut n: u64 = 1;
    let mut terms = 0;
    loop {
        term = -(mul_fix(&term, &r2, f) / BigInt::from((2 * n) * (2 * n + 1)));
        if term.is_zero() {
            break;
        }
        s += &term;
        n += 1;
        terms += 1;
    }
    // cos
    let mut term = one.clone();
    let mut c = one;
    let mut n: u64 = 1;
    loop {
        term = -(mul_fix(&term, &r2, f) / BigInt::from((2 * n - 1) * (2 * n)));
        if term.is_zero() {
            break;
        }
        c += &term;
        n += 1;
        terms += 1;
    }
    (s, c, terms + 2)
}

fn sin_cos_bits(x: f64, fmt: Format, want_cos: bool) -> u64 {
    if x.is_nan() || x.is_infinite() {
        return fmt.nan_bits();
    }
    if x == 0.0 {
        return if want_cos {
            fmt.one_bits()
        } else if x.is_sign_negative() {
            fmt.sign_bit()
        } else {
            0
        };
    }
    let ex = decode(x);
    let lead = ex.lead();
    ziv(fmt, start_precision(fmt), |p| {
        // Tiny |x|: sin x ≈ x needs fraction bits below x's exponent. A
        // result near a zero of sin/cos for large x is at least 2^-70 away
        // from it for binary64 inputs (Muller's worst case is ≈2^-61), so 96
        // extra bits plus Ziv's doubling cover it.
        let tiny = if lead < 0 { (-lead) as u32 } else { 0 };
        let f = p + 96 + tiny;
        // Reduce: q = round(x / (π/2)), r = x − q·π/2, with π taken wide
        // enough that q·(π error) stays under one unit.
        let int_bits = if lead > 0 { lead as u32 } else { 0 };
        let wide = f + int_bits + 16;
        let (pi_wide, _) = constants(wide);
        let half_pi_wide = pi_wide >> 1u32;
        let x_wide = ex.fixed_abs(wide);
        let q: BigInt = (&x_wide + (&half_pi_wide >> 1u32)) / &half_pi_wide;
        let r_wide = &x_wide - &q * &half_pi_wide;
        let r = shift(&r_wide, -i64::from(int_bits + 16)); // error ≤ 2
        let quadrant = {
            let low = (&q % 4u32).to_u32_digits().1;
            low.first().copied().unwrap_or(0)
        };
        let (s, c, terms) = sin_cos_series(&r, f);
        let (mut val, mut neg) = match (want_cos, quadrant) {
            (false, 0) => (s, false),
            (false, 1) => (c, false),
            (false, 2) => (s, true),
            (false, _) => (c, true),
            (true, 0) => (c, false),
            (true, 1) => (s, true),
            (true, 2) => (c, true),
            (true, _) => (s, false),
        };
        if val.sign() == Sign::Minus {
            val = -val;
            neg = !neg;
        }
        // sin(−x) = −sin x; cos is even.
        if ex.neg && !want_cos {
            neg = !neg;
        }
        // ≤ 2 units per series term, 2 from r (|derivative| ≤ 1), 2 for the
        // tail; the bound below doubles that.
        let error = 4 * terms + 16;
        Approx {
            negative: neg,
            value: val,
            error,
            exp: -i64::from(f),
        }
    })
}

// ----- atan2 --------------------------------------------------------------

fn atan2_bits(y: f64, x: f64, fmt: Format) -> u64 {
    if y.is_nan() || x.is_nan() {
        return fmt.nan_bits();
    }
    let y_neg = y.is_sign_negative();
    let sign = if y_neg { fmt.sign_bit() } else { 0 };
    // Exact multiples of π/4 as (numerator, quarter-π count): computed below.
    let special = |quarters: u32| -> u64 { sign | pi_multiple_bits(quarters, fmt) };
    if y == 0.0 {
        return if x > 0.0 || (x == 0.0 && !x.is_sign_negative()) {
            sign
        } else {
            special(4)
        };
    }
    if y.is_infinite() {
        return if x == f64::INFINITY {
            special(1)
        } else if x == f64::NEG_INFINITY {
            special(3)
        } else {
            special(2)
        };
    }
    if x == 0.0 {
        return special(2);
    }
    if x == f64::INFINITY {
        return sign;
    }
    if x == f64::NEG_INFINITY {
        return special(4);
    }
    let ey = decode(y);
    let ex = decode(x);
    let (ny, sy) = ey.magnitude();
    let (nx, sx) = ex.magnitude();
    // t = |y|/|x| = (ny / nx) · 2^(sy − sx); swap so the ratio is ≤ 1.
    let ly = ey.lead();
    let lx = ex.lead();
    let x_neg = ex.neg;
    let y_le_x = ratio_le_one(ey, ex);
    ziv(fmt, start_precision(fmt), |p| {
        // atan t ≈ t for tiny t: give the fraction room below t's exponent.
        let tiny = (ly - lx).unsigned_abs().saturating_sub(1) as u32;
        let f = p + 32 + tiny.min(2300);
        let (num, den, num_shift, den_shift, inverted) = if y_le_x {
            (&ny, &nx, sy, sx, false)
        } else {
            (&nx, &ny, sx, sy, true)
        };
        // u = num·2^num_shift / (den·2^den_shift) at fraction f, u ≤ 1.
        let s = i64::from(f) + num_shift - den_shift;
        let u = if s >= 0 {
            (num << (s as u64)) / den
        } else {
            num / (den << ((-s) as u64))
        }; // error ≤ 1
        let a = atan_le_one(&u, f); // (value, error)
        let (pi, _) = constants(f);
        let half_pi = &pi >> 1u32;
        let (mut v, mut err) = a;
        if inverted {
            v = &half_pi - v;
            err += 4;
        }
        if x_neg {
            v = &pi - v;
            err += 4;
        }
        Approx {
            negative: y_neg,
            value: v,
            error: err,
            exp: -i64::from(f),
        }
    })
}

/// `|y| ≤ |x|` exactly.
fn ratio_le_one(ey: Exact, ex: Exact) -> bool {
    let (ny, sy) = ey.magnitude();
    let (nx, sx) = ex.magnitude();
    let common = sy.min(sx);
    shift(&ny, sy - common) <= shift(&nx, sx - common)
}

/// `atan u` for `0 ≤ u ≤ 1` (input error ≤ 1 unit), at fraction `f`:
/// `(value, error bound in units)`.
fn atan_le_one(u: &BigInt, f: u32) -> (BigInt, u64) {
    let one = BigInt::one() << f;
    // Two halvings: atan u = 2·atan(u / (1 + √(1 + u²))).
    let mut w = u.clone();
    for _ in 0..2 {
        let w2 = mul_fix(&w, &w, f);
        let root = ((&one + w2) << f).sqrt(); // √(1 + w²) · 2^f, floor
        w = (&w << f) / (&one + root);
    }
    // |w| ≤ tan(π/16) < 0.2; Taylor Σ (−1)^n w^(2n+1)/(2n+1).
    let w2 = mul_fix(&w, &w, f);
    let mut power = w.clone();
    let mut sum = w;
    let mut n: u64 = 1;
    loop {
        power = mul_fix(&power, &w2, f);
        if power.is_zero() {
            break;
        }
        let t = &power / BigInt::from(2 * n + 1);
        if n % 2 == 1 {
            sum -= t;
        } else {
            sum += t;
        }
        n += 1;
    }
    // Each halving maps an input error e to ≤ 0.75·e + 1.5 units (so w is
    // within 3 units); the series adds ≤ 4.5 units per term; the final ×4
    // scales everything. The bound below is about 1.5 times that.
    let error = 4 * (7 * n + 32);
    (sum << 2u32, error)
}

/// `quarters · π/4` correctly rounded (for the Annex F special results).
fn pi_multiple_bits(quarters: u32, fmt: Format) -> u64 {
    ziv(fmt, start_precision(fmt), |p| {
        let f = p + 8;
        let (pi, _) = constants(f + 4);
        Approx {
            negative: false,
            value: (pi * BigInt::from(quarters)) >> 2u32,
            error: 4,
            exp: -i64::from(f + 4),
        }
    })
}

// ----- public API ---------------------------------------------------------

macro_rules! pair {
    ($f64name:ident, $f32name:ident, $bits:ident, $doc:literal) => {
        #[doc = concat!("Correctly rounded ", $doc, " (binary64).")]
        pub fn $f64name(x: f64) -> f64 {
            f64::from_bits($bits(x, Format::F64))
        }
        #[doc = concat!("Correctly rounded ", $doc, " (binary32).")]
        pub fn $f32name(x: f32) -> f32 {
            f32::from_bits($bits(f64::from(x), Format::F32) as u32)
        }
    };
}

pair!(exp, expf, exp_bits, "`e^x`");
pair!(log, logf, log_bits, "natural logarithm");

fn sin_bits(x: f64, fmt: Format) -> u64 {
    sin_cos_bits(x, fmt, false)
}
fn cos_bits(x: f64, fmt: Format) -> u64 {
    sin_cos_bits(x, fmt, true)
}
pair!(sin, sinf, sin_bits, "sine");
pair!(cos, cosf, cos_bits, "cosine");

/// Correctly rounded `atan2(y, x)` (binary64), C99 Annex F special values.
pub fn atan2(y: f64, x: f64) -> f64 {
    f64::from_bits(atan2_bits(y, x, Format::F64))
}

/// Correctly rounded `atan2(y, x)` (binary32), C99 Annex F special values.
pub fn atan2f(y: f32, x: f32) -> f32 {
    f32::from_bits(atan2_bits(f64::from(y), f64::from(x), Format::F32) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_handles_subnormals_ties_and_overflow() {
        let one = BigInt::one();
        assert_eq!(round_to(false, &one, -1074, Format::F64), 1);
        // Half of the smallest subnormal ties to even (zero).
        assert_eq!(round_to(false, &one, -1075, Format::F64), 0);
        assert_eq!(round_to(false, &BigInt::from(3), -1076, Format::F64), 1);
        assert_eq!(round_to(false, &one, 1024, Format::F64), Format::F64.inf_bits());
        assert_eq!(f32::from_bits(round_to(false, &one, 0, Format::F32) as u32), 1.0);
        // 1 + 2^-24 ties to 1 in binary32; 1 + 3·2^-25 rounds up.
        let m = (BigInt::one() << 24u32) + 1;
        assert_eq!(f32::from_bits(round_to(false, &m, -24, Format::F32) as u32), 1.0);
        let m = (BigInt::one() << 25u32) + 3;
        assert_eq!(
            f32::from_bits(round_to(false, &m, -25, Format::F32) as u32),
            1.0 + f32::EPSILON
        );
    }

    #[test]
    fn constants_are_accurate() {
        let (pi, ln2) = constants(60);
        assert_eq!(f64::from_bits(round_to(false, &pi, -60, Format::F64)), std::f64::consts::PI);
        assert_eq!(f64::from_bits(round_to(false, &ln2, -60, Format::F64)), std::f64::consts::LN_2);
    }
}
