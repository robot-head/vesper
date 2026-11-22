//! One native division for `/` and `%` of the same 128-bit operands
//! (#1437). Each 128-bit `/` or `%` is an inline word algorithm
//! (`src/integer_widths/native.rs`); a later one whose operands hold the
//! same values, where the earlier one's result is available, reuses its
//! quotient and remainder. These tests count the 64-bit `udiv`s in the
//! emitted CLIF against a lone division, and check the results on the
//! JIT and the evaluator, including cases that must not fuse.
use vesper::{check, native, parser, runtime::Runtime};

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c
}

/// The `udiv` instructions in the CLIF of `main` and everything it calls.
fn udivs(source: &str) -> usize {
    let c = checked(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    clif.matches(" udiv ").count()
}

fn agree(source: &str, expected: &str) {
    let c = checked(source);
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}): {e}"))
            .to_string();
        assert_eq!(jit, expected, "JIT (optimize {optimize})");
    }
    let mut runtime = Runtime::new(&c.world);
    let value = runtime.run("main", vec![]).unwrap().to_string();
    assert_eq!(value, expected, "evaluator");
}

const WORDS: &str = "fn w(hi: u64, lo: u64) -> u128 { (widen_u128(hi) << 64) | widen_u128(lo) }
";

/// A program whose `q(n, d)` has `body` under both failure handlers,
/// called once from `main` with `args`.
fn program(kind: &str, result: &str, body: &str, args: &str) -> String {
    format!(
        "{WORDS}fn q(n: {kind}, d: {kind}) -> {result} {{
    handle Fail[ArithOverflow] in {{
        handle Fail[RuntimeError] in {{ {body} }} with {{ return v => v, raise(e, k) => fallback(), }}
    }} with {{ return v => v, raise(e, k) => fallback(), }}
}}
fn main() -> {result} {{ q({args}) }}"
    )
}

fn pair(kind: &str) -> String {
    format!("({kind}, {kind})")
}

const ARGS_U: &str = "w(12345, 6789), w(3, 99)";
const ARGS_I: &str = "wrapping_i128(w(18446744073709551000, 6789)), wrapping_i128(w(3, 99))";

fn fallback(kind: &str, result: &str) -> String {
    let zero = format!("widen_{kind}(0)").replace("widen_i128(0)", "wrapping_i128(widen_u128(0))");
    if result.starts_with('(') {
        format!("fn fallback() -> {result} {{ ({zero}, {zero}) }}\n")
    } else {
        format!("fn fallback() -> {result} {{ {zero} }}\n")
    }
}

fn lone_and_pair(kind: &str, args: &str) -> (usize, String) {
    let lone = format!(
        "{}{}",
        fallback(kind, kind),
        program(kind, kind, "n / d", args)
    );
    let both = format!(
        "{}{}",
        fallback(kind, &pair(kind)),
        program(kind, &pair(kind), "(n / d, n % d)", args)
    );
    (udivs(&lone), both)
}

#[test]
fn unsigned_quotient_and_remainder_share_one_division() {
    let (lone, both) = lone_and_pair("u128", ARGS_U);
    assert!(lone > 0);
    assert_eq!(udivs(&both), lone, "{both}");
    let (n, d) = ((12345u128 << 64) | 6789, (3u128 << 64) | 99);
    agree(&both, &format!("({}, {})", n / d, n % d));
}

#[test]
fn signed_quotient_and_remainder_share_one_division() {
    let (lone, both) = lone_and_pair("i128", ARGS_I);
    assert_eq!(udivs(&both), lone, "{both}");
    let n = ((18446744073709551000u128 << 64) | 6789) as i128;
    let d = ((3u128 << 64) | 99) as i128;
    agree(&both, &format!("({}, {})", n / d, n % d));
}

#[test]
fn a_repeated_quotient_is_computed_once() {
    let source = format!(
        "{}{}",
        fallback("u128", &pair("u128")),
        program("u128", &pair("u128"), "(n / d, n / d)", ARGS_U)
    );
    let (lone, _) = lone_and_pair("u128", ARGS_U);
    assert_eq!(udivs(&source), lone);
    let (n, d) = ((12345u128 << 64) | 6789, (3u128 << 64) | 99);
    agree(&source, &format!("({}, {})", n / d, n / d));
}

#[test]
fn a_changed_operand_keeps_its_own_division() {
    // `m` is assigned between the two divisions.
    let source = format!(
        "{}{}",
        fallback("u128", &pair("u128")),
        program(
            "u128",
            &pair("u128"),
            "let mut m = n; let a = m / d; m = m + d; (a, m % d)",
            ARGS_U
        )
    );
    let (lone, _) = lone_and_pair("u128", ARGS_U);
    assert_eq!(udivs(&source), 2 * lone);
    let (n, d) = ((12345u128 << 64) | 6789, (3u128 << 64) | 99);
    agree(&source, &format!("({}, {})", n / d, (n + d) % d));
}

#[test]
fn a_division_on_one_branch_does_not_serve_the_join() {
    let source = format!(
        "{}{}",
        fallback("u128", &pair("u128")),
        program(
            "u128",
            &pair("u128"),
            "let a = if n > d { n / d } else { widen_u128(7) }; (a, n % d)",
            ARGS_U
        )
    );
    let (lone, _) = lone_and_pair("u128", ARGS_U);
    assert_eq!(udivs(&source), 2 * lone);
    let (n, d) = ((12345u128 << 64) | 6789, (3u128 << 64) | 99);
    agree(&source, &format!("({}, {})", n / d, n % d));
}
