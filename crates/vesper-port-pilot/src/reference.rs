//! The Rust implementation, kept as the reference for the port.
use crate::{ArithOverflow, Point, RatioError, RuntimeError, Shape};

/// Greatest common divisor (Euclid); `gcd(0, 0) == 0`.
pub fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// `x` limited to `lo..=hi` (`lo` wins when `lo > hi`).
pub fn clamp(x: i64, lo: i64, hi: i64) -> i64 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

/// `(min, max)` of the two values.
pub fn bounds(a: i64, b: i64) -> (i64, i64) {
    if a <= b { (a, b) } else { (b, a) }
}

/// The midpoint, rounding toward zero; the sums are checked.
pub fn midpoint(p: Point, q: Point) -> Result<Point, ArithOverflow> {
    let half = |a: i64, b: i64| a.checked_add(b).map(|s| s / 2).ok_or(ArithOverflow);
    Ok(Point {
        x: half(p.x, q.x)?,
        y: half(p.y, q.y)?,
    })
}

/// The area, with checked multiplication.
pub fn area(s: Shape) -> Result<i64, ArithOverflow> {
    match s {
        Shape::Empty => Ok(0),
        Shape::Square(n) => n.checked_mul(n).ok_or(ArithOverflow),
        Shape::Rect { w, h } => w.checked_mul(h).ok_or(ArithOverflow),
    }
}

/// Integer division rounding toward zero.
pub fn ratio(a: i64, b: i64) -> Result<i64, RatioError> {
    if b == 0 {
        return Err(RatioError::RuntimeError(RuntimeError::DivisionByZero));
    }
    a.checked_div(b)
        .ok_or(RatioError::ArithOverflow(ArithOverflow))
}

/// Whether `c` is an ASCII digit.
pub fn is_digit(c: u8) -> bool {
    in_range(c, b'0', b'9')
}

/// A private helper: the in-crate unit test below reaches it, so that test
/// stays Rust-only (see the unit-test rule in the porting recipe).
fn in_range(c: u8, lo: u8, hi: u8) -> bool {
    c >= lo && c <= hi
}

/// `a` if it is `Some`, else `b`.
pub fn first_some(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    a.or(b)
}

/// Steps of the Collatz map from `n` down to 1 (0 for `n <= 1`), with
/// checked arithmetic.
pub fn collatz_steps(n: u64) -> Result<u64, ArithOverflow> {
    let (mut n, mut steps) = (n, 0u64);
    while n > 1 {
        n = if n % 2 == 0 {
            n / 2
        } else {
            n.checked_mul(3)
                .and_then(|m| m.checked_add(1))
                .ok_or(ArithOverflow)?
        };
        steps = steps.checked_add(1).ok_or(ArithOverflow)?;
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    #[test]
    fn in_range_is_inclusive() {
        assert!(super::in_range(5, 5, 5));
        assert!(!super::in_range(6, 0, 5));
    }
}
