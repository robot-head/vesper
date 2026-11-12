//! `vesper.arith.checked/1`: checked `+`, `-`, `*`, negation and signed
//! division raise `Fail[ArithOverflow]`; wrapping intrinsics are total.
//! Accepted programs must agree in the evaluator (optimized and not), after
//! AST equality saturation, in the JIT (optimized and not), in AArch64/x86_64
//! object emission, and in executable Lean extraction.
use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
use vesper::{check, ir, lean, native, numeric, optimize, parser, runtime::Runtime};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check::check(&p.module)
}
fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{source}\n{:?}", c.diagnostics);
    c
}
fn evaluate(world: &check::World, optimize: bool) -> String {
    let mut runtime = Runtime::new(world);
    runtime.optimize = optimize;
    runtime.run("main", vec![]).unwrap().to_string()
}
fn lean_eval(world: &check::World, expected: &str) {
    let mut output = lean::extract(world).unwrap();
    output.push_str("\n#eval Vesper.«main»\n");
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    if Command::new(&compiler).arg("--version").output().is_err() {
        eprintln!("Lean validation skipped: set VESPER_LEAN or install the pinned toolchain");
        return;
    }
    let path = std::env::temp_dir().join(format!(
        "vesper-checked-{}-{}.lean",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, &output).unwrap();
    let result = Command::new(&compiler).arg(&path).output().unwrap();
    fs::remove_file(&path).unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&result.stdout)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout).trim(), expected);
}
/// Every backend computes `expected` for `main`.
fn agree(source: &str, expected: &str) {
    let c = accepted(source);
    assert_eq!(evaluate(&c.world, true), expected, "evaluator: {source}");
    assert_eq!(evaluate(&c.world, false), expected, "unoptimized evaluator");
    assert_eq!(
        native::jit(&c.world, "main").unwrap().to_string(),
        expected,
        "JIT"
    );
    assert_eq!(
        native::jit_with_options(&c.world, "main", false)
            .unwrap()
            .to_string(),
        expected,
        "unoptimized JIT"
    );
    let (x86, _) =
        native::object_for_target(&c.world, "main", Some("x86_64-unknown-linux-gnu")).unwrap();
    assert_eq!(&x86[..4], b"\x7fELF");
    let (arm, _) =
        native::object_for_target(&c.world, "main", Some("aarch64-unknown-linux-gnu")).unwrap();
    assert_eq!(u16::from_le_bytes([arm[18], arm[19]]), 183);
    let mut module = parser::parse(source).module;
    optimize::optimize(&mut module, &c.expression_types);
    let o = check::check(&module);
    assert!(o.diagnostics.is_empty(), "optimized: {:?}", o.diagnostics);
    assert_eq!(
        evaluate(&o.world, true),
        expected,
        "AST-optimized evaluator"
    );
    assert_eq!(native::jit(&o.world, "main").unwrap().to_string(), expected);
    lean_eval(&c.world, expected);
}
fn rejects(source: &str, code: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code} for {source}: {:?}",
        c.diagnostics
    );
}
/// Run `body` (of type `ty`) under an overflow handler returning `fallback`.
fn guarded(ty: &str, body: &str, fallback: &str) -> String {
    format!(
        "fn main() -> {ty} {{ handle Fail[ArithOverflow] in {{ {body} }} with {{ return v => v, raise(e, k) => {fallback}, }} }}"
    )
}
const KINDS: [(&str, &str, &str); 4] = [
    ("u8", "0", "255"),
    ("u32", "0", "4294967295"),
    ("u64", "0", "18446744073709551615"),
    ("i64", "-9223372036854775808", "9223372036854775807"),
];

#[test]
fn contract_is_versioned() {
    assert_eq!(numeric::ARITHMETIC_SEMANTICS, "vesper.arith.checked/1");
    let c = accepted("fn main() -> i64 { 0 }");
    let error = &c.world.adts[numeric::OVERFLOW_TYPE];
    assert_eq!(error.mode, vesper::ast::Mode::Copy);
    assert!(
        lean::extract(&c.world)
            .unwrap()
            .contains("vesper.arith.checked/1")
    );
}

#[test]
fn endpoints_succeed_and_neighbours_overflow_for_every_kind() {
    for (ty, min, max) in KINDS {
        // In range: max - 1 + 1 and min + 1 - 1. Unknown operands come from
        // a parameter so the checker cannot discharge the check statically.
        let source = |body: &str| {
            format!(
                "fn f(a: {ty}, b: {ty}) -> {ty} eff [Fail[ArithOverflow]] {{ {body} }} fn g(a: {ty}, b: {ty}) -> bool {{ handle Fail[ArithOverflow] in {{ f(a, b); true }} with {{ return v => v, raise(e, k) => false, }} }}"
            )
        };
        let cases = [
            ("a + b", format!("wrapping_sub({max}, 1)"), "1", "true"),
            ("a + b", max.to_string(), "1", "false"),
            ("a - b", format!("wrapping_add({min}, 1)"), "1", "true"),
            ("a - b", min.to_string(), "1", "false"),
            ("a * b", max.to_string(), "1", "true"),
            ("a * b", max.to_string(), "2", "false"),
        ];
        for (body, a, b, expected) in cases {
            agree(
                &format!(
                    "{} fn main() -> bool {{ let a: {ty} = {a}; let b: {ty} = {b}; g(a, b) }}",
                    source(body)
                ),
                expected,
            );
        }
    }
}

#[test]
fn unsigned_high_values_and_signed_minimum_literals() {
    agree(
        &guarded(
            "u64",
            "let x: u64 = 18446744073709551615; let y: u64 = 1; x - y",
            "0",
        ),
        "18446744073709551614",
    );
    agree(
        "fn main() -> i64 { -9223372036854775808 }",
        "-9223372036854775808",
    );
    // Constant facts prove these in range: no handler is needed.
    agree("fn main() -> u8 { let x: u8 = 250; x + 5 }", "255");
    agree(
        "fn main() -> u64 { let x: u64 = 9223372036854775808; x - 1 + x }",
        "18446744073709551615",
    );
    agree(
        &guarded(
            "u64",
            "let x: u64 = 9223372036854775808; let y: u64 = 2; x * y - 1",
            "5",
        ),
        "5",
    );
    // A literal operand takes the other operand's kind.
    agree(&guarded("u8", "let x: u8 = 200; x + 100", "7"), "7");
    agree(&guarded("u32", "let x: u32 = 1; 0 - x", "9"), "9");
}

#[test]
fn proved_constants_need_no_effect_but_overflowing_ones_do() {
    accepted("fn main() -> u8 { 200 + 55 }");
    accepted("fn main() -> i64 { let x = 3; let y = x * 4; y - 12 }");
    rejects("fn main() -> u8 { 200 + 56 }", "E0401");
    rejects("fn main() -> i64 { 9223372036854775807 + 1 }", "E0401");
    rejects("fn main() -> u64 { let x: u64 = 0; x - 1 }", "E0401");
    rejects("fn f(x: i64) -> i64 { x + 1 }", "E0401");
    // An overflowing constant still raises at run time.
    agree(&guarded("u8", "let x: u8 = 200; x + 56", "42"), "42");
}

#[test]
fn compound_assignment_failures_leave_the_target_unchanged() {
    for (op, start, operand) in [("+=", "250", "10"), ("-=", "3", "4"), ("*=", "128", "2")] {
        agree(
            &format!(
                "fn bump(a: u8, b: u8) -> u8 {{ let x: u8 = a; let ok = handle Fail[ArithOverflow] in {{ x {op} b; true }} with {{ return v => v, raise(e, k) => false, }}; if ok {{ 0 }} else {{ x }} }} fn main() -> u8 {{ bump({start}, {operand}) }}"
            ),
            start,
        );
    }
    agree(
        "copy struct Acc { total: i64 } fn add(a: i64) -> i64 { let acc = Acc { total: 9223372036854775800 }; let r = handle Fail[ArithOverflow] in { acc.total += a; 1 } with { return v => v, raise(e, k) => 0, }; wrapping_add(wrapping_mul(r, 100), wrapping_sub(acc.total, 9223372036854775800)) } fn main() -> i64 { wrapping_add(wrapping_mul(add(7), 1000), add(8)) }",
        "107000",
    );
}

#[test]
fn negation_is_checked_for_every_kind() {
    agree(
        "fn neg(x: i64) -> i64 { handle Fail[ArithOverflow] in { -x } with { return v => v, raise(e, k) => 7, } } fn main() -> i64 { wrapping_add(neg(-9223372036854775808), neg(9223372036854775807)) }",
        "-9223372036854775800",
    );
    for ty in ["u8", "u32", "u64"] {
        agree(
            &format!(
                "fn neg(x: {ty}) -> {ty} {{ handle Fail[ArithOverflow] in {{ -x }} with {{ return v => v, raise(e, k) => 7, }} }} fn main() -> {ty} {{ wrapping_add(neg(0), neg(1)) }}"
            ),
            "7",
        );
    }
    // Negating a known in-range constant needs no handler.
    agree("fn main() -> i64 { let x = 5; -x }", "-5");
    rejects("fn main() -> u8 { let x: u8 = 1; -x }", "E0401");
}

#[test]
fn signed_division_corners() {
    let divide = "fn div(a: i64, b: i64) -> i64 { handle Fail[ArithOverflow] in { handle Fail[RuntimeError] in { a / b } with { return v => v, raise(e, k) => 2, } } with { return v => v, raise(e, k) => 1, } } fn rem(a: i64, b: i64) -> i64 { handle Fail[RuntimeError] in { a % b } with { return v => v, raise(e, k) => 3, } }";
    agree(
        &format!("{divide} fn main() -> i64 {{ div(-9223372036854775808, -1) }}"),
        "1",
    );
    agree(
        &format!("{divide} fn main() -> i64 {{ div(-9223372036854775808, 0) }}"),
        "2",
    );
    agree(
        &format!("{divide} fn main() -> i64 {{ rem(-9223372036854775808, -1) }}"),
        "0",
    );
    agree(
        &format!(
            "{divide} fn main() -> i64 {{ wrapping_add(wrapping_mul(div(-7, 2), 10), rem(-7, 2)) }}"
        ),
        "-31",
    );
    agree(&format!("{divide} fn main() -> i64 {{ rem(5, 0) }}"), "3");
    // A known divisor other than -1 proves signed division in range.
    accepted("fn f(a: i64) -> i64 { a / 2 }");
    accepted("fn f(a: i64) -> i64 { a % -1 }");
    // Without an ArithOverflow capability, the RuntimeError one cannot serve.
    rejects(
        "fn f(a: i64, b: i64) -> i64 eff [Fail[RuntimeError]] { a / b }",
        "E0400",
    );
    // Unsigned division cannot overflow.
    accepted("fn f(a: u64, b: u64) -> u64 eff [Fail[RuntimeError]] { a / b }");
    // Compound division follows the same order.
    agree(
        &guarded(
            "i64",
            "let x: i64 = -9223372036854775807; x -= 1; let y = handle Fail[RuntimeError] in { let d: i64 = -1; x /= d; 0 } with { return v => v, raise(e, k) => 5, }; y",
            "6",
        ),
        "6",
    );
}

#[test]
fn floats_keep_ieee_semantics() {
    agree(
        "fn main() -> bool { let x = 1.0e308; let y = x * 10.0; y > x }",
        "true",
    );
}

#[test]
fn wrapping_intrinsics_are_total_in_every_backend() {
    for (ty, min, max) in KINDS {
        agree(
            &format!("fn main() -> {ty} {{ let x: {ty} = {max}; wrapping_add(x, 1) }}"),
            min,
        );
        agree(
            &format!("fn main() -> {ty} {{ let x: {ty} = {min}; wrapping_sub(x, 1) }}"),
            max,
        );
    }
    agree("fn main() -> u8 { wrapping_mul(16, 17) }", "16");
    agree(
        "fn main() -> i64 { wrapping_mul(-9223372036854775808, -1) }",
        "-9223372036854775808",
    );
    agree(
        "fn f(a: u32, b: u32) -> u32 { wrapping_mul(a, b) } fn main() -> u32 { f(4294967295, 4294967295) }",
        "1",
    );
}

#[test]
fn wrapping_intrinsics_reject_invalid_uses() {
    rejects("fn main() -> i64 { wrapping_add(1) }", "E0207");
    rejects("fn main() -> i64 { wrapping_add(1, 2, 3) }", "E0207");
    rejects("fn main() -> f64 { wrapping_add(1.0, 2.0) }", "E0200");
    rejects("fn main() -> bool { wrapping_sub(true, false) }", "E0200");
    rejects(
        "fn main() -> u8 { let a: u8 = 1; let b: u64 = 2; wrapping_add(a, b) }",
        "E0200",
    );
    rejects("fn main() -> u8 { wrapping_add(255, 256) }", "E0209");
    rejects(
        "fn wrapping_add(a: i64, b: i64) -> i64 { a } fn main() -> i64 { 0 }",
        "E0100",
    );
    rejects(
        "fn main() -> i64 { let x = 1; wrapping_add(inout x, 1) }",
        "E0308",
    );
    // A local binding of the same name shadows the intrinsic lexically.
    agree(
        "fn main() -> i64 { let wrapping_add = |a: i64, b: i64| wrapping_mul(a, b); wrapping_add(6, 7) }",
        "42",
    );
    // The overflow type is reserved.
    rejects(
        "copy struct ArithOverflow {} fn main() -> i64 { 0 }",
        "E0100",
    );
}

#[test]
fn typed_fail_instances_select_their_own_handlers() {
    // Inner RuntimeError handler, outer ArithOverflow handler, and the
    // reverse nesting: each failure reaches the handler of its own type.
    let f =
        "fn f(a: i64, b: i64) -> i64 eff [Fail[RuntimeError], Fail[ArithOverflow]] { a / b + 1 }";
    for (a, b, expected) in [
        ("8", "2", "5"),
        ("8", "0", "20"),
        ("-9223372036854775808", "-1", "10"),
        ("9223372036854775807", "1", "10"),
    ] {
        agree(
            &format!(
                "{f} fn main() -> i64 {{ handle Fail[ArithOverflow] in {{ handle Fail[RuntimeError] in {{ f({a}, {b}) }} with {{ return v => v, raise(e, k) => 20, }} }} with {{ return v => v, raise(e, k) => 10, }} }}"
            ),
            expected,
        );
        agree(
            &format!(
                "{f} fn main() -> i64 {{ handle Fail[RuntimeError] in {{ handle Fail[ArithOverflow] in {{ f({a}, {b}) }} with {{ return v => v, raise(e, k) => 10, }} }} with {{ return v => v, raise(e, k) => 20, }} }}"
            ),
            expected,
        );
    }
    // A nearer handler of the same type wins.
    agree(
        "fn main() -> u8 { handle Fail[ArithOverflow] in { let x: u8 = 255; let y = handle Fail[RuntimeError] in { handle Fail[ArithOverflow] in { x + 1 } with { return v => v, raise(e, k) => 2, } } with { return v => v, raise(e, k) => 3, }; y } with { return v => v, raise(e, k) => 4, } }",
        "2",
    );
    // Labeled handlers still select by label.
    agree(
        "fn main() -> i64 { handle outer: Fail[ArithOverflow] in { handle Fail[ArithOverflow] in { let r: i64 = outer.raise(ArithOverflow {}); r } with { return v => v, raise(e, k) => 1, } } with { return v => v, raise(e, k) => 2, } }",
        "2",
    );
    // Generic error types are instantiated at the call site.
    agree(
        "fn lift[E: mod copy](r: Result[i64, E]) -> i64 eff [Fail[E]] { r? } fn main() -> i64 { handle Fail[ArithOverflow] in { handle Fail[RuntimeError] in { let r: Result[i64, ArithOverflow] = Err(ArithOverflow {}); lift(r) } with { return v => v, raise(e, k) => 1, } } with { return v => v, raise(e, k) => 2, } }",
        "2",
    );
    // Without an ArithOverflow instance in scope, overflow is unhandled.
    rejects(
        "fn f(a: i64) -> i64 eff [Fail[RuntimeError]] { a + 1 }",
        "E0400",
    );
}

#[test]
fn recursive_checked_functions_run_natively() {
    agree(
        "fn fact(n: u64) -> u64 eff [Fail[ArithOverflow]] { if n == 0 { 1 } else { n * fact(n - 1) } } fn safe(n: u64) -> u64 { handle Fail[ArithOverflow] in { fact(n) } with { return v => v, raise(e, k) => 0, } } fn main() -> u64 { wrapping_add(safe(20), safe(21)) }",
        "2432902008176640000",
    );
    agree(
        "fn sum(n: i64, acc: i64) -> i64 eff [Fail[ArithOverflow]] { if n == 0 { acc } else { sum(n - 1, acc + n) } } fn main() -> i64 { handle Fail[ArithOverflow] in { sum(100, 9223372036854775000) } with { return v => v, raise(e, k) => -1, } }",
        "-1",
    );
}

#[test]
fn aborts_respect_linear_values_state_and_continuations() {
    // A checked failure is abortable: it may not discard a live linear value,
    // linear handler state or a live one-shot continuation.
    rejects(
        "linear struct Ticket {} fn bad(x: i64) -> i64 eff [Fail[ArithOverflow]] { let t = Ticket {}; let y = x + 1; let Ticket {} = t; y }",
        "E0402",
    );
    rejects(
        "linear struct Ticket {} linear effect E { fn op() -> i64; } fn bad(x: i64) -> i64 eff [Fail[ArithOverflow]] { handle E state t = Ticket {} in { x + 1 } with { return (v, t) => { let Ticket {} = t; v }, op(t, k) => k(1, t), } }",
        "E0402",
    );
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn bad(x: i64) -> i64 eff [Fail[ArithOverflow]] { handle Ask in { Ask::ask() } with { return v => v, ask(k) => { let y = x + 1; k(y) }, } }",
        "E0402",
    );
    rejects(
        "fn bad(inout x: i64) eff [Fail[ArithOverflow]] { x += 1; }",
        "E0402",
    );
    // After the continuation is consumed, checked arithmetic is allowed, and
    // a linear value consumed before the failure is safe.
    agree(
        "linear struct Ticket { id: i64 } linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Fail[ArithOverflow] in { handle Ask in { Ask::ask() } with { return v => v, ask(k) => { let r = k(9223372036854775807); let t = Ticket { id: r }; let Ticket { id } = t; id + 1 }, } } with { return v => v, raise(e, k) => 42, } }",
        "42",
    );
}

#[test]
fn facts_are_invalidated_by_assignment_inout_calls_and_resumptions() {
    // Assignment replaces the fact.
    rejects("fn f(a: u8) -> u8 { let x: u8 = 1; x = a; x + 1 }", "E0401");
    // Field writes never establish a fact about their root.
    rejects(
        "copy struct P { v: u8 } fn f(a: u8) -> u8 { let p = P { v: 1 }; p.v = a; p.v + 1 }",
        "E0401",
    );
    // A compound assignment's own fact is the checked result.
    accepted("fn f() -> u8 { let x: u8 = 100; x += 100; x + 55 }");
    rejects(
        "fn f() -> u8 { let x: u8 = 100; x += 100; x + 56 }",
        "E0401",
    );
    // An inout call may change the place.
    rejects(
        "fn set(inout x: u8) { x = 255; } fn f() -> u8 { let x: u8 = 1; set(inout x); x + 1 }",
        "E0401",
    );
    // A resumption may change a place aliased by an operation argument.
    rejects(
        "linear effect Mut { fn change(inout x: u8); } fn f() -> u8 { let x: u8 = 1; handle Mut in { Mut::change(inout x); x + 1 } with { return v => v, change(x, k) => { x = 255; k(()) }, } }",
        "E0401",
    );
    rejects(
        "linear effect Ask { fn ask(in x: u8) -> u8; } fn f() -> u8 { let x: u8 = 1; handle Ask in { Ask::ask(x) } with { return v => v, ask(x, k) => { let r = k(0); x + r + 255 }, } }",
        "E0401",
    );
    // The handled program observes the modified value.
    agree(
        "linear effect Mut { fn change(inout x: u8); } fn main() -> u8 { let x: u8 = 1; handle Mut in { Mut::change(inout x); handle Fail[ArithOverflow] in { x + 1 } with { return v => v, raise(e, k) => 9, } } with { return v => v, change(x, k) => { x = 255; k(()) }, } }",
        "9",
    );
}

#[test]
fn closures_and_effect_rows_carry_overflow() {
    agree(
        "fn apply(sink f: fn(i64) -> i64 eff [Fail[ArithOverflow]], x: i64) -> i64 eff [Fail[ArithOverflow]] { f(x) } fn main() -> i64 { handle Fail[ArithOverflow] in { let g = |v: i64| v * 2; apply(g, 4611686018427387904) } with { return v => v, raise(e, k) => 3, } }",
        "3",
    );
    rejects(
        "fn apply(sink f: fn(i64) -> i64, x: i64) -> i64 { f(x) } fn main() -> i64 { apply(|v: i64| v + 1, 1) }",
        "E0401",
    );
}

#[test]
fn optimizers_preserve_checked_failures() {
    // `(a + b) * 0` must still raise, and `(a + b) - (a + b)` must not fold.
    for body in ["(a + b) * 0", "(a + b) - (a + b)", "0 * (a * b)"] {
        agree(
            &format!(
                "fn f(a: i64, b: i64) -> i64 {{ handle Fail[ArithOverflow] in {{ {body} }} with {{ return v => v, raise(e, k) => 7, }} }} fn main() -> i64 {{ f(9223372036854775807, 2) }}"
            ),
            "7",
        );
    }
    // Total subterms still simplify.
    let c = accepted(
        "fn f(a: i64) -> i64 { wrapping_mul(a, 0) } fn g(a: i64, b: i64) -> i64 { wrapping_sub(wrapping_add(a, b), b) } fn h(a: i64) -> i64 eff [Fail[ArithOverflow]] { a * 0 + (a - a) }",
    );
    let mut module = parser::parse("fn f(a: i64) -> i64 { wrapping_mul(a, 0) } fn g(a: i64, b: i64) -> i64 { wrapping_sub(wrapping_add(a, b), b) } fn h(a: i64) -> i64 eff [Fail[ArithOverflow]] { a * 0 + (a - a) }").module;
    optimize::optimize(&mut module, &c.expression_types);
    let bodies = module
        .items
        .iter()
        .filter_map(|i| match i {
            vesper::ast::Item::Function(f) => Some((f.name.clone(), f.body.clone().unwrap())),
            _ => None,
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let is = |name: &str, kind: &dyn Fn(&vesper::ast::ExprKind) -> bool| {
        let vesper::ast::ExprKind::Block { tail: Some(t), .. } = &bodies[name].kind else {
            panic!("{name}")
        };
        assert!(kind(&t.kind), "{name}: {:?}", t.kind);
    };
    is("f", &|k| {
        matches!(
            k,
            vesper::ast::ExprKind::Literal(vesper::ast::Literal::Int(0))
        )
    });
    is(
        "g",
        &|k| matches!(k, vesper::ast::ExprKind::Name(n) if n == "a"),
    );
    is("h", &|k| {
        matches!(
            k,
            vesper::ast::ExprKind::Literal(vesper::ast::Literal::Int(0))
        )
    });
    // Overflowing constants are not folded into wrapped literals.
    let source = "fn main() -> u8 { handle Fail[ArithOverflow] in { let x: u8 = 250 + 10; x } with { return v => v, raise(e, k) => 1, } }";
    let c = accepted(source);
    let mut module = parser::parse(source).module;
    optimize::optimize(&mut module, &c.expression_types);
    let o = check::check(&module);
    assert!(!o.world.overflow_ops.is_empty());
    assert_eq!(evaluate(&o.world, true), "1");
}

#[test]
fn fallible_arithmetic_is_ordered_ir_state() {
    let c = accepted(
        "fn f(a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] { let x = a + b; let y = wrapping_add(a, b); let z: i64 = 2; x + y + z * 3 }",
    );
    let program = ir::lower(&c);
    let instructions = program.functions["f"]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .collect::<Vec<_>>();
    let binaries = instructions
        .iter()
        .filter(|i| matches!(&i.op, ir::Op::Binary(..)))
        .collect::<Vec<_>>();
    // a + b and the two outer additions may fail; wrapping_add and the
    // constant product (z = 2) are total.
    let fallible = binaries.iter().filter(|i| i.fallible).count();
    assert_eq!(fallible, 3, "{binaries:#?}");
    for i in &binaries {
        assert_eq!(i.fallible, ir::state_action(i));
        if i.fallible {
            let (input, output) = i.token.unwrap();
            assert_ne!(input, output);
        } else {
            assert!(i.token.is_none());
        }
    }
    assert!(
        binaries.iter().any(
            |i| matches!(&i.op, ir::Op::Binary(op, ..) if op == "wrapping_add") && !i.fallible
        )
    );
    assert!(vesper::ir_check::validate(&c.world, &program).is_empty());
}

#[test]
fn interface_exports_the_builtin_overflow_contract() {
    let c = accepted("fn f(a: u8) -> u8 eff [Fail[ArithOverflow]] { a + 1 }");
    let interface = vesper::interface::export(&c.world, "@example/library").unwrap();
    assert!(
        interface
            .exports
            .contains_key("std/core::type::ArithOverflow")
    );
    assert!(!interface.exports.keys().any(|k| k.contains("wrapping_")));
    let json = serde_json::to_string(&interface.exports["@example/library::fn::f"]).unwrap();
    assert!(json.contains("std/core::type::ArithOverflow"), "{json}");
}
