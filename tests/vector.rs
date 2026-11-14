//! Source vectors and `eff [Isa[P]]` (design §14, wave-seven lane 28):
//! checker positive and negative cases, exact evaluator semantics, and
//! evaluator/native differential runs of every operation at every shape,
//! with x86_64 and AArch64 object emission.
use vesper::{check, native, parser, runtime::Runtime, vector};

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check::check(&p.module)
}
fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c
}
fn rejected(source: &str, code: &str, text: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(text)),
        "expected {code} containing {text:?}, got {:?}",
        c.diagnostics
    );
}
fn evaluate(source: &str) -> String {
    let c = accepted(source);
    Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string()
}
/// Evaluator and JIT agree; x86_64 (SSE2 baseline flags) and AArch64
/// objects build. Returns the agreed result.
fn differential(source: &str) -> String {
    let c = accepted(source);
    let interpreted = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    let jit = native::jit(&c.world, "main").unwrap().to_string();
    assert_eq!(jit, interpreted, "{source}");
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (object, _) = native::object_for_target(&c.world, "main", Some(target))
            .unwrap_or_else(|e| panic!("{target}: {e}\n{source}"));
        assert_eq!(&object[..4], b"\x7fELF");
    }
    interpreted
}

// ---------------------------------------------------------------- checker

const SUM: &str = "fn sum(a: Vec[u32, 4], b: Vec[u32, 4]) -> u32 eff [Isa[Baseline128]] { Vec::reduce_add(Vec::add(a, b)) }";

#[test]
fn vector_types_are_copy_and_checked() {
    accepted(SUM);
    // Copy: a vector can be used twice.
    accepted("fn twice(a: Vec[u8, 16]) -> Vec[u8, 16] eff [Isa[Baseline128]] { Vec::add(a, a) }");
    rejected(
        "fn f(a: Vec[u16, 8]) -> u64 { 0 }",
        "E1401",
        "lanes must be u8",
    );
    rejected(
        "fn f(a: Vec[u8, 8]) -> u64 { 0 }",
        "E1401",
        "not a 128-, 256- or 512-bit",
    );
    rejected(
        "fn f(a: Mask[f64, 3]) -> u64 { 0 }",
        "E1401",
        "not a 128-, 256- or 512-bit",
    );
    rejected(
        "struct Vec[A, B] { a: A } fn main() -> u64 { 0 }",
        "E1401",
        "reserved for vector types",
    );
}

#[test]
fn operations_require_an_isa_profile() {
    rejected(
        "fn f(a: Vec[u32, 4]) -> u32 { Vec::reduce_add(a) }",
        "E1403",
        "requires 'eff [Isa[Baseline128]]'",
    );
    // 256-bit vectors need Avx2; Baseline128 is not enough.
    rejected(
        "fn f(a: Vec[u32, 8]) -> u32 eff [Isa[Baseline128]] { Vec::reduce_add(a) }",
        "E1403",
        "requires 'eff [Isa[Avx2]]'; only Isa[Baseline128] is available here",
    );
    rejected(
        "fn f(a: Vec[u64, 8]) -> u64 eff [Isa[Avx2]] { Vec::reduce_add(a) }",
        "E1403",
        "requires 'eff [Isa[Avx512]]'",
    );
    // Sve2 grants fixed 128-bit vectors only.
    rejected(
        "fn f(a: Vec[u32, 8]) -> u32 eff [Isa[Sve2]] { Vec::reduce_add(a) }",
        "E1403",
        "Avx2",
    );
    accepted("fn f(a: Vec[u32, 4]) -> u32 eff [Isa[Sve2]] { Vec::reduce_add(a) }");
    accepted("fn f(a: Vec[u32, 8]) -> u32 eff [Isa[Avx512]] { Vec::reduce_add(a) }");
    // Passing vectors around needs no profile; operating on them does.
    accepted("fn id(a: Vec[u8, 64]) -> Vec[u8, 64] { a }");
}

#[test]
fn calls_need_a_wider_or_equal_profile() {
    let callee = "fn k() -> u64 eff [Isa[Avx2]] { Mask::count(Mask::active[u8, 32](0, 5)) }";
    rejected(
        &format!("{callee} fn main() -> u64 {{ k() }}"),
        "E1403",
        "calling 'k' requires 'eff [Isa[Avx2]]'",
    );
    rejected(
        &format!("{callee} fn main() -> u64 eff [Isa[Baseline128]] {{ k() }}"),
        "E1403",
        "calling 'k'",
    );
    rejected(
        &format!("{callee} fn main() -> u64 eff [Isa[Sve2]] {{ k() }}"),
        "E1403",
        "calling 'k'",
    );
    accepted(&format!(
        "{callee} fn main() -> u64 eff [Isa[Avx512]] {{ k() }}"
    ));
    accepted(&format!(
        "{callee} fn main() -> u64 eff [Isa[Avx2]] {{ k() }}"
    ));
    // Isa is never requested from the row like an ordinary effect: a
    // Baseline128 caller of a Baseline128 function needs nothing else.
    accepted(&format!(
        "{SUM} fn main() -> u32 eff [Isa[Baseline128]] {{ sum(Vec::splat[u32, 4](1), Vec::splat[u32, 4](2)) }}"
    ));
}

#[test]
fn checked_dispatch_discharges_isa() {
    let callee = "fn k() -> u64 eff [Isa[Avx2]] { Mask::count(Mask::active[u8, 32](0, 5)) }";
    accepted(&format!(
        "{callee} fn main() -> u64 {{ if Isa::supported[Avx2]() {{ k() }} else {{ 0 }} }}"
    ));
    // Avx512 implies Avx2.
    accepted(&format!(
        "{callee} fn main() -> u64 {{ if Isa::supported[Avx512]() {{ k() }} else {{ 0 }} }}"
    ));
    // The else branch does not hold the profile.
    rejected(
        &format!(
            "{callee} fn main() -> u64 {{ if Isa::supported[Avx2]() {{ 0 }} else {{ k() }} }}"
        ),
        "E1403",
        "calling 'k'",
    );
    // A narrower probe does not discharge a wider profile.
    rejected(
        &format!(
            "{callee} fn main() -> u64 {{ if Isa::supported[Baseline128]() {{ k() }} else {{ 0 }} }}"
        ),
        "E1403",
        "calling 'k'",
    );
    // Only the probe itself as the condition grants it.
    rejected(
        &format!(
            "{callee} fn main() -> u64 {{ let ok = Isa::supported[Avx2](); if ok {{ k() }} else {{ 0 }} }}"
        ),
        "E1403",
        "calling 'k'",
    );
    // Direct vector operations inside the branch.
    accepted(
        "fn main() -> u64 { if Isa::supported[Avx2]() { Mask::count(Mask::active[u8, 32](3, 9)) } else { 0 } }",
    );
}

#[test]
fn closures_hold_only_their_own_profile() {
    rejected(
        "fn main() -> u64 eff [Isa[Baseline128]] { let f = || -> u64 { Mask::count(Mask::active[u8, 16](0, 3)) }; f() }",
        "E1403",
        "Mask::count",
    );
    accepted(
        "fn main() -> u64 eff [Isa[Baseline128]] { let f = || -> u64 eff [Isa[Baseline128]] { Mask::count(Mask::active[u8, 16](0, 3)) }; f() }",
    );
    // Calling a function value whose type carries Isa needs the profile.
    rejected(
        "fn main() -> u64 { let f = || -> u64 eff [Isa[Baseline128]] { Mask::count(Mask::active[u8, 16](0, 3)) }; f() }",
        "E1403",
        "calling 'f'",
    );
}

#[test]
fn isa_is_reserved_and_cannot_be_handled() {
    rejected(
        "fn f() -> u64 eff [Isa[Neon]] { 0 }",
        "E1404",
        "one ISA profile argument",
    );
    rejected("fn f() -> u64 eff [Isa] { 0 }", "E1404", "found []");
    rejected(
        "effect Isa[P] { fn probe() -> u64; } fn main() -> u64 { 0 }",
        "E0100",
        "reserved",
    );
    rejected(
        "fn main() -> u64 { handle Isa[Avx2] in { 1 } with { return x => x, } }",
        "E0701",
        "Isa",
    );
    rejected(
        "fn main() -> bool { Isa::supported[Neon]() }",
        "E1404",
        "profile",
    );
}

#[test]
fn operation_typing_errors() {
    let isa = "eff [Isa[Baseline128]]";
    rejected(
        &format!("fn f(a: Vec[u8, 16]) -> Vec[u8, 16] {isa} {{ Vec::mul(a, a) }}"),
        "E1402",
        "not defined for u8 lanes",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4]) -> Vec[u32, 4] {isa} {{ Vec::div(a, a) }}"),
        "E1402",
        "f64 lanes only",
    );
    rejected(
        &format!("fn f(a: Vec[f64, 2]) -> Vec[f64, 2] {isa} {{ Vec::xor(a, a) }}"),
        "E1402",
        "bitwise",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4]) -> Vec[u32, 4] {isa} {{ Vec::saturating_add(a, a) }}"),
        "E1402",
        "u8 lanes only",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4]) -> u32 {isa} {{ Vec::extract(a, 4) }}"),
        "E1402",
        "out of range",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4], i: u64) -> u32 {isa} {{ Vec::extract(a, i) }}"),
        "E1402",
        "literal lane indices",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4]) -> Vec[u32, 4] {isa} {{ Vec::shuffle(a, 0, 1) }}"),
        "E0207",
        "expects 5 arguments",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4], b: Vec[u64, 2]) -> Vec[u32, 4] {isa} {{ Vec::add(a, b) }}"),
        "E0200",
        "expected Vec[u32, 4], found Vec[u64, 2]",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4]) -> u64 {isa} {{ Mask::count(a) }}"),
        "E1402",
        "expects a Mask operand",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4]) -> Vec[u32, 4] {isa} {{ Vec::rotate(a) }}"),
        "E1402",
        "unknown vector operation 'Vec::rotate'",
    );
    rejected(
        &format!("fn f() -> u64 {isa} {{ Mask::count(Mask::active(0, 1)) }}"),
        "E1401",
        "cannot infer the shape",
    );
    rejected(
        &format!("fn f(s: String) -> Vec[u32, 4] {isa} {{ Vec::load_masked[u32, 4](s, 0) }}"),
        "E1402",
        "u8 lanes",
    );
    rejected(
        &format!("fn f() -> Vec[u8, 16] {isa} {{ Vec::splat[u8, 16](300) }}"),
        "E0209",
        "outside the range",
    );
    // The expected type supplies the shape of a shape-taking intrinsic.
    accepted(&format!(
        "fn f() -> Vec[u8, 16] {isa} {{ let v: Vec[u8, 16] = Vec::splat(7); v }}"
    ));
}

// -------------------------------------------------------------- semantics

fn baseline(result: &str, body: &str) -> String {
    format!(
        "fn kernel() -> {result} eff [Isa[Baseline128]] {{ {body} }} fn main() -> Option[{result}] {{ if Isa::supported[Baseline128]() {{ Some(kernel()) }} else {{ None }} }}"
    )
}

#[test]
fn exact_lane_semantics() {
    let cases: &[(&str, &str, &str)] = &[
        // Wrapping integer lanes.
        (
            "(u8, u8)",
            "let v = Vec::add(Vec::splat[u8, 16](250), Vec::splat[u8, 16](10)); (Vec::extract(v, 0), Vec::extract(v, 15))",
            "Some((4, 4))",
        ),
        (
            "u8",
            "Vec::extract(Vec::saturating_add(Vec::splat[u8, 16](250), Vec::splat[u8, 16](10)), 3)",
            "Some(255)",
        ),
        (
            "u8",
            "Vec::extract(Vec::saturating_sub(Vec::splat[u8, 16](5), Vec::splat[u8, 16](10)), 3)",
            "Some(0)",
        ),
        // Shift amounts are taken modulo the lane width.
        (
            "u32",
            "Vec::extract(Vec::shl(Vec::splat[u32, 4](1), 33), 0)",
            "Some(2)",
        ),
        (
            "i64",
            "Vec::extract(Vec::shr(Vec::splat[i64, 2](-16), 2), 1)",
            "Some(-4)",
        ),
        (
            "u64",
            "Vec::extract(Vec::shr(Vec::splat[u64, 2](16), 2), 1)",
            "Some(4)",
        ),
        // Signed and unsigned comparisons.
        (
            "u64",
            "Mask::bitmask(Vec::lt(Vec::from[i64, 2](-1, 5), Vec::from[i64, 2](0, 4)))",
            "Some(1)",
        ),
        (
            "u64",
            "Mask::bitmask(Vec::lt(Vec::from[u64, 2](18446744073709551615, 5), Vec::from[u64, 2](0, 6)))",
            "Some(2)",
        ),
        // Float min/max order -0.0 below +0.0.
        (
            "(f64, f64)",
            "let v = Vec::min(Vec::from[f64, 2](0.0, -0.0), Vec::from[f64, 2](-0.0, 0.0)); let w = Vec::max(Vec::from[f64, 2](0.0, -0.0), Vec::from[f64, 2](-0.0, 0.0)); (Vec::extract(v, 0), Vec::extract(w, 1))",
            "Some((-0, 0))",
        ),
        // NaN compares unordered: only `ne` holds.
        (
            "(u64, u64)",
            "let nan = Vec::div(Vec::splat[f64, 2](0.0), Vec::splat[f64, 2](0.0)); (Mask::bitmask(Vec::ne(nan, nan)), Mask::bitmask(Vec::eq(nan, nan)))",
            "Some((3, 0))",
        ),
        // Reductions fold in index order.
        (
            "u32",
            "Vec::reduce_max(Vec::from[u32, 4](3, 9, 4294967295, 1))",
            "Some(4294967295)",
        ),
        (
            "i64",
            "Vec::reduce_min(Vec::from[i64, 2](3, -9))",
            "Some(-9)",
        ),
        ("u8", "Vec::reduce_add(Vec::splat[u8, 16](17))", "Some(16)"),
        // Masks.
        (
            "(u64, bool, bool)",
            "let m = Mask::active[u8, 16](13, 16); (Mask::bitmask(m), Mask::any(m), Mask::all(m))",
            "Some((7, true, false))",
        ),
        (
            "(u64, u64)",
            "(Mask::count(Mask::active[u64, 2](18446744073709551615, 0)), Mask::count(Mask::not(Mask::active[u64, 2](1, 2))))",
            "Some((0, 1))",
        ),
        // Strings: zero lanes past the end, never a fault.
        (
            "(u8, u8, u64)",
            "let v = Vec::load_masked[u8, 16](\"abc\", 1); (Vec::extract(v, 1), Vec::extract(v, 2), Mask::count(Mask::in_bounds[u8, 16](\"abc\", 1)))",
            "Some((99, 0, 2))",
        ),
        (
            "u64",
            "Mask::count(Mask::in_bounds[u8, 16](\"abc\", 18446744073709551615))",
            "Some(0)",
        ),
        // Lane replacement, selection and swizzles.
        (
            "(u32, u32)",
            "let v = Vec::replace(Vec::splat[u32, 4](1), 2, 7); let s = Vec::shuffle(v, 2, 2, 0, 1); (Vec::extract(s, 0), Vec::extract(s, 2))",
            "Some((7, 1))",
        ),
        (
            "u32",
            "let a = Vec::from[u32, 4](1, 8, 3, 9); let b = Vec::from[u32, 4](5, 2, 7, 4); Vec::reduce_add(Vec::select(Vec::gt(a, b), a, b))",
            "Some(29)",
        ),
    ];
    for (result, body, expected) in cases {
        let source = baseline(result, body);
        assert_eq!(&evaluate(&source), expected, "{body}");
        assert_eq!(&differential(&source), expected, "{body}");
    }
}

// ------------------------------------------------------- differential

/// Deterministic lane values, including each type's extremes.
fn lane_values(elem: vector::Elem, n: usize, seed: u64) -> Vec<String> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..n)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            match elem {
                vector::Elem::F64 => {
                    let special = [0.0, -0.0, 1.5e300, -2.25, 1.0, -7.0e-300];
                    if i % 3 == 0 {
                        format!("{:?}", special[(x % special.len() as u64) as usize])
                    } else {
                        format!("{:?}", (x % 20_000) as f64 / 8.0 - 1250.0)
                    }
                }
                vector::Elem::I64 => match i % 4 {
                    0 => "-9223372036854775807".into(),
                    1 => ((x as i64) >> 1).to_string(),
                    _ => ((x % 2001) as i64 - 1000).to_string(),
                },
                e => {
                    let m = e.mask();
                    if i % 5 == 0 {
                        m.to_string()
                    } else {
                        (x & m).to_string()
                    }
                }
            }
        })
        .collect()
}

/// One program per shape: every applicable operation, all lanes returned.
fn shape_program(elem: vector::Elem, n: usize, profile: &str) -> String {
    use vector::{BinOp, CmpOp, Intrinsic, ReduceOp};
    let e = elem.name();
    let vt = format!("Vec[{e}, {n}]");
    // Up to eight lanes are returned exactly; wider integer vectors return
    // four lanes plus xor and sum digests over all of them, which keeps the
    // generated programs small enough to check and compile quickly.
    let digest = n > 8 && !elem.is_float();
    let lanes_ty = format!("({})", vec![e; if digest { 6 } else { n }].join(", "));
    let lanes = |v: &str| {
        let parts = if digest {
            [0, 1, n / 2, n - 1]
                .iter()
                .map(|i| format!("Vec::extract(r, {i})"))
                .chain(["Vec::reduce_xor(r)".into(), "Vec::reduce_add(r)".into()])
                .collect::<Vec<_>>()
        } else {
            (0..n).map(|i| format!("Vec::extract(r, {i})")).collect()
        };
        format!("{{ let r = {v}; ({}) }}", parts.join(", "))
    };
    let mut items = vec![];
    let mut push = |ty: String, expr: String| items.push((ty, expr));
    let binary = [
        ("add", BinOp::Add),
        ("sub", BinOp::Sub),
        ("mul", BinOp::Mul),
        ("div", BinOp::Div),
        ("min", BinOp::Min),
        ("max", BinOp::Max),
        ("and", BinOp::And),
        ("or", BinOp::Or),
        ("xor", BinOp::Xor),
        ("saturating_add", BinOp::SaturatingAdd),
        ("saturating_sub", BinOp::SaturatingSub),
    ];
    for (name, op) in binary {
        if Intrinsic::Binary(op).lane_error(elem).is_none() {
            push(lanes_ty.clone(), lanes(&format!("Vec::{name}(a(), b())")));
        }
    }
    if !elem.is_float() {
        push(lanes_ty.clone(), lanes("Vec::not(a())"));
        for k in [0, 3, 70] {
            push(lanes_ty.clone(), lanes(&format!("Vec::shl(a(), {k})")));
            push(lanes_ty.clone(), lanes(&format!("Vec::shr(a(), {k})")));
        }
    }
    let compares = [
        ("eq", CmpOp::Eq),
        ("ne", CmpOp::Ne),
        ("lt", CmpOp::Lt),
        ("le", CmpOp::Le),
        ("gt", CmpOp::Gt),
        ("ge", CmpOp::Ge),
    ];
    for (name, _) in compares {
        push(
            "u64".into(),
            format!("Mask::bitmask(Vec::{name}(a(), b()))"),
        );
        // Each lane against itself, and the ordering of a against a+b.
        push(
            "u64".into(),
            format!("Mask::bitmask(Vec::{name}(a(), a()))"),
        );
    }
    push(
        lanes_ty.clone(),
        lanes("Vec::select(Vec::lt(a(), b()), a(), b())"),
    );
    push(
        "(u64, u64, u64, bool, bool)".into(),
        "{ let p = Vec::lt(a(), b()); let q = Vec::ge(a(), b()); (Mask::bitmask(Mask::and(p, q)), Mask::bitmask(Mask::or(p, q)), Mask::count(Mask::xor(p, Mask::not(q))), Mask::any(p), Mask::all(Mask::or(p, q))) }".into(),
    );
    for (name, op) in [
        ("add", ReduceOp::Add),
        ("xor", ReduceOp::Xor),
        ("or", ReduceOp::Or),
        ("and", ReduceOp::And),
        ("min", ReduceOp::Min),
        ("max", ReduceOp::Max),
    ] {
        if Intrinsic::Reduce(op).lane_error(elem).is_none() {
            push(e.into(), format!("Vec::reduce_{name}(a())"));
        }
    }
    let last = n - 1;
    push(
        lanes_ty.clone(),
        lanes(&format!(
            "Vec::replace(Vec::replace(a(), 0, Vec::extract(b(), {last})), {last}, Vec::extract(b(), 1))"
        )),
    );
    let reversed = (0..n)
        .rev()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    push(
        lanes_ty.clone(),
        lanes(&format!("Vec::shuffle(a(), {reversed})")),
    );
    let broadcast = vec!["1"; n].join(", ");
    push(
        lanes_ty.clone(),
        lanes(&format!("Vec::shuffle(b(), {broadcast})")),
    );
    for (offset, len) in [(0, 0), (0, 3), (5, n + 9), (n - 1, n), (9, 3)] {
        push(
            "u64".into(),
            format!("Mask::bitmask(Mask::active[{e}, {n}]({offset}, {len}))"),
        );
    }
    if elem == vector::Elem::U8 {
        let text =
            "\"Vesper vectors \\\\ \\\"quoted\\\" text with a tail that runs past one chunk.\"";
        for offset in [0, 3, 17, 60, 70, 200] {
            push(
                lanes_ty.clone(),
                lanes(&format!("Vec::load_masked[u8, {n}]({text}, {offset})")),
            );
            push(
                "u64".into(),
                format!("Mask::bitmask(Mask::in_bounds[u8, {n}]({text}, {offset}))"),
            );
        }
        push(
            lanes_ty.clone(),
            lanes(&format!("Vec::load_masked[u8, {n}](\"\", 0)")),
        );
    }
    let a = lane_values(elem, n, 1).join(", ");
    let b = lane_values(elem, n, 2).join(", ");
    let result = format!(
        "({})",
        items
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let body = items
        .iter()
        .map(|(_, e)| e.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "fn a() -> {vt} eff [Isa[{profile}]] {{ Vec::from[{e}, {n}]({a}) }}
fn b() -> {vt} eff [Isa[{profile}]] {{ Vec::from[{e}, {n}]({b}) }}
fn run() -> {result} eff [Isa[{profile}]] {{ ({body}) }}
fn main() -> Option[{result}] {{ if Isa::supported[{profile}]() {{ Some(run()) }} else {{ None }} }}"
    )
}

fn shapes(bits: u32, profile: &str) {
    for elem in vector::Elem::ALL {
        let n = (bits / elem.bits()) as usize;
        let source = shape_program(elem, n, profile);
        let result = differential(&source);
        let enabled = vector::host_enabled(vector::Profile::from_name(profile).unwrap());
        assert_eq!(result.starts_with("Some("), enabled, "{profile} {result}");
        if !enabled {
            eprintln!(
                "note: host code generator does not target {profile}; {elem:?}x{n} ran the fallback only"
            );
        }
    }
}

#[test]
fn differential_128_bit_every_operation() {
    shapes(128, "Baseline128");
}
#[test]
fn differential_256_bit_every_operation() {
    shapes(256, "Avx2");
}
#[test]
fn differential_512_bit_every_operation() {
    shapes(512, "Avx512");
}

#[test]
fn vectors_cross_function_boundaries_and_branches() {
    // Vector parameters, results, block parameters and mutable locals.
    let source = "fn pick(c: bool, a: Vec[u32, 4], b: Vec[u32, 4]) -> Vec[u32, 4] { if c { a } else { b } }
fn step(acc: Vec[u32, 4], n: u64) -> Vec[u32, 4] eff [Isa[Baseline128]] {
    if n == 0 { acc } else { step(Vec::add(acc, Vec::from[u32, 4](1, 2, 3, 4)), wrapping_sub(n, 1)) }
}
fn kernel() -> (u32, u32) eff [Isa[Baseline128]] {
    let v = step(Vec::splat[u32, 4](0), 10);
    v = pick(false, v, Vec::mul(v, v));
    (Vec::extract(v, 0), Vec::extract(v, 3))
}
fn main() -> Option[(u32, u32)] { if Isa::supported[Baseline128]() { Some(kernel()) } else { None } }";
    assert_eq!(differential(source), "Some((100, 1600))");
}

#[test]
fn dispatch_follows_the_code_generator() {
    let source = "fn main() -> (bool, bool, bool, bool, bool) { (Isa::supported[Baseline128](), Isa::supported[Avx2](), Isa::supported[Avx512](), Isa::supported[Sve2](), Isa::supported[Rvv1]()) }";
    let expected = format!(
        "({}, {}, {}, false, false)",
        cfg!(any(target_arch = "x86_64", target_arch = "aarch64")),
        vector::host_enabled(vector::Profile::Avx2),
        vector::host_enabled(vector::Profile::Avx512),
    );
    assert_eq!(differential(source), expected);
    // Object emission for a named target folds the probe from that target's
    // flags: the generic x86_64 and AArch64 targets have no AVX2.
    let c = accepted(source);
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let isa = native::object_isa(Some(target)).unwrap();
        assert!(vector::isa_enabled(&*isa, vector::Profile::Baseline128));
        assert!(!vector::isa_enabled(&*isa, vector::Profile::Avx2));
        assert!(!vector::isa_enabled(&*isa, vector::Profile::Sve2));
        native::object_for_target(&c.world, "main", Some(target)).unwrap();
    }
}

#[test]
fn lowering_uses_cranelift_vector_instructions() {
    let source = baseline(
        "u64",
        "Mask::count(Vec::eq(Vec::load_masked[u8, 16](\"needle in a haystack\", 2), Vec::splat[u8, 16](97)))",
    );
    let c = accepted(&source);
    let (_, clif) =
        native::object_for_target(&c.world, "main", Some("x86_64-unknown-linux-gnu")).unwrap();
    for instruction in ["i8x16", "icmp eq", "vhigh_bits", "splat"] {
        assert!(clif.contains(instruction), "missing {instruction}:\n{clif}");
    }
}

#[test]
fn programs_without_vectors_emit_no_vector_code() {
    for example in [
        "fibonacci",
        "transaction",
        "projections",
        "closures",
        "checked_math",
    ] {
        let path = format!("{}/examples/{example}.ves", env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read_to_string(path).unwrap();
        let c = accepted(&source);
        let (_, clif) = native::object(&c.world, "main").unwrap();
        for ty in ["i8x16", "i32x4", "i64x2", "f64x2"] {
            assert!(!clif.contains(ty), "{example} gained {ty}");
        }
        let ir = vesper::ir::lower(&c);
        assert!(
            ir.functions.values().all(|f| f
                .blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .all(|i| !matches!(i.op, vesper::ir::Op::Vector { .. }))),
            "{example}"
        );
    }
}
