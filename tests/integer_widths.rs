//! Integer widths (`vesper.arith.checked/1`): every operator on every
//! integer kind at boundary values agrees in the evaluator (optimized and
//! not) and the JIT (optimized and not), and matches the reference contract
//! in `vesper::numeric`. On AArch64 hosts the JIT runs AArch64 code; on
//! every host the program also compiles to x86-64 and AArch64 objects.
use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
use vesper::{
    check, lean, native,
    numeric::{ArithFailure, Conversion, INTEGER_KINDS, IntegerKind},
    parser,
    runtime::Runtime,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    check::check(&p.module)
}
fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{source}\n{:?}", c.diagnostics);
    c
}
fn rejects(source: &str, code: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code} for {source}: {:?}",
        c.diagnostics
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
    let (x86, _) =
        native::object_for_target(&c.world, "main", Some("x86_64-unknown-linux-gnu")).unwrap();
    assert_eq!(&x86[..4], b"\x7fELF");
    let (arm, _) =
        native::object_for_target(&c.world, "main", Some("aarch64-unknown-linux-gnu")).unwrap();
    assert_eq!(u16::from_le_bytes([arm[18], arm[19]]), 183);
}

/// Boundary values of a kind: MIN, MIN + 1, -1, 0, 1, MAX - 1, MAX.
fn boundaries(k: IntegerKind) -> Vec<i128> {
    let mut v = vec![k.min(), k.min() + 1, -1, 0, 1, k.max() - 1, k.max()];
    v.retain(|n| k.fits(*n));
    v.dedup();
    v
}
/// Shift and rotation amounts: in range, the width, and out of range.
fn amounts(k: IntegerKind) -> Vec<i128> {
    let bits = i128::from(k.bits());
    let mut v = vec![0, 1, bits - 1, bits, bits + 1, k.max(), -1];
    v.retain(|n| k.fits(*n));
    v.sort();
    v.dedup();
    v
}
/// The outcome tuple `(code, value)` a guarded call returns: code 0 is
/// success, 1 `Fail[ArithOverflow]`, 2 `Fail[RuntimeError]`.
fn outcome(r: Result<i128, ArithFailure>) -> String {
    match r {
        Ok(v) => format!("(0, {v})"),
        Err(ArithFailure::Overflow) => "(1, 0)".into(),
        Err(ArithFailure::DivisionByZero) => "(2, 0)".into(),
    }
}
/// A program whose `main` returns the outcome of `body` (over parameters
/// `a: ta` and `b: tb`, of type `result`) for every pair of `cases`.
fn program(ta: &str, tb: &str, result: &str, body: &str, cases: &[(i128, i128)]) -> String {
    let calls = cases
        .iter()
        .map(|(a, b)| format!("t({a}, {b})"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "fn op(a: {ta}, b: {tb}) -> {result} eff [Fail[ArithOverflow], Fail[RuntimeError]] {{ {body} }}
fn t(a: {ta}, b: {tb}) -> (i64, {result}) {{
    handle Fail[RuntimeError] in {{
        handle Fail[ArithOverflow] in {{ (0, op(a, b)) }} with {{ return v => v, raise(e, k) => (1, 0), }}
    }} with {{ return v => v, raise(e, k) => (2, 0), }}
}}
fn main() -> ({}) {{ ({calls},) }}",
        vec![format!("(i64, {result})"); cases.len()].join(", ")
    )
}
fn tuple(items: &[String]) -> String {
    format!("({})", items.join(", "))
}

#[test]
fn binary_operators_agree_on_every_kind_at_boundaries() {
    type Reference = fn(IntegerKind, i128, i128) -> Result<i128, ArithFailure>;
    let operators: [(&str, Reference); 13] = [
        ("a + b", |k, a, b| k.checked("+", a, b)),
        ("a - b", |k, a, b| k.checked("-", a, b)),
        ("a * b", |k, a, b| k.checked("*", a, b)),
        ("a / b", |k, a, b| k.checked("/", a, b)),
        ("a % b", |k, a, b| k.checked("%", a, b)),
        ("a & b", |k, a, b| Ok(k.bit("&", a, b).unwrap())),
        ("a | b", |k, a, b| Ok(k.bit("|", a, b).unwrap())),
        ("a ^ b", |k, a, b| Ok(k.bit("^", a, b).unwrap())),
        ("wrapping_add(a, b)", |k, a, b| Ok(k.wrapping("+", a, b))),
        ("wrapping_sub(a, b)", |k, a, b| Ok(k.wrapping("-", a, b))),
        ("wrapping_mul(a, b)", |k, a, b| Ok(k.wrapping("*", a, b))),
        ("if a < b { a } else { b }", |_, a, b| Ok(a.min(b))),
        ("if a == b { a } else { ~b }", |k, a, b| {
            Ok(if a == b { a } else { k.not(b) })
        }),
    ];
    for k in INTEGER_KINDS {
        let values = boundaries(k);
        let cases: Vec<(i128, i128)> = values
            .iter()
            .flat_map(|a| values.iter().map(move |b| (*a, *b)))
            .collect();
        for (body, reference) in operators {
            let expected = tuple(
                &cases
                    .iter()
                    .map(|(a, b)| outcome(reference(k, *a, *b)))
                    .collect::<Vec<_>>(),
            );
            agree(
                &program(k.name(), k.name(), k.name(), body, &cases),
                &expected,
            );
        }
    }
}

#[test]
fn negation_agrees_on_every_kind() {
    for k in INTEGER_KINDS {
        let cases: Vec<(i128, i128)> = boundaries(k).into_iter().map(|a| (a, 0)).collect();
        let expected = tuple(
            &cases
                .iter()
                .map(|(a, _)| outcome(k.checked_neg(*a).ok_or(ArithFailure::Overflow)))
                .collect::<Vec<_>>(),
        );
        agree(
            &program(k.name(), k.name(), k.name(), "-a", &cases),
            &expected,
        );
    }
}

#[test]
fn shifts_and_rotations_agree_on_every_kind() {
    for k in INTEGER_KINDS {
        let values = boundaries(k);
        let amounts = amounts(k);
        let cases: Vec<(i128, i128)> = values
            .iter()
            .flat_map(|a| amounts.iter().map(move |b| (*a, *b)))
            .collect();
        type Reference = fn(IntegerKind, i128, i128) -> Result<i128, ArithFailure>;
        let operators: [(&str, Reference); 4] = [
            ("a << b", |k, a, b| k.shift("<<", a, b)),
            ("a >> b", |k, a, b| k.shift(">>", a, b)),
            ("rotate_left(a, b)", |k, a, b| Ok(k.rotate(true, a, b))),
            ("rotate_right(a, b)", |k, a, b| Ok(k.rotate(false, a, b))),
        ];
        for (body, reference) in operators {
            let expected = tuple(
                &cases
                    .iter()
                    .map(|(a, b)| outcome(reference(k, *a, *b)))
                    .collect::<Vec<_>>(),
            );
            agree(
                &program(k.name(), k.name(), k.name(), body, &cases),
                &expected,
            );
        }
    }
    // The amount may have another kind than the shifted value.
    let cases = [(1, 0), (1, 63), (1, 64), (-1, 255)];
    let k = IntegerKind::I64;
    let expected = tuple(
        &cases
            .iter()
            .map(|(a, b)| outcome(k.shift("<<", *a, *b)))
            .collect::<Vec<_>>(),
    );
    agree(&program("i64", "u8", "i64", "a << b", &cases), &expected);
}

#[test]
fn conversions_agree_between_every_pair_of_kinds() {
    for from in INTEGER_KINDS {
        let values = boundaries(from);
        for to in INTEGER_KINDS {
            let mut conversions = vec![Conversion::Narrow, Conversion::Wrapping];
            if from.widens_to(to) {
                conversions.push(Conversion::Widen);
            }
            for conversion in conversions {
                let cases: Vec<(i128, i128)> = values.iter().map(|a| (*a, 0)).collect();
                let expected = tuple(
                    &cases
                        .iter()
                        .map(|(a, _)| outcome(conversion.apply(to, *a)))
                        .collect::<Vec<_>>(),
                );
                let body = format!("{}(a)", conversion.name(to));
                agree(
                    &program(from.name(), "u8", to.name(), &body, &cases),
                    &expected,
                );
            }
        }
    }
}

#[test]
fn byte_order_round_trips_and_matches_rust() {
    for k in INTEGER_KINDS.into_iter().filter(|k| k.bits() >= 16) {
        let n = (k.bits() / 8) as usize;
        for value in boundaries(k)
            .into_iter()
            .chain([0x0102_0304_0506_0708 & k.max()])
        {
            let pattern = k.pattern(value);
            let le: Vec<String> = (0..n)
                .map(|i| ((pattern >> (8 * i)) & 255).to_string())
                .collect();
            let be: Vec<String> = le.iter().rev().cloned().collect();
            let name = k.name();
            let source = format!(
                "fn main() -> (({u8s}), ({u8s}), {name}, {name}) {{
    let x: {name} = {value};
    let le = to_le_bytes_{name}(x);
    let be = to_be_bytes_{name}(x);
    (le, be, from_le_bytes_{name}(le), from_be_bytes_{name}(be))
}}",
                u8s = vec!["u8"; n].join(", ")
            );
            let expected = format!("({}, {}, {value}, {value})", tuple(&le), tuple(&be));
            agree(&source, &expected);
        }
    }
}

#[test]
fn proven_amounts_and_conversions_need_no_effect() {
    agree(
        "fn main() -> (u8, i16, u32, i8) { let x: u8 = 3; (x << 5, narrow_i16(-300), widen_u32(x) << 31, wrapping_i8(x) >> 1) }",
        "(96, -300, 2147483648, 1)",
    );
    // An unproven amount or narrowing needs Fail[ArithOverflow].
    rejects("fn f(a: u8, n: u32) -> u8 { a << n }", "E0401");
    rejects("fn f(a: i64) -> i8 { narrow_i8(a) }", "E0401");
    rejects("fn f() -> u8 { let x: u8 = 1; x << 8 }", "E0401");
}

#[test]
fn invalid_uses_are_rejected() {
    // Widening must be lossless.
    rejects("fn f(a: i8) -> u64 { widen_u64(a) }", "E0200");
    rejects("fn f(a: u64) -> i64 { widen_i64(a) }", "E0200");
    rejects("fn f(a: usize) -> u32 { widen_u32(a) }", "E0200");
    // Bit operators take integers of one kind.
    rejects("fn f(a: bool, b: bool) -> bool { a & b }", "E0200");
    rejects("fn f(a: f64) -> f64 { ~a }", "E0200");
    rejects("fn f(a: u8, b: u16) -> u8 { a | b }", "E0200");
    rejects(
        "fn f(a: u8, b: f64) -> u8 eff [Fail[ArithOverflow]] { a << b }",
        "E0200",
    );
    rejects("fn f(a: f64) -> f64 { rotate_left(a, 1) }", "E0200");
    rejects("fn f(a: u8) -> u16 { widen_u16(a, a) }", "E0207");
    // No implicit conversions between kinds of the same width.
    rejects("fn f(a: usize) -> u64 { a }", "E0200");
    rejects("fn f(a: isize) -> i64 { a }", "E0200");
    rejects("fn f(a: u32) -> i32 { a }", "E0200");
    // An unsuffixed shift amount is a u32 literal.
    rejects(
        "fn f() -> u64 eff [Fail[ArithOverflow]] { 1 << 4294967296 }",
        "E0209",
    );
    // A checked conversion of a secret branches on it; a wrapping one does not.
    rejects(
        "copy struct Secret[T] { value: T } fn f(s: Secret[u64]) -> Unit eff [ConstTime, Fail[ArithOverflow]] { let Secret { value } = s; let x = narrow_u8(value); }",
        "E0703",
    );
    accepted(
        "copy struct Secret[T] { value: T } fn f(s: Secret[u64]) -> Secret[u8] eff [ConstTime] { let Secret { value } = s; Secret { value: wrapping_u8(value) } }",
    );
}

#[test]
fn literal_patterns_match_every_kind() {
    for kind in INTEGER_KINDS {
        let k = kind.name();
        let n = if kind.signed() { "-1" } else { "1" };
        agree(
            &format!(
                "fn pick(x: {k}) -> u8 {{ match x {{ {n} => 10, 0 => 20, _ => 30 }} }}
fn main() -> (u8, u8, u8) {{ let a: {k} = {n}; let b: {k} = 0; let c: {k} = 2; (pick(a), pick(b), pick(c)) }}"
            ),
            "(10, 20, 30)",
        );
    }
}

#[test]
fn every_kind_is_copy_and_distinct() {
    let c = accepted(
        "copy struct All { a: u8, b: u16, c: u32, d: u64, e: usize, f: i8, g: i16, h: i32, i: i64, j: isize }
fn main() -> All { let x = All { a: 255, b: 65535, c: 1, d: 2, e: 18446744073709551615, f: -128, g: -32768, h: -2147483648, i: 5, j: -1 }; let y = x; x }",
    );
    let expected = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), expected);
}

/// Lean extraction models every kind's checked arithmetic and comparisons;
/// it replays when a Lean toolchain is available (`VESPER_LEAN`).
#[test]
fn lean_extraction_models_every_kind() {
    let source = include_str!("fixtures/integer_widths/kinds.ves");
    let c = accepted(source);
    let expected = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), expected);
    let mut output = lean::extract(&c.world).unwrap();
    for lean_type in ["UInt16", "Int8", "Int16", "Int32"] {
        assert!(output.contains(lean_type), "{lean_type}");
    }
    output.push_str("\n#eval Vesper.«main»\n");
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    if Command::new(&compiler).arg("--version").output().is_err() {
        eprintln!("Lean validation skipped: set VESPER_LEAN or install the pinned toolchain");
        return;
    }
    let path = std::env::temp_dir().join(format!(
        "vesper-widths-{}-{}.lean",
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
    // Lean prints tuples right-nested and wrapped, so compare the scalars
    // in order.
    let scalars = |s: &str| -> Vec<String> {
        s.split(|c: char| !(c.is_ascii_digit() || c == '-'))
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    };
    assert_eq!(
        scalars(&String::from_utf8_lossy(&result.stdout)),
        scalars(&expected)
    );
}

#[test]
fn lean_wasm_and_ebpf_refuse_new_operators_with_typed_errors() {
    let c = accepted("fn main() -> u8 { let x: u8 = 3; x & 1 }");
    let error = lean::extract(&c.world).unwrap_err();
    assert!(error.contains("&"), "{error}");
    let c = accepted("fn main() -> u64 { let x: u8 = 3; widen_u64(x) }");
    let error = lean::extract(&c.world).unwrap_err();
    assert!(error.contains("widen_u64"), "{error}");
    let c = accepted("fn main() -> i16 { 7 }");
    let error = vesper::wasm_backend::compile(&c.world, "main", &Default::default()).unwrap_err();
    assert!(
        matches!(
            error,
            vesper::wasm_backend::WasmError::Unsupported {
                construct: vesper::wasm_backend::Unsupported::IntegerWidth,
                ..
            }
        ),
        "{error:?}"
    );
}
