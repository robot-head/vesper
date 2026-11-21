//! Integer totality (SH5 lane 5, #1328; manuscript ch. 3, "Integer kinds").
//!
//! - A shift whose amount is a constant, a mask, or an expression whose
//!   `+`, `-`, `*`, `/` and `>>` range facts bound below the width (such as
//!   `i * 8` under `if i < 8`, or `hw * 16` with `hw = (w >> 21) & 3`) is
//!   total: no `Fail`, no check.
//! - A division or remainder whose divisor such facts prove nonzero needs
//!   no handler; signed `MIN / -1` keeps its rule.
//! - Prefix `!` on an integer is its bitwise complement, `~`.
//!
//! Every accepted program runs in the evaluator (optimized and not) and the
//! JIT (optimized and not) and compiles to x86-64 and AArch64 objects; the
//! results match a Rust model. Programs that could fail still need `Fail`.
use vesper::{check, native, parser, runtime::Runtime};

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    check::check(&p.module)
}

fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    let shown: Vec<_> = c
        .diagnostics
        .iter()
        .map(|d| format!("{}: {} at `{}`", d.code, d.message, &source[d.span.clone()]))
        .collect();
    assert!(c.diagnostics.is_empty(), "{source}\n{shown:#?}");
    c
}

/// `source` is rejected with `code` at a span whose text is `at` (a binary
/// expression's span ends before a closing parenthesis of its right operand).
fn rejects_at(source: &str, code: &str, at: &str) {
    let c = checked(source);
    let matches = |text: Option<&str>| {
        text.is_some_and(|t| t == at || (!t.is_empty() && t == at.trim_end_matches(')')))
    };
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && matches(source.get(d.span.clone()))),
        "expected {code} at `{at}` for {source}: {:?}",
        c.diagnostics
            .iter()
            .map(|d| (d.code.clone(), source.get(d.span.clone()).unwrap_or("")))
            .collect::<Vec<_>>()
    );
}

/// Evaluator, optimized and unoptimized JIT, and both object targets agree
/// on `main`, and equal `expected`.
fn agree(source: &str, expected: &str) {
    let c = accepted(source);
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = optimize;
        let value = runtime.run("main", vec![]).unwrap().to_string();
        assert_eq!(value, expected, "evaluator (optimize {optimize}): {source}");
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(jit, expected, "JIT (optimize {optimize}): {source}");
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (object, _) = native::object_for_target(&c.world, "main", Some(target)).unwrap();
        assert_eq!(&object[..4], b"\x7fELF", "{target}");
    }
}

/// The checked operations of `source` that may raise `Fail`, by source text.
fn failing_ops(source: &str) -> Vec<String> {
    let c = accepted(source);
    let world = &c.world;
    world
        .overflow_ops
        .iter()
        .chain(&world.partial_ops)
        .filter_map(|(a, b)| source.get(*a..*b).map(str::to_string))
        .collect()
}

/// A fold of `f(w, i)` for `i` in `0..n`, in the program and in Rust.
fn fold_main(n: u64) -> String {
    format!(
        "fn main() -> u64 {{
    let mut acc: u64 = 7;
    let w: u64 = 81985529216486895;
    for i in 0..{n} {{
        acc = wrapping_add(wrapping_mul(acc, 31), f(wrapping_mul(w, wrapping_add(i, 1)), i));
    }}
    acc
}}"
    )
}

fn fold(n: u64, f: impl Fn(u64, u64) -> u64) -> String {
    let w: u64 = 81985529216486895;
    (0..n)
        .fold(7u64, |acc, i| {
            acc.wrapping_mul(31)
                .wrapping_add(f(w.wrapping_mul(i.wrapping_add(1)), i))
        })
        .to_string()
}

#[test]
fn guarded_shift_amounts_are_total() {
    // (source of `f(w: u64, i: u64) -> u64`, Rust model)
    type Model = fn(u64, u64) -> u64;
    let cases: [(&str, Model); 8] = [
        ("if i < 8 { (w >> (i * 8)) & 255 } else { 0 }", |w, i| {
            if i < 8 { (w >> (i * 8)) & 255 } else { 0 }
        }),
        ("w >> ((i % 8) * 8)", |w, i| w >> ((i % 8) * 8)),
        ("let hw = (w >> 21) & 3; (w >> 5) << (hw * 16)", |w, _| {
            (w >> 5) << (((w >> 21) & 3) * 16)
        }),
        ("w >> (63 - (i & 63))", |w, i| w >> (63 - (i & 63))),
        ("if i < 64 { w >> (63 - i) } else { 0 }", |w, i| {
            if i < 64 { w >> (63 - i) } else { 0 }
        }),
        ("if i >= 16 { return 0; } w << (i * 4 + 3)", |w, i| {
            if i >= 16 { 0 } else { w << (i * 4 + 3) }
        }),
        ("w >> ((i & 255) / 4)", |w, i| w >> ((i & 255) / 4)),
        ("w << ((i & 1023) >> 4)", |w, i| w << ((i & 1023) >> 4)),
    ];
    for (body, model) in cases {
        let source = format!(
            "fn f(w: u64, i: u64) -> u64 {{ {body} }}\n{}",
            fold_main(70)
        );
        assert_eq!(failing_ops(&source), Vec::<String>::new(), "{source}");
        agree(&source, &fold(70, model));
    }
}

#[test]
fn guarded_shifts_on_narrow_and_signed_kinds() {
    let source = "fn f(x: u32, n: u64) -> u32 { if n < 4 { x >> (n * 8) } else { 0 } }
fn g(x: i16, n: i64) -> i16 { if n >= 0 && n < 4 { x >> (n * 4 + 3) } else { 0 } }
fn main() -> (u32, u32, i16, i16) { (f(4278190080, 3), f(4278190080, 2), g(-32768, 3), g(1024, 1)) }";
    assert_eq!(failing_ops(source), Vec::<String>::new());
    agree(source, "(255, 65280, -1, 8)");
}

#[test]
fn shifts_that_could_fail_still_need_fail() {
    for (body, at) in [
        // No guard: `i` may be 8.
        ("(w >> (i * 8)) & 255", "i * 8"),
        // `i < 9` admits an amount of 64.
        ("if i < 9 { w >> (i * 8) } else { 0 }", "w >> (i * 8)"),
        // `63 - i` underflows for `i > 63`.
        ("w >> (63 - i)", "63 - i"),
        // A negative amount is out of range on a signed kind.
        ("if j < 8 { w >> (j * 8) } else { 0 }", "w >> (j * 8)"),
    ] {
        let source =
            format!("fn f(w: u64, i: u64, j: i64) -> u64 {{ {body} }}\nfn main() -> u64 {{ 0 }}");
        rejects_at(&source, "E0401", at);
    }
}

#[test]
fn guarded_divisors_need_no_handler() {
    type Model = fn(u64, u64) -> u64;
    let cases: [(&str, Model); 5] = [
        ("if i == 0 { return 0; } w / i", |w, i| {
            w.checked_div(i).unwrap_or(0)
        }),
        ("if i != 0 { w % i } else { 0 }", |w, i| {
            if i != 0 { w % i } else { 0 }
        }),
        ("w / ((i & 15) + 1)", |w, i| w / ((i & 15) + 1)),
        ("if i < 100 { w % (i * 3 + 2) } else { 1 }", |w, i| {
            if i < 100 { w % (i * 3 + 2) } else { 1 }
        }),
        ("if i > 0 && i < 1000 { w / (i * 7) } else { 2 }", |w, i| {
            if i > 0 && i < 1000 { w / (i * 7) } else { 2 }
        }),
    ];
    for (body, model) in cases {
        let source = format!(
            "fn f(w: u64, i: u64) -> u64 {{ {body} }}\n{}",
            fold_main(40)
        );
        assert_eq!(failing_ops(&source), Vec::<String>::new(), "{source}");
        agree(&source, &fold(40, model));
    }
}

#[test]
fn divisions_that_could_fail_still_need_fail() {
    for (body, at) in [
        // No guard.
        ("w / i", "w / i"),
        // `i - 1` is zero at `i == 1`.
        ("if i > 0 { w / (i - 1) } else { 0 }", "w / (i - 1)"),
        // `i + 1` overflows at `u64::MAX`.
        ("w / (i + 1)", "i + 1"),
        // `i & 15` may be zero.
        ("w % (i & 15)", "w % (i & 15)"),
    ] {
        let source = format!("fn f(w: u64, i: u64) -> u64 {{ {body} }}\nfn main() -> u64 {{ 0 }}");
        rejects_at(&source, "E0401", at);
    }
    // Signed `MIN / -1` keeps its rule: a nonzero divisor is not enough.
    rejects_at(
        "fn f(a: i64, d: i64) -> i64 { if d == 0 { return 0; } a / d }\nfn main() -> i64 { 0 }",
        "E0401",
        "a / d",
    );
    // A divisor that cannot be -1 is.
    let source = "fn f(a: i64, d: i64) -> i64 { if d > 0 { a / d } else { 0 } }
fn g(a: i64, d: i64) -> i64 { if d >= 0 && d < 10 { a / (d * 2 + 1) } else { 0 } }
fn main() -> (i64, i64) { (f(-9223372036854775807, 3), g(-100, 2)) }";
    assert_eq!(failing_ops(source), Vec::<String>::new());
    agree(source, "(-3074457345618258602, -20)");
}

#[test]
fn integer_not_is_the_complement_on_every_kind() {
    let kinds = [
        ("u8", 0x5a_u128, 0xa5_u128),
        ("u16", 0x1234, 0xedcb),
        ("u32", 0, 0xffff_ffff),
        ("u64", 1, u128::from(u64::MAX - 1)),
        ("usize", 7, u128::from(u64::MAX - 7)),
    ];
    for (k, x, not) in kinds {
        let source = format!("fn f(x: {k}) -> {k} {{ !x }}\nfn main() -> {k} {{ f({x}) }}");
        assert_eq!(failing_ops(&source), Vec::<String>::new());
        agree(&source, &not.to_string());
    }
    for (k, x, not) in [
        ("i8", -128_i128, 127_i128),
        ("i16", 5, -6),
        ("i32", -1, 0),
        ("i64", 0, -1),
        ("isize", 41, -42),
    ] {
        let source =
            format!("fn f(x: {k}) -> {k} {{ !x }}\nfn main() -> {k} {{ let v: {k} = {x}; f(v) }}");
        agree(&source, &not.to_string());
    }
    // 128-bit kinds.
    agree(
        "fn main() -> bool { let x: u128 = 5; let y: i128 = -6; !x == ~x && !y == 5 }",
        "true",
    );
    // `!` on `bool` keeps its meaning, also beside an integer `!`.
    agree(
        "fn main() -> (bool, u8) { let b = true; let m: u8 = 15; (!b, !m & 255) }",
        "(false, 240)",
    );
    // Nested, in a condition and in a pure guard.
    agree(
        "fn main() -> u64 { let x: u64 = 6; if !!x == x && (!x & 1) == 1 { !(x | 1) } else { 0 } }",
        &(!7u64).to_string(),
    );
}

#[test]
fn integer_not_compiles_exactly_like_complement() {
    let not = "fn f(x: u32, y: i64) -> (u32, i64) { (!x, !y ^ 3) }\nfn main() -> u32 { let (a, b) = f(9, 3); a }";
    let tilde = not.replace('!', "~");
    let a = accepted(not);
    let b = accepted(&tilde);
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (x, _) = native::object_for_target(&a.world, "main", Some(target)).unwrap();
        let (y, _) = native::object_for_target(&b.world, "main", Some(target)).unwrap();
        assert_eq!(x, y, "{target}");
    }
}

#[test]
fn integer_not_misuse_is_rejected() {
    // An integer complement is not a `bool`.
    rejects_at(
        "fn f(x: u64) -> bool { !x }\nfn main() -> bool { f(1) }",
        "E0200",
        "!x",
    );
    // `!` on a non-integer, non-`bool` operand.
    rejects_at(
        "fn f(x: f64) -> f64 { !x }\nfn main() -> f64 { f(1.0) }",
        "E0200",
        "!x",
    );
}

#[test]
fn isolated_checking_rewrites_integer_not() {
    // `!z & 63` is a pure guard only once `!` is `~`: the isolated checker
    // (expected-error tests) must agree with the full one.
    let source = "fn f(n: u64) -> u64 { let z: u64 = 0; if n < (!z & 63) { 1 << n } else { 0 } }
fn main() -> u64 { f(3) }";
    let c = accepted(source);
    let p = parser::parse(source);
    for item in &p.module.items {
        if let vesper::ast::Item::Function(f) = item {
            assert_eq!(
                c.world.check_isolated_item(f),
                vec![],
                "isolated check of {}",
                f.name
            );
        }
    }
}

#[test]
fn contradictory_guards_do_not_crash_the_checker() {
    // `d` has the empty range `1..=0` in the unreachable branch.
    for body in [
        "if d > 0 && d < 1 { n >> (n / d) } else { 0 }",
        "if d > 5 && d < 3 { n >> (d * 2) } else { 0 }",
        "if d > 0 && d < 1 { n / (d - 1) } else { 0 }",
    ] {
        let source =
            format!("fn f(n: u64, d: u64) -> u64 {{ {body} }}\nfn main() -> u64 {{ f(1, 2) }}");
        let _ = checked(&source);
    }
}
