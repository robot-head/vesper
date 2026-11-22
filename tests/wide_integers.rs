//! 128-bit integers (`i128`, `u128`; self-hosting wave SH2 lane 2, #1174).
//!
//! The census sample (`docs/parallel-results/self-host-census.md`, gap C2)
//! stored `Type::IntLiteral(i128)` as a sign and a `u64` magnitude because
//! Vesper had no 128-bit kind. These tests pin the two kinds down: literal
//! typing, the checked contract, the total operators, conversions and the
//! builtins, agreeing in the evaluator (optimized and not) and the JIT
//! (optimized and not), against Rust's own `i128`/`u128` arithmetic. Every
//! program also compiles to x86-64 and AArch64 objects.
use vesper::{
    check, native,
    numeric::{INTEGER_KINDS, IntegerKind},
    parser,
    runtime::Runtime,
};

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
/// The evaluator and the JIT, optimized and not, agree on `main` and equal
/// `expected`; both object targets compile.
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
        native::object_for_target(&c.world, "main", Some(target))
            .unwrap_or_else(|e| panic!("{target}: {e}\n{source}"));
    }
}

const WIDE: [IntegerKind; 2] = [IntegerKind::I128, IntegerKind::U128];

/// A 128-bit value as Vesper source: literals reach `u64::MAX`, so larger
/// patterns are built from their two words.
fn value(k: IntegerKind, pattern: u128) -> String {
    let (hi, lo) = ((pattern >> 64) as u64, pattern as u64);
    format!("w_{}({hi}, {lo})", k.name())
}
/// The constructors [`value`] calls.
const WORDS: &str =
    "fn w_u128(hi: u64, lo: u64) -> u128 { (widen_u128(hi) << 64) | widen_u128(lo) }
fn w_i128(hi: u64, lo: u64) -> i128 { wrapping_i128(w_u128(hi, lo)) }
";
/// How a pattern of `k` prints.
fn show(k: IntegerKind, pattern: u128) -> String {
    if k.signed() {
        (pattern as i128).to_string()
    } else {
        pattern.to_string()
    }
}
/// Boundary values of a 128-bit kind as bit patterns: MIN, MIN + 1, -1, 0,
/// 1, the word edges and MAX - 1, MAX.
fn boundaries(k: IntegerKind) -> Vec<u128> {
    let mut v: Vec<u128> = if k.signed() {
        [
            i128::MIN,
            i128::MIN + 1,
            -(1i128 << 64),
            -1,
            0,
            1,
            u64::MAX as i128,
            1 << 64,
            i128::MAX - 1,
            i128::MAX,
        ]
        .map(|n| n as u128)
        .to_vec()
    } else {
        vec![
            0,
            1,
            u64::MAX as u128,
            1 << 64,
            1 << 127,
            u128::MAX - 1,
            u128::MAX,
        ]
    };
    v.dedup();
    v
}
/// `(code, value)`: code 0 is success, 1 `Fail[ArithOverflow]`, 2
/// `Fail[RuntimeError]`.
#[derive(Clone, Copy)]
enum Outcome {
    Ok(u128),
    Overflow,
    DivisionByZero,
}
fn outcome(k: IntegerKind, o: Outcome) -> String {
    match o {
        Outcome::Ok(p) => format!("(0, {})", show(k, p)),
        Outcome::Overflow => "(1, 0)".into(),
        Outcome::DivisionByZero => "(2, 0)".into(),
    }
}
/// A program whose `main` returns the outcome of `body` (over `a: ta`,
/// `b: tb`, of type `result`) for every pair of argument sources.
fn program(ta: &str, tb: &str, result: &str, body: &str, cases: &[(String, String)]) -> String {
    let calls = cases
        .iter()
        .map(|(a, b)| format!("t({a}, {b})"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{WORDS}fn op(a: {ta}, b: {tb}) -> {result} eff [Fail[ArithOverflow], Fail[RuntimeError]] {{ {body} }}
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

/// Rust's own checked arithmetic on `k`.
fn rust_checked(k: IntegerKind, op: &str, a: u128, b: u128) -> Outcome {
    if matches!(op, "/" | "%") && b == 0 {
        return Outcome::DivisionByZero;
    }
    let r = if k.signed() {
        let (a, b) = (a as i128, b as i128);
        match op {
            "+" => a.checked_add(b),
            "-" => a.checked_sub(b),
            "*" => a.checked_mul(b),
            "/" => a.checked_div(b),
            _ => Some(a.wrapping_rem(b)),
        }
        .map(|r| r as u128)
    } else {
        match op {
            "+" => a.checked_add(b),
            "-" => a.checked_sub(b),
            "*" => a.checked_mul(b),
            "/" => a.checked_div(b),
            _ => a.checked_rem(b),
        }
    };
    r.map_or(Outcome::Overflow, Outcome::Ok)
}
fn less(k: IntegerKind, a: u128, b: u128) -> bool {
    if k.signed() {
        (a as i128) < (b as i128)
    } else {
        a < b
    }
}

/// The census repro: a literal above `i64::MAX` typed as an `i128`.
#[test]
fn census_literal_is_an_i128() {
    agree(
        "fn big() -> i128 { 18408377700990114895 }\nfn main() -> i128 { big() }",
        "18408377700990114895",
    );
    agree(
        "fn main() -> (i128, u128, i128) { let a: i128 = -18446744073709551615; let b: u128 = 18446744073709551615; (a, b, -1) }",
        "(-18446744073709551615, 18446744073709551615, -1)",
    );
}

#[test]
fn binary_operators_agree_at_boundaries() {
    type Reference = fn(IntegerKind, u128, u128) -> Outcome;
    let operators: [(&str, Reference); 13] = [
        ("a + b", |k, a, b| rust_checked(k, "+", a, b)),
        ("a - b", |k, a, b| rust_checked(k, "-", a, b)),
        ("a * b", |k, a, b| rust_checked(k, "*", a, b)),
        ("a / b", |k, a, b| rust_checked(k, "/", a, b)),
        ("a % b", |k, a, b| rust_checked(k, "%", a, b)),
        ("a & b", |_, a, b| Outcome::Ok(a & b)),
        ("a | b", |_, a, b| Outcome::Ok(a | b)),
        ("a ^ b", |_, a, b| Outcome::Ok(a ^ b)),
        ("wrapping_add(a, b)", |_, a, b| {
            Outcome::Ok(a.wrapping_add(b))
        }),
        ("wrapping_sub(a, b)", |_, a, b| {
            Outcome::Ok(a.wrapping_sub(b))
        }),
        ("wrapping_mul(a, b)", |_, a, b| {
            Outcome::Ok(a.wrapping_mul(b))
        }),
        ("if a < b { a } else { b }", |k, a, b| {
            Outcome::Ok(if less(k, a, b) { a } else { b })
        }),
        (
            "if a == b { a } else if a >= b { ~b } else { b }",
            |k, a, b| {
                Outcome::Ok(if a == b {
                    a
                } else if !less(k, a, b) {
                    !b
                } else {
                    b
                })
            },
        ),
    ];
    for k in WIDE {
        let values = boundaries(k);
        let pairs: Vec<(u128, u128)> = values
            .iter()
            .flat_map(|a| values.iter().map(move |b| (*a, *b)))
            .collect();
        let cases: Vec<(String, String)> = pairs
            .iter()
            .map(|(a, b)| (value(k, *a), value(k, *b)))
            .collect();
        for (body, reference) in operators {
            let expected = tuple(
                &pairs
                    .iter()
                    .map(|(a, b)| outcome(k, reference(k, *a, *b)))
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
fn negation_agrees() {
    for k in WIDE {
        let values = boundaries(k);
        let cases: Vec<(String, String)> =
            values.iter().map(|a| (value(k, *a), value(k, 0))).collect();
        let expected = tuple(
            &values
                .iter()
                .map(|a| {
                    let r = if k.signed() {
                        (*a as i128).checked_neg().map(|r| r as u128)
                    } else {
                        (*a == 0).then_some(0)
                    };
                    outcome(k, r.map_or(Outcome::Overflow, Outcome::Ok))
                })
                .collect::<Vec<_>>(),
        );
        agree(
            &program(k.name(), k.name(), k.name(), "-a", &cases),
            &expected,
        );
    }
}

#[test]
fn shifts_and_rotations_agree() {
    for k in WIDE {
        let values = boundaries(k);
        let amounts: [u32; 7] = [0, 1, 63, 64, 65, 127, 128];
        let pairs: Vec<(u128, u32)> = values
            .iter()
            .flat_map(|a| amounts.iter().map(move |n| (*a, *n)))
            .collect();
        let cases: Vec<(String, String)> = pairs
            .iter()
            .map(|(a, n)| (value(k, *a), n.to_string()))
            .collect();
        type Reference = fn(IntegerKind, u128, u32) -> Outcome;
        let operators: [(&str, Reference); 4] = [
            ("a << b", |_, a, n| {
                a.checked_shl(n).map_or(Outcome::Overflow, Outcome::Ok)
            }),
            ("a >> b", |k, a, n| {
                if n >= 128 {
                    Outcome::Overflow
                } else if k.signed() {
                    Outcome::Ok(((a as i128) >> n) as u128)
                } else {
                    Outcome::Ok(a >> n)
                }
            }),
            ("rotate_left(a, b)", |_, a, n| Outcome::Ok(a.rotate_left(n))),
            ("rotate_right(a, b)", |_, a, n| {
                Outcome::Ok(a.rotate_right(n))
            }),
        ];
        for (body, reference) in operators {
            let expected = tuple(
                &pairs
                    .iter()
                    .map(|(a, n)| outcome(k, reference(k, *a, *n)))
                    .collect::<Vec<_>>(),
            );
            agree(&program(k.name(), "u32", k.name(), body, &cases), &expected);
        }
    }
    // A 128-bit amount shifts a narrow value.
    agree(
        "fn sh(x: u64, n: u128) -> u64 eff [Fail[ArithOverflow]] { x << n }
fn main() -> u64 { handle Fail[ArithOverflow] in { sh(1, 3) } with { return v => v, raise(e, k) => 0, } }",
        "8",
    );
}

/// Every value of `from` (by its pattern) as an exact `i128`-or-more.
fn exact(from: IntegerKind, pattern: u128) -> (bool, u128) {
    // (negative, pattern sign-extended to 128 bits)
    if from.signed() {
        let shift = 128 - from.bits();
        let v = ((pattern << shift) as i128) >> shift;
        (v < 0, v as u128)
    } else {
        (false, pattern)
    }
}
fn fits(to: IntegerKind, (negative, p): (bool, u128)) -> bool {
    if negative {
        to.signed() && (p as i128) >= to.min()
    } else {
        p <= to.max_u128()
    }
}
fn low(to: IntegerKind, p: u128) -> u128 {
    if to.bits() == 128 {
        p
    } else {
        p & ((1u128 << to.bits()) - 1)
    }
}
fn one_word_boundaries(k: IntegerKind) -> Vec<u128> {
    let mut v: Vec<u128> = [k.min(), k.min() + 1, -1, 0, 1, k.max() - 1, k.max()]
        .into_iter()
        .filter(|n| k.fits(*n))
        .map(|n| low(k, n as u128))
        .collect();
    v.dedup();
    v
}
fn show_any(k: IntegerKind, p: u128) -> String {
    let (negative, x) = exact(k, p);
    if negative {
        (x as i128).to_string()
    } else {
        x.to_string()
    }
}

#[test]
fn conversions_agree_with_every_kind() {
    let all: Vec<IntegerKind> = INTEGER_KINDS.into_iter().chain(WIDE).collect();
    for from in all.iter().copied() {
        let values = if from.wide() {
            boundaries(from)
        } else {
            one_word_boundaries(from)
        };
        for to in all.iter().copied().filter(|t| from.wide() || t.wide()) {
            let mut conversions = vec!["narrow_", "wrapping_"];
            if from.widens_to(to) {
                conversions.push("widen_");
            }
            for conversion in conversions {
                let source = |p: u128| {
                    if from.wide() {
                        value(from, p)
                    } else {
                        // One-word values are literals of the kind.
                        show_any(from, p)
                    }
                };
                let cases: Vec<(String, String)> =
                    values.iter().map(|p| (source(*p), "0".into())).collect();
                let expected = tuple(
                    &values
                        .iter()
                        .map(|p| {
                            let e = exact(from, *p);
                            if conversion == "wrapping_" || fits(to, e) {
                                format!("(0, {})", show_any(to, low(to, e.1)))
                            } else {
                                "(1, 0)".into()
                            }
                        })
                        .collect::<Vec<_>>(),
                );
                let body = format!("{conversion}{}(a)", to.name());
                agree(
                    &program(from.name(), "u8", to.name(), &body, &cases),
                    &expected,
                );
            }
        }
    }
}

#[test]
fn checked_and_bit_builtins_agree() {
    for k in WIDE {
        let n = k.name();
        let values = boundaries(k);
        let pairs: Vec<(u128, u128)> = values
            .iter()
            .flat_map(|a| values.iter().map(move |b| (*a, *b)))
            .collect();
        let cases: Vec<(String, String)> = pairs
            .iter()
            .map(|(a, b)| (value(k, *a), value(k, *b)))
            .collect();
        for (op, name) in [("+", "add"), ("-", "sub"), ("*", "mul")] {
            let body = format!(
                "match checked_{name}_{n}(a, b) {{ Option::Some(v) => v, Option::None => {{ let one: {n} = 1; let zero: {n} = 0; one / zero }} }}"
            );
            let expected = tuple(
                &pairs
                    .iter()
                    .map(|(a, b)| {
                        outcome(
                            k,
                            match rust_checked(k, op, *a, *b) {
                                Outcome::Overflow => Outcome::DivisionByZero,
                                o => o,
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            );
            agree(&program(n, n, n, &body, &cases), &expected);
        }
        let unary: Vec<(String, String)> =
            values.iter().map(|a| (value(k, *a), "0".into())).collect();
        type Reference = fn(IntegerKind, u128, u32) -> u128;
        let ops: [(&str, &str, Reference); 6] = [
            ("leading_zeros", "u32", |_, a, _| a.leading_zeros().into()),
            ("trailing_zeros", "u32", |_, a, _| a.trailing_zeros().into()),
            ("count_ones", "u32", |_, a, _| a.count_ones().into()),
            ("wrapping_neg", "K", |_, a, _| a.wrapping_neg()),
            ("wrapping_shl", "K", |_, a, n| a.wrapping_shl(n)),
            ("wrapping_shr", "K", |k, a, n| {
                if k.signed() {
                    (a as i128).wrapping_shr(n) as u128
                } else {
                    a.wrapping_shr(n)
                }
            }),
        ];
        for (op, result, reference) in ops {
            let result_kind = if result == "K" { k } else { IntegerKind::U32 };
            let rk = result_kind.name();
            let shifts = op.starts_with("wrapping_sh");
            for amount in if shifts {
                vec![0u32, 1, 64, 127, 128, 200]
            } else {
                vec![0]
            } {
                let body = if shifts {
                    format!("{op}_{n}(a, {amount})")
                } else {
                    format!("{op}_{n}(a)")
                };
                let expected = tuple(
                    &values
                        .iter()
                        .map(|a| {
                            format!("(0, {})", show_any(result_kind, reference(k, *a, amount)))
                        })
                        .collect::<Vec<_>>(),
                );
                agree(&program(n, "u8", rk, &body, &unary), &expected);
            }
        }
    }
}

#[test]
fn literals_patterns_and_aggregates() {
    // Literal patterns, a struct field, a tuple and a sequence element.
    agree(
        &format!(
            "{WORDS}copy struct Row {{ tag: u8, n: i128, m: u128 }}
fn pick(x: i128) -> u8 {{ match x {{ -1 => 10, 0 => 20, 18408377700990114895 => 30, _ => 40 }} }}
fn bump(r: Row) -> Row {{ Row {{ tag: r.tag, n: wrapping_add(r.n, 1), m: r.m >> 1 }} }}
fn main() -> (u8, u8, u8, u8, Row, (i128, u128)) {{
    let a: i128 = -1;
    let r = bump(Row {{ tag: 7, n: w_i128(9223372036854775808, 0), m: w_u128(18446744073709551615, 0) }});
    (pick(a), pick(0), pick(18408377700990114895), pick(r.n), r, (r.n, r.m))
}}"
        ),
        "(10, 20, 30, 40, Row { m: 170141183460469231722463931679029329920, n: -170141183460469231731687303715884105727, tag: 7 }, (-170141183460469231731687303715884105727, 170141183460469231722463931679029329920))",
    );
}

#[test]
fn checker_rules() {
    // Widening must be lossless both ways round.
    rejects("fn f(a: u128) -> i128 { widen_i128(a) }", "E0200");
    rejects("fn f(a: i128) -> u128 { widen_u128(a) }", "E0200");
    rejects("fn f(a: i8) -> u128 { widen_u128(a) }", "E0200");
    rejects("fn f(a: i128) -> i64 { widen_i64(a) }", "E0200");
    accepted(
        "fn f(a: u64, b: i64, c: usize) -> (u128, i128, i128) { (widen_u128(a), widen_i128(b), widen_i128(c)) }",
    );
    // No implicit conversions.
    rejects("fn f(a: u128) -> i128 { a }", "E0200");
    rejects("fn f(a: i64) -> i128 { a }", "E0200");
    // A negative literal is no u128.
    rejects("fn f() -> u128 { -1 }", "E0209");
    // Arithmetic may overflow; the operators that cannot need no effect.
    rejects("fn f(a: u128, b: u128) -> u128 { a + b }", "E0401");
    rejects("fn f(a: i128) -> i64 { narrow_i64(a) }", "E0401");
    accepted("fn f(a: u128, b: u128) -> u128 { wrapping_mul(a, b) ^ (a & b) }");
    accepted("fn f() -> u128 { let a: u128 = 18446744073709551615; a * 3 }");
    // No float conversions yet.
    rejects("fn f(a: i128) -> f64 { nearest_f64(a) }", "E0200");
    rejects("fn f(a: f64) -> i128 { saturating_i128(a) }", "E0200");
    rejects(
        "fn f(a: f64) -> u128 eff [Fail[ArithOverflow]] { narrow_u128(a) }",
        "E0200",
    );
    // No 128-bit atomics.
    let c = checked(
        "fn f() -> u128 { let c: Atomic[u128] = Atomic::new(5); c.load(MemoryOrder::Relaxed) }",
    );
    assert!(!c.diagnostics.is_empty(), "Atomic[u128] was accepted");
}

#[test]
fn sequences_hold_wide_elements() {
    agree(
        &format!(
            "{WORDS}fn f() -> (u128, u128, u64) eff [Fail[RuntimeError]] {{
    let mut s: ChunkSeq[u128] = ChunkSeq::new[u128]();
    s.push(w_u128(1, 2));
    s.push(w_u128(18446744073709551615, 3));
    s.push(7);
    (s.get(0), s.get(1), s.len())
}}
fn main() -> (u128, u128, u64) {{ handle Fail[RuntimeError] in {{ f() }} with {{ return v => v, raise(e, k) => (0, 0, 0), }} }}"
        ),
        "(18446744073709551618, 340282366920938463444927863358058659843, 3)",
    );
}

#[test]
fn export_abi_passes_wide_integers_by_pointer() {
    // SH4 lane 3 (#1277) gave 128-bit integers a two-word export cell;
    // tests/wide_aggregates_export.rs drives it from C.
    use vesper::native::export::{LibraryOptions, compile_source};
    let options = LibraryOptions {
        library: "wide",
        target: None,
        optimize: true,
    };
    for source in [
        "pub fn f(a: i128) -> u64 { wrapping_u64(a) }",
        "pub fn f(a: u64) -> u128 { widen_u128(a) }",
    ] {
        let interface = compile_source("wide.ves", source, &options)
            .unwrap_or_else(|e| panic!("{source}: {e}"))
            .interface;
        let f = &interface.functions[0];
        assert!(f.result_pointer || f.params[0].pointer, "{source}: {f:?}");
    }
}

/// An executable's exit status matches the evaluator on a digest of every
/// operator (Unix hosts with a C compiler, as the other executable tests).
#[test]
fn executable_agrees_with_the_evaluator() {
    use std::process::Command;
    if !cfg!(unix)
        || !Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let source = format!(
        "{WORDS}fn mix(h: u128, x: u128) -> u128 {{ wrapping_mul(h ^ x, w_u128(1099511628211, 14695981039346656037)) }}
fn digest(n: u64) -> u128 eff [Fail[ArithOverflow], Fail[RuntimeError]] {{
    let mut h: u128 = 7;
    for i in 0..n {{
        let a: u128 = (widen_u128(i) << 70) | widen_u128(n);
        let b: i128 = wrapping_i128(a) >> 3;
        let c: i128 = b / wrapping_i128(widen_u128(i) + 3);
        let d: u128 = a % (widen_u128(i) + 1);
        h = mix(h, wrapping_u128(c) + d);
        h = mix(h, rotate_left(a, i) ^ widen_u128(count_ones_u128(a)));
        if wrapping_i128(h) < c {{ h = mix(h, 1); }}
    }}
    h
}}
fn main() -> i64 {{
    handle Fail[RuntimeError] in {{
        handle Fail[ArithOverflow] in {{ wrapping_i64(digest(300) >> 57) & 127 }} with {{ return v => v, raise(e, k) => 126, }}
    }} with {{ return v => v, raise(e, k) => 125, }}
}}"
    );
    let c = accepted(&source);
    let mut runtime = Runtime::new(&c.world);
    let expected = runtime.run("main", vec![]).unwrap().to_string();
    let jit = native::jit_with_options(&c.world, "main", true)
        .unwrap()
        .to_string();
    assert_eq!(jit, expected);
    let dir = std::env::temp_dir().join(format!("vesper-wide-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("digest.ves");
    std::fs::write(&file, &source).unwrap();
    let exe = dir.join("digest");
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let status = Command::new(&exe).output().unwrap().status.code();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(status.map(|s| s.to_string()), Some(expected));
}

/// Shifts by constant amounts take the two-word path in native code.
#[test]
fn constant_shifts_agree() {
    for k in WIDE {
        for p in boundaries(k) {
            let amounts = [1u32, 7, 63, 64, 65, 100, 127];
            let body: Vec<String> = amounts
                .iter()
                .flat_map(|n| [format!("x << {n}"), format!("x >> {n}")])
                .collect();
            let expected: Vec<String> = amounts
                .iter()
                .flat_map(|n| {
                    let right = if k.signed() {
                        ((p as i128) >> n) as u128
                    } else {
                        p >> n
                    };
                    [show(k, p << n), show(k, right)]
                })
                .collect();
            let n = k.name();
            agree(
                &format!(
                    "{WORDS}fn main() -> ({}) {{ let x: {n} = {}; ({}) }}",
                    vec![n; body.len()].join(", "),
                    value(k, p),
                    body.join(", ")
                ),
                &tuple(&expected),
            );
        }
    }
}

/// Corners from review: rotation amounts past `i128::MAX`, a negated
/// `i128::MIN` fact, `Never` placeholders and region boxes.
#[test]
fn review_corners() {
    let k = IntegerKind::U128;
    let amounts = [1u128 << 127, (1 << 127) + 3, u128::MAX, 200];
    for a in [1u128, 0x8000_0000_0000_0001] {
        for n in amounts {
            agree(
                &format!(
                    "{WORDS}fn r(a: u128, n: u128) -> u128 {{ rotate_left(a, n) }}
fn q(a: u64, n: u128) -> u64 {{ rotate_right(a, n) }}
fn main() -> (u128, u64) {{ (r({}, {}), q({}, {})) }}",
                    value(k, a),
                    value(k, n),
                    a as u64,
                    value(k, n)
                ),
                &format!(
                    "({}, {})",
                    a.rotate_left((n % 128) as u32),
                    (a as u64).rotate_right((n % 64) as u32)
                ),
            );
        }
    }
    agree(
        "fn main() -> i128 { handle Fail[ArithOverflow] in { let a: i128 = 9223372036854775808; let b = wrapping_mul(a, a); let m = wrapping_add(b, b); -m } with { return v => v, raise(e, k) => 7, } }",
        "7",
    );
    agree(
        "fn f(b: bool) -> i128 { if b { return 1; } else { 2 } }
fn main() -> i128 { wrapping_add(wrapping_mul(f(true), 10), f(false)) }",
        "12",
    );
    agree(
        "fn main() -> u128 { region r { let x: u128 = 18446744073709551615; let y = wrapping_add(x, x); let b = r.box(y); r.get(b) } }",
        "36893488147419103230",
    );
}
