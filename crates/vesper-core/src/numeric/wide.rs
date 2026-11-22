//! `i128` and `u128` (SH2 lane 2, #1174): the 128-bit half of
//! `vesper.arith.checked/1`, on two's-complement bit patterns.
//!
//! The exact `i128` domain of [`super::IntegerKind`] holds every value of
//! every kind but the `u128` values above `i128::MAX`, so the checker's
//! facts stay in it and treat those as unknown. Execution needs the full
//! range: the evaluator, the JIT and native code carry a 128-bit value as
//! its bit pattern, and every operation here takes and returns patterns
//! (`u128`), read as `i128` for the signed kind. The rules are the ones of
//! the module docs of [`super`], at width 128.
use super::{ArithFailure, Conversion, IntegerKind};
use std::cmp::Ordering;

/// Checked `+`, `-`, `*`, `/` and `%` of `kind` (`i128` or `u128`).
pub fn checked(kind: IntegerKind, op: &str, a: u128, b: u128) -> Result<u128, ArithFailure> {
    use ArithFailure::{DivisionByZero, Overflow};
    if matches!(op, "/" | "%") && b == 0 {
        return Err(DivisionByZero);
    }
    if kind.signed() {
        let (a, b) = (a as i128, b as i128);
        let r = match op {
            "+" => a.checked_add(b),
            "-" => a.checked_sub(b),
            "*" => a.checked_mul(b),
            "/" => a.checked_div(b),
            // MIN % -1 is 0, as for every signed kind.
            "%" => Some(a.wrapping_rem(b)),
            _ => None,
        };
        r.map(|r| r as u128).ok_or(Overflow)
    } else {
        match op {
            "+" => a.checked_add(b),
            "-" => a.checked_sub(b),
            "*" => a.checked_mul(b),
            "/" => Some(a / b),
            "%" => Some(a % b),
            _ => None,
        }
        .ok_or(Overflow)
    }
}
/// Checked value-level negation `-a`.
pub fn checked_neg(kind: IntegerKind, a: u128) -> Option<u128> {
    if kind.signed() {
        (a as i128).checked_neg().map(|r| r as u128)
    } else {
        (a == 0).then_some(0)
    }
}
/// `wrapping_add`, `wrapping_sub` and `wrapping_mul` (the operator `op`):
/// the same pattern for both kinds.
pub fn wrapping(op: &str, a: u128, b: u128) -> u128 {
    match op {
        "+" => a.wrapping_add(b),
        "-" => a.wrapping_sub(b),
        _ => a.wrapping_mul(b),
    }
}
/// `&`, `|` and `^`.
pub fn bit(op: &str, a: u128, b: u128) -> Option<u128> {
    Some(match op {
        "&" => a & b,
        "|" => a | b,
        "^" => a ^ b,
        _ => return None,
    })
}
/// `<<` and `>>` by an amount already checked to be in `0..128`.
pub fn shift(kind: IntegerKind, op: &str, a: u128, amount: u32) -> Result<u128, ArithFailure> {
    if amount >= 128 {
        return Err(ArithFailure::Overflow);
    }
    Ok(match op {
        "<<" => a << amount,
        ">>" if kind.unsigned() => a >> amount,
        ">>" => ((a as i128) >> amount) as u128,
        _ => return Err(ArithFailure::Overflow),
    })
}
/// `rotate_left` / `rotate_right` by `amount` modulo 128.
pub fn rotate(left: bool, a: u128, amount: i128) -> u128 {
    let n = amount.rem_euclid(128) as u32;
    if left {
        a.rotate_left(n)
    } else {
        a.rotate_right(n)
    }
}
/// Order of two values of `kind`.
pub fn compare(kind: IntegerKind, a: u128, b: u128) -> Ordering {
    if kind.signed() {
        (a as i128).cmp(&(b as i128))
    } else {
        a.cmp(&b)
    }
}
/// An integer value of any kind, exactly: its bit pattern sign-extended (or
/// zero-extended) to 128 bits, and whether it is negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exact {
    pub pattern: u128,
    pub negative: bool,
}
impl Exact {
    /// The value whose pattern in `kind` is `pattern` (only the kind's low
    /// bits are read).
    pub fn of(kind: IntegerKind, pattern: u128) -> Self {
        let bits = kind.bits();
        let low = if bits == 128 {
            pattern
        } else {
            pattern & ((1u128 << bits) - 1)
        };
        if kind.signed() {
            let shift = 128 - bits;
            let extended = (((low << shift) as i128) >> shift) as u128;
            Self {
                pattern: extended,
                negative: (extended as i128) < 0,
            }
        } else {
            Self {
                pattern: low,
                negative: false,
            }
        }
    }
    /// The value `n` of the exact `i128` domain.
    pub fn from_i128(n: i128) -> Self {
        Self {
            pattern: n as u128,
            negative: n < 0,
        }
    }
    /// The value in the exact `i128` domain, if it is there.
    pub fn to_i128(self) -> Option<i128> {
        (self.negative || self.pattern <= i128::MAX as u128).then_some(self.pattern as i128)
    }
    /// Whether the value is one of `kind`'s.
    pub fn fits(self, kind: IntegerKind) -> bool {
        if self.negative {
            kind.signed() && (self.pattern as i128) >= kind.min()
        } else {
            self.pattern <= kind.max_u128()
        }
    }
    /// The value's pattern in `kind` modulo `2^bits(kind)`, zero-extended.
    pub fn wrap(self, kind: IntegerKind) -> u128 {
        let bits = kind.bits();
        if bits == 128 {
            self.pattern
        } else {
            self.pattern & ((1u128 << bits) - 1)
        }
    }
}
/// The explicit conversion `conversion` of the value `from` to `to`, as
/// `to`'s pattern (zero-extended).
pub fn convert(conversion: Conversion, to: IntegerKind, from: Exact) -> Result<u128, ArithFailure> {
    match conversion {
        Conversion::Wrapping => Ok(from.wrap(to)),
        _ if from.fits(to) => Ok(from.wrap(to)),
        _ => Err(ArithFailure::Overflow),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use IntegerKind as K;
    #[test]
    fn checked_corners() {
        let min = i128::MIN as u128;
        let neg1 = u128::MAX;
        assert_eq!(
            checked(K::I128, "/", min, neg1),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(checked(K::I128, "%", min, neg1), Ok(0));
        assert_eq!(
            checked(K::I128, "+", i128::MAX as u128, 1),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(
            checked(K::U128, "+", u128::MAX, 1),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(checked(K::U128, "-", 0, 1), Err(ArithFailure::Overflow));
        assert_eq!(checked(K::U128, "*", 1 << 64, 1 << 63), Ok(1 << 127));
        assert_eq!(
            checked(K::U128, "*", 1 << 64, 1 << 64),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(
            checked(K::I128, "*", min, neg1),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(
            checked(K::U128, "/", 1, 0),
            Err(ArithFailure::DivisionByZero)
        );
        assert_eq!(checked_neg(K::I128, min), None);
        assert_eq!(checked_neg(K::U128, 0), Some(0));
        assert_eq!(checked_neg(K::U128, 1), None);
    }
    #[test]
    fn shifts_rotations_and_order() {
        assert_eq!(shift(K::I128, ">>", i128::MIN as u128, 127), Ok(u128::MAX));
        assert_eq!(shift(K::U128, ">>", 1 << 127, 127), Ok(1));
        assert_eq!(shift(K::U128, "<<", 1, 128), Err(ArithFailure::Overflow));
        assert_eq!(rotate(true, 1 << 127, 1), 1);
        assert_eq!(rotate(false, 1, -1), 2);
        assert_eq!(compare(K::I128, u128::MAX, 0), Ordering::Less);
        assert_eq!(compare(K::U128, u128::MAX, 0), Ordering::Greater);
    }
    #[test]
    fn conversions() {
        let minus_one = Exact::of(K::I64, u64::MAX as u128);
        assert!(minus_one.negative);
        assert_eq!(
            convert(Conversion::Widen, K::I128, minus_one),
            Ok(u128::MAX)
        );
        assert_eq!(
            convert(Conversion::Narrow, K::U128, minus_one),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(
            convert(Conversion::Wrapping, K::U128, minus_one),
            Ok(u128::MAX)
        );
        let top = Exact::of(K::U128, u128::MAX);
        assert_eq!(top.to_i128(), None);
        assert_eq!(
            convert(Conversion::Wrapping, K::U64, top),
            Ok(u64::MAX as u128)
        );
        assert_eq!(convert(Conversion::Wrapping, K::I128, top), Ok(u128::MAX));
        assert_eq!(
            convert(Conversion::Narrow, K::I128, top),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(
            convert(Conversion::Narrow, K::U64, Exact::of(K::I128, 5)),
            Ok(5)
        );
        assert!(K::U64.widens_to(K::U128));
        assert!(K::I64.widens_to(K::I128));
        assert!(K::U64.widens_to(K::I128));
        assert!(!K::U128.widens_to(K::I128));
        assert!(!K::I128.widens_to(K::U128));
        assert!(!K::I8.widens_to(K::U128));
        assert_eq!(K::I128.min(), i128::MIN);
        assert_eq!(K::U128.max_u128(), u128::MAX);
    }
}
