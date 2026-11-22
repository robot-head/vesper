//! Integer literals past `u64::MAX` (self-hosting SH3 lane 5, #1244): a
//! literal is at most `u128::MAX`, and `-170141183460469231731687303715884105728`
//! is `i128::MIN`. A literal above `i128::MAX` is a `u128`; an unconstrained
//! literal past `u64::MAX` is an `i128`.
use vesper::{ast::Literal, check, native, parser, runtime::Runtime};

fn accepted(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    let c = check::check(&p.module);
    let shown: Vec<_> = c
        .diagnostics
        .iter()
        .map(|d| format!("{}: {} at `{}`", d.code, d.message, &source[d.span.clone()]))
        .collect();
    assert!(c.diagnostics.is_empty(), "{source}\n{shown:#?}");
    c.world
}

fn both(source: &str) -> String {
    let world = accepted(source);
    let evaluated = Runtime::new(&world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    let jit = native::jit(&world, "main").unwrap().to_string();
    assert_eq!(evaluated, jit, "evaluator and native JIT agree");
    evaluated
}

fn rejected(source: &str) -> Vec<String> {
    let p = parser::parse(source);
    if !p.diagnostics.is_empty() {
        return p.diagnostics.iter().map(|d| d.message.clone()).collect();
    }
    check::check(&p.module)
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn lane_repro_u128_max() {
    assert_eq!(
        both(
            "fn f() -> u128 { 340282366920938463463374607431768211455 }
fn main() -> bool { f() == 340282366920938463463374607431768211455 }"
        ),
        "true"
    );
}

#[test]
fn the_parser_picks_int_or_wide_int() {
    let lit = |text: &str| {
        let p = parser::parse(&format!("fn f() -> u128 {{ {text} }}"));
        assert!(p.diagnostics.is_empty(), "{text}: {:?}", p.diagnostics);
        format!("{:?}", p.module.items)
    };
    assert!(lit("18446744073709551616").contains("Int(18446744073709551616)"));
    assert!(
        lit("170141183460469231731687303715884105727")
            .contains("Int(170141183460469231731687303715884105727)")
    );
    assert!(
        lit("170141183460469231731687303715884105728")
            .contains("WideInt(170141183460469231731687303715884105728)")
    );
    assert!(
        lit("0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff")
            .contains(&format!("WideInt({})", u128::MAX))
    );
    assert_eq!(Literal::int(u128::MAX), Literal::WideInt(u128::MAX));
    assert_eq!(Literal::int(5), Literal::Int(5));
}

#[test]
fn i128_min_and_negated_literals() {
    assert_eq!(
        both(
            "fn low() -> i128 { -170141183460469231731687303715884105728 }
fn main() -> bool { low() < -170141183460469231731687303715884105727 }"
        ),
        "true"
    );
    assert_eq!(
        both(
            "fn s(x: i128) -> u64 {
    match x { -170141183460469231731687303715884105728 => 1, -18446744073709551616 => 2, _ => 3 }
}
fn main() -> u64 {
    wrapping_add(wrapping_mul(s(-170141183460469231731687303715884105728), 10), s(-18446744073709551616))
}"
        ),
        "12"
    );
}

#[test]
fn literals_take_their_kind_from_context() {
    // Between u64::MAX and i128::MAX the literal fits i128 and u128.
    assert_eq!(
        both(
            "fn a() -> i128 { 18446744073709551616 }
fn b() -> u128 { 18446744073709551616 }
fn main() -> bool { a() == 18446744073709551616 && b() == wrapping_add(widen_u128(18446744073709551615), 1) }"
        )
        .trim(),
        "true"
    );
}

#[test]
fn out_of_range_literals_are_errors() {
    // Past u128::MAX.
    let e = rejected("fn f() -> u128 { 340282366920938463463374607431768211456 }");
    assert!(e.iter().any(|m| m.contains("exceeds u128")), "{e:?}");
    // Above i128::MAX only u128 holds it.
    let e = rejected("fn f() -> i128 { 170141183460469231731687303715884105728 }");
    assert!(!e.is_empty());
    // A 128-bit literal does not fit u64.
    let e = rejected("fn f() -> u64 { 18446744073709551616 }");
    assert!(!e.is_empty());
    // A negated pattern past 2^127.
    let e = rejected(
        "fn f(x: i128) -> u64 { match x { -170141183460469231731687303715884105729 => 1, _ => 0 } }",
    );
    assert!(e.iter().any(|m| m.contains("exceeds i128")), "{e:?}");
}

#[test]
fn native_objects_build() {
    let world = accepted(include_str!("fixtures/wide_literals.ves"));
    native::object(&world, "main").expect("native object");
}

/// The fixture the front end v2 and AST differentials parse also checks and
/// runs.
#[test]
fn the_differential_fixture_runs() {
    assert_eq!(both(include_str!("fixtures/wide_literals.ves")), "141");
}

/// A constant table of 128-bit integers keeps both words: it is not encoded
/// as one-word read-only data, and the evaluator and native code (#1270)
/// read every element.
#[test]
fn wide_tables_keep_both_words() {
    assert_eq!(
        both(
            "const TABLE: [i128; 3] = [18446744073709551616, -1, -170141183460469231731687303715884105728];
fn main() -> bool { TABLE[0] == 18446744073709551616 && TABLE[1] == -1 && TABLE[2] == -170141183460469231731687303715884105728 }"
        ),
        "true"
    );
}
