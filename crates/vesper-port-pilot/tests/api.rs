//! The pilot's API tests. They use only the public API, so they run
//! unchanged against the Rust build and, with `--features vesper-port`,
//! against the Vesper build.
use vesper_port_pilot::*;

#[test]
fn reports_its_implementation() {
    let expected = if cfg!(feature = "vesper-port") {
        "vesper"
    } else {
        "rust"
    };
    assert_eq!(IMPLEMENTATION, expected);
}

#[test]
fn gcd_matches_euclid() {
    assert_eq!(gcd(0, 0), 0);
    assert_eq!(gcd(12, 18), 6);
    assert_eq!(gcd(17, 5), 1);
    assert_eq!(gcd(u64::MAX, 3), 3);
    assert_eq!(gcd(1 << 40, 1 << 20), 1 << 20);
}

#[test]
fn clamp_and_bounds() {
    assert_eq!(clamp(5, 0, 10), 5);
    assert_eq!(clamp(-5, 0, 10), 0);
    assert_eq!(clamp(50, 0, 10), 10);
    assert_eq!(clamp(i64::MIN, i64::MIN, i64::MAX), i64::MIN);
    assert_eq!(bounds(3, -4), (-4, 3));
    assert_eq!(bounds(i64::MAX, i64::MIN), (i64::MIN, i64::MAX));
}

#[test]
fn structs_cross_the_boundary() {
    let p = Point { x: 2, y: -8 };
    let q = Point { x: 10, y: 3 };
    assert_eq!(midpoint(p, q), Ok(Point { x: 6, y: -2 }));
    let far = Point { x: i64::MAX, y: 0 };
    assert_eq!(midpoint(far, far), Err(ArithOverflow));
}

#[test]
fn enums_cross_the_boundary() {
    assert_eq!(area(Shape::Empty), Ok(0));
    assert_eq!(area(Shape::Square(-7)), Ok(49));
    assert_eq!(area(Shape::Rect { w: 3, h: 5 }), Ok(15));
    assert_eq!(area(Shape::Square(1 << 32)), Err(ArithOverflow));
}

#[test]
fn several_fail_types_map_to_one_error_enum() {
    assert_eq!(ratio(7, 2), Ok(3));
    assert_eq!(ratio(-7, 2), Ok(-3));
    assert_eq!(
        ratio(1, 0),
        Err(RatioError::RuntimeError(RuntimeError::DivisionByZero))
    );
    assert_eq!(
        ratio(i64::MIN, -1),
        Err(RatioError::ArithOverflow(ArithOverflow))
    );
}

#[test]
fn narrow_integer_kinds() {
    assert_eq!(add_i8(-100, -28), Ok(i8::MIN));
    assert_eq!(add_i8(-100, -29), Err(ArithOverflow));
    assert_eq!(add_i8(100, 27), Ok(i8::MAX));
    assert_eq!(add_i8(-1, -1), Ok(-2));
    assert_eq!(sum_u16(65_000, 535), Ok(u16::MAX));
    assert_eq!(sum_u16(65_000, 536), Err(ArithOverflow));
}

#[test]
fn bytes_and_booleans() {
    for c in 0..=255u8 {
        assert_eq!(is_digit(c), c.is_ascii_digit(), "byte {c}");
    }
}

#[test]
fn options_cross_the_boundary() {
    assert_eq!(first_some(Some(1), Some(2)), Some(1));
    assert_eq!(first_some(None, Some(u32::MAX)), Some(u32::MAX));
    assert_eq!(first_some(None, None), None);
}

#[test]
fn recursion_and_checked_arithmetic() {
    assert_eq!(collatz_steps(1), Ok(0));
    assert_eq!(collatz_steps(27), Ok(111));
    assert_eq!(collatz_steps(u64::MAX), Err(ArithOverflow));
}
