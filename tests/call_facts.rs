//! Facts across calls (self-hosting wave SH3 lane 6, #1245).
//!
//! - A function whose returns are all bounded has a result range, which its
//!   call sites use as the bounds of the call: `128 / bits(x)` needs no
//!   `Fail` when `bits` returns only 8 or 64.
//! - `u128` constant facts cover the whole `u128` range, not only the `i128`
//!   domain.
//! - A checked `u128` multiply whose operand is a widened one-word value
//!   runs a cheaper overflow check (native code and the JIT).
//!
//! Every accepted program agrees in the evaluator and the JIT (optimized and
//! not) and compiles to x86-64 and AArch64 objects; negative tests pin that
//! no `Fail` is dropped where a divisor can be zero or a value can overflow.
use vesper::{check, native, parser, runtime::Runtime};

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
        c.diagnostics
            .iter()
            .any(|d| code.split('|').any(|code| d.code == code)),
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

const BITS: &str = "fn bits(x: u8) -> u64 { if x == 1 { 8 } else { 64 } }\n";

#[test]
fn callee_result_range_is_a_divisor_fact() {
    // The SH2 lane 9 repro (gap 1).
    agree(
        &format!(
            "{BITS}fn lanes(x: u8) -> u64 {{ 128 / bits(x) }}\nfn main() -> u64 {{ wrapping_add(wrapping_mul(lanes(1), 100), lanes(2)) }}"
        ),
        "1602",
    );
    // Through a `let`, a remainder and a nested call.
    agree(
        &format!(
            "{BITS}fn twice(x: u8) -> u64 {{ bits(x) }}
fn main() -> u64 {{ let b = twice(3); let r = 1000 % twice(1); wrapping_add(1024 / b, r) }}"
        ),
        "16",
    );
}

#[test]
fn result_ranges_follow_every_return_shape() {
    let cases = [
        // `match` arms and a wildcard.
        (
            "fn d(x: u64) -> u64 { match x { 0 => 2, 1 => 4, _ => 8 } }",
            "d(1)",
            "25",
        ),
        // A constant guard on a parameter that nothing writes.
        (
            "fn d(x: u64) -> u64 { if x == 0 { 1 } else { x } }",
            "d(0)",
            "100",
        ),
        (
            "fn d(x: u64) -> u64 { if x < 3 { 3 } else { x } }",
            "d(50)",
            "2",
        ),
        (
            "fn d(x: u64) -> u64 { if 10 >= x { 10 } else { x } }",
            "d(4)",
            "10",
        ),
        // An early `return` and the tail.
        (
            "fn d(x: u8) -> u64 { if x > 3 { return 4; } 2 }",
            "d(9)",
            "25",
        ),
        // An immutable `let`, a mask and a quotient of bounded values.
        (
            "fn d(x: u64) -> u64 { let m = (x & 7) % 4; if m == 0 { 5 } else { m } }",
            "d(6)",
            "50",
        ),
        (
            "fn e(x: u8) -> u64 { if x == 1 { 8 } else { 64 } }\nfn d(x: u8) -> u64 { 64 / e(x) }",
            "d(1)",
            "12",
        ),
        // A widening keeps its operand's bounds.
        (
            "fn d(x: u8) -> u64 { if x == 0 { 1 } else { widen_u64(x) } }",
            "d(4)",
            "25",
        ),
        // A module constant.
        (
            "const K: u64 = 20;\nfn d(x: u64) -> u64 { if x == 0 { K } else { 25 } }",
            "d(0)",
            "5",
        ),
    ];
    for (decl, call, expected) in cases {
        agree(
            &format!("{decl}\nfn main() -> u64 {{ 100 / {call} }}"),
            expected,
        );
    }
}

#[test]
fn result_ranges_bound_shift_amounts() {
    agree(
        "fn amount(x: u8) -> u32 { if x == 0 { 3 } else { 63 } }
fn main() -> u64 { let one: u64 = 1; (one << amount(0)) | (one << amount(1) >> 60) }",
        "8",
    );
}

#[test]
fn no_fact_where_a_divisor_can_be_zero() {
    let cases = [
        // A zero return.
        "fn d(x: u8) -> u64 { if x == 1 { 0 } else { 64 } }",
        // An unbounded parameter: summaries ignore the call's arguments.
        "fn d(x: u64) -> u64 { x }",
        // Recursion gives no fact.
        "fn d(x: u64) -> u64 { if x == 0 { 1 } else { d(x / 2) } }",
        // Mutual recursion.
        "fn e(x: u64) -> u64 { if x == 0 { 1 } else { d(x / 2) } }\nfn d(x: u64) -> u64 { e(x) }",
        // A `return` of an unguarded parameter.
        "fn d(x: u64) -> u64 { if x == 0 { return x; } 1 }",
        // A `return` of a name a `let` rebinds.
        "fn d(x: u8) -> u64 { let x: u64 = 0; if x == 0 { return x; } 5 }",
        // A guard on a name the branch then writes.
        "fn d(x: u64) -> u64 { let mut y: u64 = x; if y != 0 { y = 0; y } else { 1 } }",
        // A guard on a name lent `inout`.
        "fn zero(inout v: u64) { v = 0; }\nfn d(x: u64) -> u64 { let mut y: u64 = x; if y != 0 { zero(inout y); y } else { 1 } }",
        // The same with the mode inferred from the callee (codex review).
        "fn zero(inout v: u64) { v = 0; }\nfn d(x: u64) -> u64 { let mut y: u64 = x; if y != 0 { zero(y); y } else { 1 } }",
        // A call of a local closure may write what it is passed.
        "fn d(x: u64) -> u64 { let mut y: u64 = x; let f = |inout v: u64| { v = 0; }; if y != 0 { f(y); y } else { 1 } }",
        // A narrowing that may fail: a handler may resume it with any `u8`.
        "fn d(x: u64) -> u8 eff [Fail[ArithOverflow]] { if x == 0 { 1 } else { narrow_u8(x) } }",
        // A loop's value is not bounded.
        "fn d(x: u64) -> u64 { loop { break 4; } }",
        // A `-` is not bounded (it may underflow and be resumed).
        "fn d(x: u8) -> u64 eff [Fail[ArithOverflow]] { if x == 0 { 1 } else { 9 - widen_u64(x) } }",
    ];
    for decl in cases {
        let ret = if decl.contains("-> u8") { "u8" } else { "u64" };
        // Only the division's `Fail[RuntimeError]` is left unhandled (E0400
        // inside the `Fail[ArithOverflow]` handler).
        rejects(
            &format!(
                "{decl}\nfn main() -> {ret} {{ handle Fail[ArithOverflow] in {{ 100 / d(1) }} with {{ return v => v, raise(e, _) => 1, }} }}"
            ),
            "E0400|E0401",
        );
        // The `Fail` is a `RuntimeError` the program must declare.
        let c = checked(&format!(
            "{decl}\nfn main() -> {ret} eff [Fail[ArithOverflow], Fail[RuntimeError]] {{ 100 / d(1) }}"
        ));
        assert!(c.diagnostics.is_empty(), "{decl}: {:?}", c.diagnostics);
    }
}

#[test]
fn local_bindings_and_constructors_shadow_functions() {
    // A closure named like a function: the call is the closure's.
    rejects(
        &format!("{BITS}fn main() -> u64 {{ let bits = |x: u64| -> u64 {{ x }}; 128 / bits(0) }}"),
        "E0401",
    );
}

const BIG: &str = "18446744073709551615";

#[test]
fn u128_facts_cover_the_whole_range() {
    // The SH2 lane 2 repro (gap 2): (2^64 - 1)^2 = 2^128 - 2^65 + 1.
    agree(
        &format!("fn main() -> u128 {{ let a: u128 = {BIG}; a * a }}"),
        &(u128::from(u64::MAX) * u128::from(u64::MAX)).to_string(),
    );
    // Facts above i128::MAX compose: a sum, a difference, a quotient, and
    // the wrapping builtins.
    let p = u128::from(u64::MAX) * u128::from(u64::MAX);
    agree(
        &format!(
            "fn main() -> u128 {{ let a: u128 = {BIG}; let p = a * a; let q = p + {BIG}; q - p / 3 + wrapping_add(p, p) / p }}"
        ),
        &((p + u128::from(u64::MAX)) - p / 3 + p.wrapping_add(p) / p).to_string(),
    );
}

#[test]
fn u128_facts_keep_every_real_failure() {
    let pre = format!("let a: u128 = {BIG}; let p = a * a;");
    for (body, code) in [
        // Past u128::MAX.
        ("p * 2", "E0401"),
        ("p + p", "E0401"),
        ("p + a * 2 + 1", "E0401"),
        // Below zero.
        ("a - p", "E0401"),
        // A zero divisor.
        ("p / (p - p)", "E0401"),
        ("p % (a - a)", "E0401"),
    ] {
        rejects(&format!("fn main() -> u128 {{ {pre} {body} }}"), code);
    }
    // A value above i128::MAX is not a bound: narrowing it may fail.
    for (to, body) in [
        ("i128", "narrow_i128(p)"),
        ("u64", "narrow_u64(p)"),
        ("i64", "narrow_i64(p)"),
    ] {
        rejects(&format!("fn main() -> {to} {{ {pre} {body} }}"), "E0401");
    }
    // A shift by a fact above i128::MAX is not proven in range.
    rejects(
        &format!("fn main() -> u64 {{ {pre} let one: u64 = 1; one << p }}"),
        "E0401",
    );
    // -p of a nonzero u128 overflows.
    rejects(&format!("fn main() -> u128 {{ {pre} -p }}"), "E0401");
    agree(
        &format!("fn main() -> u128 {{ let z: u128 = {BIG} - {BIG}; -z }}"),
        "0",
    );
}

/// `a * widen_u128(b)` boundary values: the cheaper check must raise exactly
/// where the full one does.
#[test]
fn widened_multiply_agrees_at_the_boundaries() {
    let words: [u128; 7] = [
        0,
        1,
        2,
        u128::from(u64::MAX),
        1 << 64,
        (1 << 64) + 1,
        u128::MAX,
    ];
    let small: [u64; 5] = [0, 1, 2, 1 << 63, u64::MAX];
    let mut source = String::from(
        "fn w(hi: u64, lo: u64) -> u128 { (widen_u128(hi) << 64) | widen_u128(lo) }
fn f(a: u128, b: u64) -> u128 eff [Fail[ArithOverflow]] { a * widen_u128(b) }
fn g(a: u128, b: u64) -> u128 eff [Fail[ArithOverflow]] { widen_u128(b) * a }
fn h(a: u64, b: u64) -> u128 eff [Fail[ArithOverflow]] { widen_u128(a) * widen_u128(b) }
fn probe(a: u128, b: u64, which: u64) -> u128 {
    handle Fail[ArithOverflow] in {
        if which == 0 { f(a, b) } else if which == 1 { g(a, b) } else { h(narrow_u64(a), b) }
    } with {
        return value => value,
        raise(error, _) => 7,
    }
}
fn mix(h: u128, x: u128) -> u128 { wrapping_mul(h ^ x, 1099511628211) }
fn main() -> u128 {
    let mut d: u128 = 0;
",
    );
    let mut expected: u128 = 0;
    let mix = |h: u128, x: u128| (h ^ x).wrapping_mul(1_099_511_628_211);
    for a in words {
        for b in small {
            for which in 0..3u64 {
                if which == 2 && a > u128::from(u64::MAX) {
                    continue;
                }
                let r = a.checked_mul(u128::from(b)).unwrap_or(7);
                expected = mix(expected, r);
                source.push_str(&format!(
                    "    d = mix(d, probe(w({}, {}), {b}, {which}));\n",
                    (a >> 64) as u64,
                    a as u64
                ));
            }
        }
    }
    source.push_str("    d\n}\n");
    agree(&source, &expected.to_string());
}

/// The widened multiply emits fewer partial products: two `umulhi` for one
/// widened operand (the generic check has three) and one for two.
#[test]
fn widened_multiply_emits_fewer_partial_products() {
    use std::process::Command;
    let dir = std::env::temp_dir().join(format!("vesper-call-facts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let count = |name: &str, body: &str| {
        let file = dir.join(format!("{name}.ves"));
        std::fs::write(
            &file,
            format!(
                "{body}
fn main() -> u64 {{
    handle Fail[ArithOverflow] in {{ wrapping_u64(f(5, 7)) }} with {{ return v => v, raise(e, _) => 1, }}
}}"
            ),
        )
        .unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .args(["emit", "--emit", "clif"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let clif = String::from_utf8_lossy(&out.stdout).to_string();
        // The function `f`: from its `; f` header to the next header.
        let f: Vec<&str> = clif
            .lines()
            .skip_while(|l| *l != "; f")
            .skip(1)
            .take_while(|l| !l.starts_with("; "))
            .collect();
        assert!(!f.is_empty(), "no `f` in\n{clif}");
        f.iter().filter(|l| l.contains("umulhi")).count()
    };
    let generic = count(
        "generic",
        "fn f(a: u128, b: u128) -> u128 eff [Fail[ArithOverflow]] { a * b }",
    );
    let one = count(
        "one",
        "fn f(a: u128, b: u64) -> u128 eff [Fail[ArithOverflow]] { a * widen_u128(b) }",
    );
    let two = count(
        "two",
        "fn f(a: u64, b: u64) -> u128 eff [Fail[ArithOverflow]] { widen_u128(a) * widen_u128(b) }",
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!((generic, one, two), (3, 2, 1));
}

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
        "{BITS}fn lanes(x: u8) -> u64 {{ 128 / bits(x) }}
fn f(a: u128, b: u64) -> u128 eff [Fail[ArithOverflow]] {{ a * widen_u128(b) }}
fn digest(n: u64) -> u128 eff [Fail[ArithOverflow]] {{
    let mut h: u128 = 7;
    for i in 0..n {{
        let a: u128 = (widen_u128(i) << 50) | widen_u128(lanes(narrow_u8(i & 1)));
        h = wrapping_mul(h ^ f(a, i * 977), 1099511628211);
    }}
    let big: u128 = {BIG};
    h ^ big * big
}}
fn main() -> i64 {{
    handle Fail[ArithOverflow] in {{ wrapping_i64(digest(300) >> 57) & 127 }} with {{ return v => v, raise(e, k) => 126, }}
}}"
    );
    let c = accepted(&source);
    let mut runtime = Runtime::new(&c.world);
    let expected = runtime.run("main", vec![]).unwrap().to_string();
    let jit = native::jit_with_options(&c.world, "main", true)
        .unwrap()
        .to_string();
    assert_eq!(jit, expected);
    let dir = std::env::temp_dir().join(format!("vesper-call-facts-exe-{}", std::process::id()));
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
