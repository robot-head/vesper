use vesper::{check, native, parser, runtime::Runtime};
fn eval(source: &str, expected: &str) {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let actual = Runtime::new(&c.world).run("main", vec![]).unwrap();
    assert_eq!(actual.to_string(), expected);
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), expected);
    let mut module = p.module;
    vesper::optimize::optimize(&mut module, &c.expression_types);
    let optimized = check::check(&module);
    assert!(
        optimized.diagnostics.is_empty(),
        "optimized: {:?}",
        optimized.diagnostics
    );
    assert_eq!(
        Runtime::new(&optimized.world)
            .run("main", vec![])
            .unwrap()
            .to_string(),
        expected
    );
    assert_eq!(
        native::jit(&optimized.world, "main").unwrap().to_string(),
        expected
    );
}
fn rejects(source: &str) {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(
        !c.diagnostics.is_empty(),
        "accepted invalid numeric program"
    );
}
#[test]
fn byte_wrap() {
    eval("fn main() -> u8 { let x: u8 = 255; x + 2 }", "1");
}
#[test]
fn byte_compound_wrap() {
    eval("fn main() -> u8 { let x: u8 = 255; x += 2; x }", "1");
}
#[test]
fn word_wrap() {
    eval("fn main() -> u32 { let x: u32 = 4294967295; x + 2 }", "1");
}
#[test]
fn unsigned_max() {
    eval(
        "fn main() -> u64 { 18446744073709551615 }",
        "18446744073709551615",
    );
}
#[test]
fn unsigned_wrap() {
    eval("fn main() -> u64 { 18446744073709551615 + 2 }", "1");
}
#[test]
fn unsigned_compare() {
    eval(
        "fn main() -> bool { let x: u64 = 18446744073709551615; x > 0 }",
        "true",
    );
}
#[test]
fn unsigned_divide() {
    eval(
        "fn main() -> u64 { let x: u64 = 18446744073709551615; x / 3 }",
        "6148914691236517205",
    );
}
#[test]
fn unsigned_remainder() {
    eval(
        "fn main() -> u64 { let x: u64 = 18446744073709551615; x % 3 }",
        "0",
    );
}
#[test]
fn signed_min() {
    eval(
        "fn main() -> i64 { -9223372036854775808 }",
        "-9223372036854775808",
    );
}
#[test]
fn signed_wrap() {
    eval(
        "fn main() -> i64 { 9223372036854775807 + 1 }",
        "-9223372036854775808",
    );
}
#[test]
fn byte_argument() {
    eval(
        "fn inc(x: u8) -> u8 { x + 1 } fn main() -> u8 { inc(255) }",
        "0",
    );
}
#[test]
fn literal_left_compare() {
    eval(
        "fn main() -> bool { let x: u64 = 18446744073709551615; 0 < x }",
        "true",
    );
}
#[test]
fn literal_range() {
    rejects("fn main() -> u8 { 256 }");
    rejects("fn main() -> u64 { -1 }");
    rejects("fn main() -> i64 { 9223372036854775808 }");
}
#[test]
fn no_implicit_integer_conversion() {
    rejects("fn main() -> u8 { let x: u64 = 255; x }");
    rejects("fn main() -> u64 { let x: i64 = 1; x + 1 }");
}

#[test]
fn generic_literal_infers_later_byte_argument() {
    eval(
        "fn add[T: mod copy](a: T, b: T) -> T { a } fn main() -> u8 { let x: u8 = 255; add(1, x) }",
        "1",
    );
}
#[test]
fn generic_literals_share_unsigned_range() {
    eval(
        "fn first[T: mod copy](a: T, b: T) -> T { a } fn main() -> u64 { first(1, 18446744073709551615) }",
        "1",
    );
}
#[test]
fn generic_literals_reject_incompatible_signed_ranges() {
    rejects(
        "fn same[T: mod copy](a: T, b: T) -> bool { a == b } fn main() -> bool { same(-1, 18446744073709551615) }",
    );
}
#[test]
fn generic_literal_rechecked_after_later_unsigned_argument() {
    rejects(
        "fn same[T: mod copy](a: T, b: T) -> bool { a == b } fn main() -> bool { let b: u8 = 0; same(-1, b) }",
    );
}
#[test]
fn generic_nested_literal_infers_later_argument() {
    eval(
        "fn same[T: mod copy](a: Option[T], b: T) -> bool { match a { Some(x) => x == b, None => false, } } fn main() -> bool { let b: u8 = 1; same(Some(1), b) }",
        "true",
    );
}

#[test]
fn signed_division_overflow_wraps() {
    eval(
        "fn main() -> i64 { let x: i64 = -9223372036854775808; x / -1 }",
        "-9223372036854775808",
    );
}

#[test]
fn control_flow_and_indexing_retain_every_literal_constraint() {
    rejects("fn main() -> u8 { let i = 1; (1, 256)[i] }");
    rejects("fn main() -> u8 { let i = 0; (256, 1)[i] }");
    rejects("fn main() -> i64 { if true { -1 } else { 18446744073709551615 } }");
    eval(
        "fn same[T: mod copy](a: T, b: T) -> bool { a == b } fn main() -> bool { let n: u8 = 2; same(if true { 2 } else { 1 }, n) }",
        "true",
    );
    eval(
        "fn main() -> u64 { let n: u64 = 18446744073709551615; let x = if false { 1 } else { n }; x }",
        "18446744073709551615",
    );
}
