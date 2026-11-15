//! Flat syntax transport, recovery and the Vesper-only region adapter.
#[path = "fixtures/self_host/driver.rs"]
mod support;
use support::*;
use vesper::{
    parser,
    runtime::{Runtime, Value},
};

fn text(program: &Program, message: i64, a: i64, b: i64) -> String {
    let mut runtime = Runtime::new(&program.world);
    runtime.optimize = false;
    let Value::Int(len) = runtime
        .run(
            "syntax_message_len",
            vec![Value::Int(message), Value::Int(a), Value::Int(b)],
        )
        .unwrap()
    else {
        panic!("message length")
    };
    let bytes: Vec<_> = (0..len)
        .map(|i| {
            let Value::Int(byte) = runtime
                .run(
                    "syntax_message_byte",
                    vec![
                        Value::Int(message),
                        Value::Int(a),
                        Value::Int(b),
                        Value::Int(i),
                    ],
                )
                .unwrap()
            else {
                panic!("message byte")
            };
            u8::try_from(byte).unwrap()
        })
        .collect();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn recovery_diagnostics_match_the_seed() {
    on_big_stack(|| {
        let front = read_front_end();
        for source in [
            "fn f() { let a = ; let b = ; } fn g() { let c = ; }",
            "fn f(x i64) { let y = ; } fn good() {}",
            "fn f() { & } fn g() { & }",
            "fn é() {} fn g() { & }",
            "fn f() { \"open }",
            "fn f() {} /* open",
            "trait T[A: Tr, B: Other] {}",
            "impl[A: Tr, B: Other] A {}",
            "model M: Tr X {}",
            "impl fn() for X {}",
            "struct X { f: i64 @@ }",
            "fn f[e]() {}",
            "fn f() -> A[fn() | e] {}",
            "fn f() { match 1 { -true => 0, -thing => 1, } }",
            "trait T { huh fn f(in self); type A mod copy; }",
            "fn f() -> String { \"\\q\" } fn g() -> u64 { 18446744073709551616 }",
        ] {
            let expected = parser::parse(source).diagnostics;
            assert!(!expected.is_empty(), "{source}");
            let program = program(&front, source.as_bytes());
            let (outcome, cells) = transport(&program);
            assert!(matches!(decode(&outcome), Outcome::Error { .. }));
            let actual: Vec<_> = (0..cells[2] as usize)
                .map(|i| {
                    let d = &cells[1000000 + i * 6..1000000 + i * 6 + 6];
                    (
                        format!("E{:04}", d[0]),
                        text(&program, d[1], d[4], d[5]),
                        d[2] as usize..d[3] as usize,
                    )
                })
                .collect();
            let expected: Vec<_> = expected
                .into_iter()
                .map(|d| (d.code, d.message, d.span))
                .collect();
            assert_eq!(
                actual.first(),
                expected.first(),
                "first diagnostic: {source}"
            );
            for diagnostic in &expected {
                assert!(
                    actual.contains(diagnostic),
                    "missing {diagnostic:?} in {actual:?}: {source}"
                );
            }
        }
    });
}

#[test]
fn traits_models_projections_unicode_and_row_shadowing() {
    on_big_stack(|| {
        let front = read_front_end();
        for source in [
            "pub trait Tr[T: mod copy, e: eff] { type A: mod copy; fn f(in self, x: T::A) -> <T as Tr>::A eff [e]; } model ByX[T: Tr]: Tr[T] for T where T: Tr { type A = i64; fn f(in self, x: T::A) -> <T as Tr>::A { x } } use model ByX;",
            "impl @scope/codec::Codec for Item { type Out = Item; fn go(in self) -> Item { self } }",
            "fn f[e: eff](xs: (i64, i64), i: i64) -> i64 { xs[i] }",
            "fn f[e: eff]() { handle E state foo = foo[e]() in {} with { return x => x, op(y, k) => k(y), } }",
            "fn f[e: eff](e: i64, xs: (i64, i64)) -> i64 { let get = |values: (i64, i64)| values[e]; match Some(xs) { Some(values) => values[e], None => 0, } }",
            "fn f[e: eff](xs: (i64, i64)) { let e = 0; xs[e]; helper[e](); { let xs = 1; helper[e](); } }",
            "fn f() -> String { \"\\u0000\\u0041\\u00e9\\uD834\\uDD1E\" }",
        ] {
            let expected = parser::parse(source);
            assert!(
                expected.diagnostics.is_empty(),
                "{source}: {:?}",
                expected.diagnostics
            );
            let program = program(&front, source.as_bytes());
            let (outcome, cells) = transport(&program);
            assert!(
                matches!(decode(&outcome), Outcome::Digest(_)),
                "{source}: {outcome:?}"
            );
            assert_flat(source, &cells);
        }
        for escape in [
            "\\uD800",
            "\\uDC00",
            "\\uD800\\u0041",
            "\\u0z01",
            "\\u123",
            "\\u{41}",
        ] {
            let source = format!("fn f() -> String {{ \"{escape}\" }}");
            let program = program(&front, source.as_bytes());
            let (outcome, cells) = transport(&program);
            assert!(
                matches!(decode(&outcome), Outcome::Error { code: 4, .. }),
                "{source}"
            );
            assert_eq!(
                text(&program, cells[1000001], 0, 0),
                "invalid string escape"
            );
        }
    });
}

#[test]
fn region_adapter_matches_external_storage() {
    on_big_stack(|| {
        let front = read_front_end();
        for source in [
            "fn f() -> i64 { 42 }",
            "trait T { type A; fn f(in self); }",
            "fn f() { let x = ; let y = ; }",
        ] {
            let program = program(&front, source.as_bytes());
            let (expected, _) = transport(&program);
            let mut runtime = Runtime::new(&program.world);
            runtime.optimize = false;
            runtime.fuel = u64::MAX / 2;
            assert_eq!(
                decode(&runtime.run("parse_main", vec![]).unwrap()),
                decode(&expected),
                "{source}"
            );
        }
    });
}

#[test]
fn truncated_declarations_keep_the_seed_first_diagnostic() {
    on_big_stack(|| {
        let front = read_front_end();
        for whole in [
            "trait T[A: mod copy] { type Out: mod copy; fn f(in self) -> A; }",
            "model M: T for X { type Out = X; fn f(in self) -> X { self } }",
            "fn f[e: eff](xs: (i64, i64), i: i64) -> i64 { xs[i] }",
            "fn f[e: eff]() { handle E state foo = foo[e]() in {} with { return x => x, op(y, k) => k(y), } }",
            "fn f() -> String { \"\\uD834\\uDD1E\" }",
        ] {
            for end in (0..=whole.len())
                .step_by(7)
                .chain(std::iter::once(whole.len()))
            {
                let source = &whole[..end];
                let expected = parser::parse(source);
                let program = program(&front, source.as_bytes());
                let (outcome, cells) = transport(&program);
                if let Some(expected) = expected.diagnostics.first() {
                    assert!(
                        matches!(decode(&outcome), Outcome::Error { .. }),
                        "{source}"
                    );
                    assert!(cells[2] > 0, "missing diagnostic: {source}");
                    let d = &cells[1000000..1000006];
                    assert_eq!(
                        (
                            format!("E{:04}", d[0]),
                            text(&program, d[1], d[4], d[5]),
                            d[2] as usize..d[3] as usize
                        ),
                        (
                            expected.code.clone(),
                            expected.message.clone(),
                            expected.span.clone()
                        ),
                        "{source}"
                    );
                } else {
                    assert!(matches!(decode(&outcome), Outcome::Digest(_)), "{source}");
                    assert_flat(source, &cells);
                }
            }
        }
    });
}

#[test]
fn storage_bounds_and_little_endian_byte_view() {
    on_big_stack(|| {
        let front = format!(
            "{}\n{}",
            read_front_end(),
            r#"
fn storage_probe() -> i64 eff [SyntaxMemory, Fail[Diag], Fail[ArithOverflow]] {
    syntax_put(0, 513);
    syntax_put(1048575, 4294967295);
    if syntax_cell(0) == 513 && syntax_byte(0, 0) == 1 && syntax_byte(0, 1) == 2 && syntax_cell(1048575) == 4294967295 && syntax_byte(1048575, 3) == 255 && syntax_byte(1048575, 7) == 0 { 42 } else { 0 }
}
fn bad_index() -> i64 eff [SyntaxMemory, Fail[Diag], Fail[ArithOverflow]] { syntax_cell(-1) }
fn bad_write() -> i64 eff [SyntaxMemory, Fail[Diag], Fail[ArithOverflow]] { syntax_put(1048576, 0); 42 }
fn bad_byte() -> i64 eff [SyntaxMemory, Fail[Diag], Fail[ArithOverflow]] { syntax_byte(0, 8) }
"#
        );
        for (probe, expected) in [
            ("storage_probe", Outcome::Digest(42)),
            ("bad_index", Outcome::Error { code: 5, at: 0 }),
            ("bad_write", Outcome::Error { code: 5, at: 0 }),
            ("bad_byte", Outcome::Error { code: 5, at: 0 }),
        ] {
            let front = format!(
                "{front}\nfn storage_entry() -> i64 {{ handle Fail[ArithOverflow] in {{ handle Fail[Diag] in {{ syntax_run({probe}) }} with {{ return v => v, raise(error, resume) => diag_code(error), }} }} with {{ return v => v, raise(error, resume) => -1, }} }}"
            );
            let program = program(&front, b"");
            let mut runtime = Runtime::new(&program.world);
            runtime.optimize = false;
            runtime.fuel = u64::MAX / 2;
            assert_eq!(
                decode(&runtime.run("storage_entry", vec![]).unwrap()),
                expected,
                "{probe}"
            );
        }
    });
}
