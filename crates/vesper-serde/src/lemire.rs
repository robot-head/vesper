//! Decimal to binary64 conversion for validated JSON numbers (§20.3).
//!
//! Three paths, tried in order:
//!
//! 1. **Clinger's fast path**: an exact significand of at most 2^53 and a
//!    power of ten of magnitude at most 22 are both exact doubles, so one
//!    IEEE multiplication or division is correctly rounded.
//! 2. **Eisel–Lemire** (Lemire, "Number Parsing at a Gigabyte per Second",
//!    Software: Practice and Experience 51(8), 2021; with the Mushtak–Lemire
//!    2022 bound): multiply the normalised 64-bit significand by a 128-bit
//!    truncated power of five ([`crate::pow5`]) and round. The product is
//!    either provably correctly rounded or the algorithm reports that it
//!    cannot decide; significands of more than 19 digits are tried as `w`
//!    and `w + 1` and accepted only when both round to the same double.
//! 3. **Fallback**: Rust's `str::parse::<f64>`, which is correctly rounded on
//!    every input (it contains its own big-decimal slow path).
//!
//! The result is therefore always the correctly rounded double, and
//! `tests/float_lemire.rs` checks bit equality with `str::parse` on a large
//! differential corpus, together with how often each path was taken.

use crate::pow5::POW5_128;

/// Which conversion path produced a value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DecodePath {
    /// Significand and power of ten exact as doubles.
    Clinger,
    /// The 128-bit product decided the rounding.
    EiselLemire,
    /// Eisel–Lemire could not decide; `str::parse` did.
    Fallback,
}

/// Exponent magnitudes are clamped here: 2^62, while a slice holds fewer
/// than 2^61 digits on any supported target, so `e - digits` keeps its sign
/// and lies far outside -342..=308.
const EXP_CAP: i64 = 1 << 62;
const SMALLEST_POWER_OF_TEN: i64 = -342;
const LARGEST_POWER_OF_TEN: i64 = 308;
const MANTISSA_BITS: i32 = 52;
const MINIMUM_EXPONENT: i32 = -1023;
const INFINITE_POWER: i32 = 0x7ff;
const MIN_EXPONENT_ROUND_TO_EVEN: i64 = -4;
const MAX_EXPONENT_ROUND_TO_EVEN: i64 = 23;

/// Exact powers of ten representable as doubles.
const POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// A decimal `w * 10^q`, with `truncated` set when digits beyond the first
/// 19 significant ones were dropped from `w`.
#[derive(Clone, Copy, Debug)]
struct Decimal {
    negative: bool,
    w: u64,
    q: i64,
    truncated: bool,
}

/// Parse the JSON number grammar. `None` if `text` is not a JSON number.
///
/// The common case (at most 19 digits) accumulates the significand in one
/// pass; longer spellings re-read their digits to keep the first 19
/// significant ones and note whether anything non-zero was dropped.
fn decimal(text: &[u8]) -> Option<Decimal> {
    let mut p = 0usize;
    let negative = text.first() == Some(&b'-');
    if negative {
        p += 1;
    }
    let digit = |p: usize| {
        text.get(p)
            .and_then(|c| c.checked_sub(b'0'))
            .filter(|d| *d < 10)
    };
    let mut w = 0u64;
    let int_start = p;
    eight_digits(text, &mut p, &mut w);
    while let Some(d) = digit(p) {
        w = w.wrapping_mul(10).wrapping_add(u64::from(d));
        p += 1;
    }
    let int_end = p;
    if int_end == int_start || (int_end - int_start > 1 && text[int_start] == b'0') {
        return None;
    }
    let (mut frac_start, mut frac_end) = (p, p);
    if text.get(p) == Some(&b'.') {
        p += 1;
        frac_start = p;
        eight_digits(text, &mut p, &mut w);
        while let Some(d) = digit(p) {
            w = w.wrapping_mul(10).wrapping_add(u64::from(d));
            p += 1;
        }
        frac_end = p;
        if frac_end == frac_start {
            return None;
        }
    }
    let mut e: i64 = 0;
    if matches!(text.get(p), Some(b'e' | b'E')) {
        p += 1;
        let exp_negative = match text.get(p) {
            Some(b'-') => {
                p += 1;
                true
            }
            Some(b'+') => {
                p += 1;
                false
            }
            _ => false,
        };
        let exp_start = p;
        while let Some(d) = digit(p) {
            // Saturate far beyond any digit count a slice can hold, so the
            // significand's own scale can never cancel the saturation.
            e = e
                .saturating_mul(10)
                .saturating_add(i64::from(d))
                .min(EXP_CAP);
            p += 1;
        }
        if p == exp_start {
            return None;
        }
        if exp_negative {
            e = -e;
        }
    }
    if p != text.len() {
        return None;
    }
    let digits = (int_end - int_start) + (frac_end - frac_start);
    let mut d = Decimal {
        negative,
        w,
        q: e.saturating_sub((frac_end - frac_start) as i64),
        truncated: false,
    };
    if digits > 19 {
        // Leading zeros do not count; re-read when there are too many
        // significant digits for `w` to be exact.
        let all = text[int_start..int_end]
            .iter()
            .chain(&text[frac_start..frac_end]);
        let significant = digits - all.clone().take_while(|c| **c == b'0').count();
        if significant > 19 {
            let mut w = 0u64;
            let mut kept = 0usize;
            let mut truncated = false;
            for &c in all.skip_while(|c| **c == b'0') {
                if kept < 19 {
                    w = w * 10 + u64::from(c - b'0');
                    kept += 1;
                } else if c != b'0' {
                    truncated = true;
                }
            }
            d.w = w;
            // Every significant digit past the 19th moves the scale by one.
            d.q = d.q.saturating_add((significant - 19) as i64);
            d.truncated = truncated;
        }
    }
    Some(d)
}

/// Fold runs of eight ASCII digits into `w` a 64-bit word at a time (SWAR,
/// as in fast_float), leaving any shorter tail to the byte loop.
#[inline]
fn eight_digits(text: &[u8], p: &mut usize, w: &mut u64) {
    while let Some(chunk) = text.get(*p..*p + 8) {
        let v = u64::from_le_bytes(chunk.try_into().expect("eight bytes"));
        // Every byte in 0x30..=0x39: high nibble 3, and adding 6 keeps it 3.
        let all_digits = ((v & 0xf0f0_f0f0_f0f0_f0f0)
            | ((v.wrapping_add(0x0606_0606_0606_0606) & 0xf0f0_f0f0_f0f0_f0f0) >> 4))
            == 0x3333_3333_3333_3333;
        if !all_digits {
            return;
        }
        let v = (v & 0x0f0f_0f0f_0f0f_0f0f).wrapping_mul(2561) >> 8;
        let v = (v & 0x00ff_00ff_00ff_00ff).wrapping_mul(6_553_601) >> 16;
        let v = (v & 0x0000_ffff_0000_ffff).wrapping_mul(42_949_672_960_001) >> 32;
        *w = w.wrapping_mul(100_000_000).wrapping_add(v);
        *p += 8;
    }
}

/// Biased binary exponent and 52-bit mantissa, or `None` when Eisel–Lemire
/// cannot decide the rounding.
fn eisel_lemire(q: i64, w: u64) -> Option<(i32, u64)> {
    if w == 0 || q < SMALLEST_POWER_OF_TEN {
        return Some((0, 0));
    }
    if q > LARGEST_POWER_OF_TEN {
        return Some((INFINITE_POWER, 0));
    }
    let lz = w.leading_zeros();
    let w = w << lz;
    let (lo, hi) = product_approx(q, w, MANTISSA_BITS as u32 + 3);
    if lo == u64::MAX {
        // The truncated low half of the power of five may hide a carry into
        // the bits we keep. Lemire shows this cannot change the result for
        // q in -27..=55; elsewhere give up and fall back.
        let inside_safe_exponent = (-27..=55).contains(&q);
        if !inside_safe_exponent {
            return None;
        }
    }
    let upperbit = (hi >> 63) as i32;
    let shift = upperbit + 64 - MANTISSA_BITS - 3;
    let mut mantissa = hi >> shift;
    let mut power2 = power(q as i32) + upperbit - lz as i32 - MINIMUM_EXPONENT;
    if power2 <= 0 {
        // Subnormal (or zero) result.
        if -power2 + 1 >= 64 {
            return Some((0, 0));
        }
        mantissa >>= -power2 + 1;
        mantissa += mantissa & 1;
        mantissa >>= 1;
        power2 = i32::from(mantissa >= (1u64 << MANTISSA_BITS));
        return Some((power2, mantissa & ((1u64 << MANTISSA_BITS) - 1)));
    }
    // A product that lands exactly halfway between two doubles: round to
    // even. Only possible for small |q|, where 5^q is exact.
    if lo <= 1
        && (MIN_EXPONENT_ROUND_TO_EVEN..=MAX_EXPONENT_ROUND_TO_EVEN).contains(&q)
        && mantissa & 3 == 1
        && (mantissa << shift) == hi
    {
        mantissa &= !1;
    }
    mantissa += mantissa & 1;
    mantissa >>= 1;
    if mantissa >= (2u64 << MANTISSA_BITS) {
        mantissa = 1u64 << MANTISSA_BITS;
        power2 += 1;
    }
    mantissa &= !(1u64 << MANTISSA_BITS);
    if power2 >= INFINITE_POWER {
        return Some((INFINITE_POWER, 0));
    }
    Some((power2, mantissa))
}

/// floor(log2(10^q)) + 63, valid for |q| < 1400 or so.
#[inline]
fn power(q: i32) -> i32 {
    (q.wrapping_mul(152_170 + 65_536) >> 16) + 63
}

/// The high 128 bits of `w * 5^q` to `precision` bits, as `(low, high)`.
#[inline]
fn product_approx(q: i64, w: u64, precision: u32) -> (u64, u64) {
    let mask = u64::MAX >> precision;
    let (hi5, lo5) = POW5_128[(q - SMALLEST_POWER_OF_TEN) as usize];
    let first = u128::from(w) * u128::from(hi5);
    let (mut first_lo, mut first_hi) = (first as u64, (first >> 64) as u64);
    if first_hi & mask == mask {
        // Only then can the low half of the power change the top bits.
        let second_hi = ((u128::from(w) * u128::from(lo5)) >> 64) as u64;
        first_lo = first_lo.wrapping_add(second_hi);
        if second_hi > first_lo {
            first_hi += 1;
        }
    }
    (first_lo, first_hi)
}

fn assemble(negative: bool, (power2, mantissa): (i32, u64)) -> f64 {
    let bits = ((power2 as u64) << MANTISSA_BITS) | mantissa | (u64::from(negative) << 63);
    f64::from_bits(bits)
}

/// Convert a JSON number to the correctly rounded double (possibly
/// infinite or a signed zero), reporting the path taken. `None` if `text`
/// is not a JSON number.
pub fn decode_f64(text: &str) -> Option<(f64, DecodePath)> {
    let d = decimal(text.as_bytes())?;
    let sign = |x: f64| if d.negative { -x } else { x };
    if d.w == 0 {
        return Some((sign(0.0), DecodePath::Clinger));
    }
    if !d.truncated && d.w <= (1u64 << 53) && (-22..=22).contains(&d.q) {
        let w = d.w as f64;
        let value = if d.q < 0 {
            w / POW10[(-d.q) as usize]
        } else {
            w * POW10[d.q as usize]
        };
        return Some((sign(value), DecodePath::Clinger));
    }
    let fast = eisel_lemire(d.q, d.w).and_then(|fp| {
        if d.truncated {
            // The true significand lies in (w, w + 1): both ends must agree.
            let up = eisel_lemire(d.q, d.w + 1)?;
            (up == fp).then_some(fp)
        } else {
            Some(fp)
        }
    });
    if let Some(fp) = fast {
        return Some((assemble(d.negative, fp), DecodePath::EiselLemire));
    }
    let value = text.parse::<f64>().ok()?;
    Some((value, DecodePath::Fallback))
}
