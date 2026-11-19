//! Fixed-size arrays `[T; N]` (manuscript §5, P3b gaps lane 6, #1091).
//!
//! Every program runs in the evaluator (optimized and not), the JIT
//! (optimized and not), as x86-64 and AArch64 objects and, where a C driver
//! links executables, as an executable, and every backend must give the same
//! answer.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{ast::*, check, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-arrays-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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

fn rejects(source: &str, code: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code} for {source}: {:?}",
        c.diagnostics
    );
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn executable_status(name: &str, source: &str) -> Option<i32> {
    let temp = Temp::new();
    let file = temp.0.join(format!("{name}.ves"));
    fs::write(&file, source).unwrap();
    let exe = temp.0.join(name);
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{name}: vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    Command::new(&exe).output().unwrap().status.code()
}

/// Runs `main` everywhere and checks each result against `expected`.
fn differential(name: &str, source: &str, expected: u64) {
    let c = accepted(source);
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = optimize;
        let value = runtime
            .run("main", vec![])
            .unwrap_or_else(|e| panic!("{name}: evaluator (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            value,
            expected.to_string(),
            "{name}: evaluator (optimize {optimize})"
        );
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            jit,
            expected.to_string(),
            "{name}: JIT (optimize {optimize})"
        );
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        native::object_for_target(&c.world, "main", Some(target))
            .unwrap_or_else(|e| panic!("{name}: {target} object failed: {e}"));
    }
    if links_executables() {
        assert_eq!(
            executable_status(name, source),
            Some((expected & 0xff) as i32),
            "{name}: executable"
        );
    }
}

#[test]
fn array_types_and_literals_are_homogeneous_tuples() {
    let p = parser::parse(
        "fn f(in a: [u64; 3], b: [[u8; 2]; 2]) -> [i64; 2] { let s = [g(); 2]; [1, 2] } fn g() -> i64 { 1 }",
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let Item::Function(f) = &p.module.items[0] else {
        panic!("expected a function");
    };
    assert_eq!(f.params[0].ty, Type::Tuple(vec![Type::named("u64"); 3]));
    assert_eq!(
        f.params[1].ty,
        Type::Tuple(vec![Type::Tuple(vec![Type::named("u8"); 2]); 2])
    );
    assert_eq!(f.result, Type::Tuple(vec![Type::named("i64"); 2]));
    // `[g(); 2]` evaluates `g()` once, into a hidden binding.
    let Some(Expr {
        kind: ExprKind::Block { statements, .. },
        ..
    }) = &f.body
    else {
        panic!("expected a block body");
    };
    let Statement::Let { value, .. } = &statements[0] else {
        panic!("expected a let");
    };
    let ExprKind::Block {
        statements: inner,
        tail: Some(tail),
    } = &value.kind
    else {
        panic!("a repeated call is a block: {value:?}");
    };
    assert!(matches!(
        &inner[0],
        Statement::Let { pat: Pattern { kind: PatternKind::Bind(n), .. }, .. }
            if n == vesper::arrays::ARRAY_SPLAT_BINDING
    ));
    assert!(matches!(&tail.kind, ExprKind::Tuple(es) if es.len() == 2));
    // A tensor shape `[M, N]` is not an array.
    let shape = parser::parse("fn f(in t: Tensor[f32, [M, 4]]) -> u64 { 0 }");
    let Item::Function(f) = &shape.module.items[0] else {
        panic!("expected a function");
    };
    assert!(
        !matches!(&f.params[0].ty, Type::Named(_, a) if matches!(a.get(1), Some(Type::Tuple(_))))
    );
}

#[test]
fn array_lengths_must_be_integer_literals() {
    for source in [
        "fn f(in a: [u64; n]) -> u64 { 0 }",
        "fn f() -> u64 { let a = [0; n]; 0 }",
        "fn f(in a: [u64; 99999999]) -> u64 { 0 }",
    ] {
        assert!(
            !parser::parse(source).diagnostics.is_empty(),
            "expected a parse error: {source}"
        );
    }
}

#[test]
fn reads_writes_set_and_len_agree_everywhere() {
    differential(
        "basics",
        "struct Acc { counts: [u64; 4], total: u64 }
fn sum(in a: [u64; 4]) -> u64 {
    let mut s: u64 = 0;
    for i in 0..a.len() {
        s = wrapping_add(s, a[i]);
    }
    s
}
fn main() -> u64 {
    let mut a: [u64; 4] = [0; 4];
    a[1] = 5;
    a.set(2, 7);
    a[3] = 2;
    let b: [u64; 4] = [1, 2, 3, 4];
    let mut acc = Acc { counts: a, total: 0 };
    acc.counts[0] = 9;
    let x = wrapping_add(sum(acc.counts), a.len());
    acc.total = wrapping_add(x, b[3]);
    wrapping_add(acc.total, a[0])
}",
        23 + 4 + 4,
    );
}

#[test]
fn arrays_are_copies() {
    differential(
        "copies",
        "fn bump(a: [u64; 3]) -> [u64; 3] { let mut b = a; b[0] = 100; b }
fn main() -> u64 {
    let a: [u64; 3] = [1, 2, 3];
    let b = bump(a);
    let mut c = b;
    c[1] = 50;
    wrapping_add(wrapping_add(a[0], b[0]), wrapping_add(b[1], c[1]))
}",
        1 + 100 + 2 + 50,
    );
}

#[test]
fn struct_elements_nested_arrays_and_returned_arrays() {
    differential(
        "nested",
        "copy struct P { x: u64, y: i64 }
fn zeros() -> [u64; 4] { [0; 4] }
fn mk(v: u64) -> P { P { x: v, y: -1 } }
fn main() -> u64 {
    let ps: [P; 3] = [mk(4); 3];
    let mut q = ps;
    q[1] = P { x: 9, y: 2 };
    let m: [[u64; 2]; 3] = [[1, 2], [3, 4], [5, 6]];
    let i: u64 = 2;
    let j: u64 = 1;
    let r = zeros();
    let t = wrapping_add(q[1].x, ps[1].x);
    let t = wrapping_add(t, m[i][j]);
    let t = wrapping_add(t, r[3]);
    wrapping_add(t, q.len())
}",
        9 + 4 + 6 + 3,
    );
}

#[test]
fn out_of_bounds_reads_and_writes_raise() {
    differential(
        "bounds",
        "fn read(in a: [u64; 4], i: u64) -> u64 eff [Fail[RuntimeError]] { a[i] }
fn write(i: u64) -> u64 eff [Fail[RuntimeError]] {
    let mut a: [u64; 4] = [1; 4];
    a[i] = 9;
    a.set(i, 8);
    a[0]
}
fn guard(f: u64) -> u64 { f }
fn main() -> u64 {
    let a: [u64; 4] = [1, 2, 3, 4];
    let r1 = handle Fail[RuntimeError] in { read(a, 3) } with { return x => x, raise(e, k) => 100, };
    let r2 = handle Fail[RuntimeError] in { read(a, 4) } with { return x => x, raise(e, k) => 100, };
    let r3 = handle Fail[RuntimeError] in { write(0) } with { return x => x, raise(e, k) => 50, };
    let r4 = handle Fail[RuntimeError] in { write(7) } with { return x => x, raise(e, k) => 50, };
    wrapping_add(wrapping_add(r1, r2), wrapping_add(r3, r4))
}",
        4 + 100 + 8 + 50,
    );
}

#[test]
fn compound_writes_check_overflow() {
    differential(
        "compound",
        "fn bump(v: u8) -> u64 eff [Fail[ArithOverflow]] {
    let mut a: [u8; 2] = [250, 1];
    a[0] += v;
    a[1] *= 3;
    wrapping_add(widen_u64(a[0]), widen_u64(a[1]))
}
fn main() -> u64 {
    let r1 = handle Fail[ArithOverflow] in { bump(5) } with { return x => x, raise(e, k) => 7, };
    let r2 = handle Fail[ArithOverflow] in { bump(6) } with { return x => x, raise(e, k) => 7, };
    wrapping_add(r1, r2)
}",
        255 + 3 + 7,
    );
}

#[test]
fn constant_and_guarded_indices_are_total() {
    // No `Fail[RuntimeError]` in these rows: every index is proved in
    // bounds (`i - 1` itself is checked arithmetic, `Fail[ArithOverflow]`).
    differential(
        "total",
        "fn first(in a: [u64; 4]) -> u64 { a[0] }
fn guarded(in a: [u64; 4], i: u64) -> u64 { if i < 4 { a[i] } else { 0 } }
fn id(x: u64) -> u64 { x }
fn masked(in a: [u64; 4], i: u64) -> u64 { a[id(i) & 3] }
fn looped(in a: [u64; 4]) -> u64 {
    let mut s: u64 = 0;
    for i in 0..4 {
        s = wrapping_add(s, a[i]);
    }
    for i in 0..a.len() {
        s = wrapping_add(s, a[i]);
    }
    s
}
fn writes() -> u64 eff [Fail[ArithOverflow]] {
    let mut b: [u64; 8] = [0; 8];
    for i in 1..=7 {
        b[i] = wrapping_add(b[i - 1], 2);
    }
    b[7]
}
fn main() -> u64 {
    let a: [u64; 4] = [1, 2, 3, 4];
    let w = handle Fail[ArithOverflow] in { writes() } with { return x => x, raise(e, k) => 0, };
    wrapping_add(wrapping_add(first(a), guarded(a, 2)), wrapping_add(looped(a), wrapping_add(w, masked(a, 5))))
}",
        1 + 3 + 20 + 14 + 2,
    );
    // An index the checker cannot bound needs `Fail[RuntimeError]`.
    rejects(
        "fn f(in a: [u64; 4], i: u64) -> u64 { a[i] } fn main() -> u64 { 0 }",
        "E0401",
    );
    rejects(
        "fn f(i: u64) -> u64 { let mut a: [u64; 4] = [0; 4]; a[i] = 1; a[0] } fn main() -> u64 { 0 }",
        "E0401",
    );
    rejects(
        "fn f(in a: [u64; 4]) -> u64 { let mut s: u64 = 0; for i in 0..5 { s = wrapping_add(s, a[i]); } s } fn main() -> u64 { 0 }",
        "E0401",
    );
}

#[test]
fn loop_index_arithmetic_is_total() {
    // `i + 1`, `i - 1` and `15 - i` of a constant loop range neither
    // overflow nor leave the array: no `Fail` at all in these rows.
    differential(
        "loop_arith",
        "fn shifted(in a: [u64; 16]) -> u64 {
    let mut b = a;
    for i in 0..15 {
        b[i] = b[i + 1];
    }
    for i in 1..16 {
        b[i] = wrapping_add(b[i], b[i - 1]);
    }
    b[15]
}
fn reversed(in a: [u64; 16]) -> u64 {
    let mut s: u64 = 0;
    for i in 0..16 {
        s = wrapping_add(wrapping_mul(s, 3), a[15 - i]);
    }
    s
}
fn main() -> u64 {
    let mut a: [u64; 16] = [0; 16];
    for i in 0..16 {
        a[i] = wrapping_add(wrapping_mul(i, 2), 1);
    }
    wrapping_add(shifted(a), reversed(a))
}",
        {
            let a: Vec<u64> = (0..16).map(|i| i * 2 + 1).collect();
            let mut b = a.clone();
            for i in 0..15 {
                b[i] = b[i + 1];
            }
            for i in 1..16 {
                b[i] = b[i].wrapping_add(b[i - 1]);
            }
            let r = (0..16).fold(0u64, |s, i| s.wrapping_mul(3).wrapping_add(a[15 - i]));
            b[15].wrapping_add(r)
        },
    );
    // One past the range overflows the array or the kind.
    for body in [
        "for i in 0..16 { s = wrapping_add(s, a[i + 1]); }",
        "for i in 0..16 { s = wrapping_add(s, a[16 - i]); }",
        "for i in 0..=17 { s = wrapping_add(s, 16 - i); }",
        "for i in 0..16 { s = wrapping_add(s, i - 1); }",
    ] {
        rejects(
            &format!(
                "fn f(in a: [u64; 16]) -> u64 {{ let mut s: u64 = 0; {body} s }} fn main() -> u64 {{ 0 }}"
            ),
            "E0401",
        );
    }
}

#[test]
fn writes_need_mutable_array_places() {
    rejects(
        "fn f() -> u64 { let a: [u64; 2] = [1, 2]; a[0] = 3; a[0] } fn main() -> u64 { 0 }",
        "E0313",
    );
    rejects(
        "fn g() -> [u64; 2] { [1, 2] } fn f() -> u64 { g()[0] = 3; 0 } fn main() -> u64 { 0 }",
        "E0300",
    );
    rejects(
        "fn f() -> u64 { let mut a: [u64; 2] = [1, 2]; a.set(0, 1, 2); 0 } fn main() -> u64 { 0 }",
        "E0207",
    );
    rejects(
        "fn f() -> u64 { let mut a: [u64; 2] = [1, 2]; a[0] = true; 0 } fn main() -> u64 { 0 }",
        "E0200",
    );
    rejects(
        "fn f() -> u64 { let mut a: [bool; 2] = [true, false]; a[0] += true; 0 } fn main() -> u64 { 0 }",
        "E0200",
    );
}

#[test]
fn arrays_inside_closures_and_handlers() {
    differential(
        "closures",
        "fn body(n: u64) -> u64 eff [Fail[RuntimeError]] {
    let mut a: [u64; 4] = [1; 4];
    a[n] = 6;
    let f = |x: u64| -> u64 { wrapping_add(x, 7) };
    wrapping_add(f(a[n]), a[0])
}
fn main() -> u64 {
    let x = handle Fail[RuntimeError] in { body(1) } with { return x => x, raise(e, k) => 100, };
    let y = handle Fail[RuntimeError] in { body(9) } with { return x => x, raise(e, k) => 100, };
    let z = handle Fail[RuntimeError] in { let mut b: [u64; 2] = [0, 0]; b[3] = 1; b[0] } with { return x => x, raise(e, k) => 50, };
    wrapping_add(wrapping_add(x, y), z)
}",
        14 + 100 + 50,
    );
}

#[test]
fn a_large_scratch_table() {
    // The Bench `add_rank` shape: a stack table indexed in a counted loop.
    differential(
        "scratch",
        "fn ranks(n: u64) -> u64 eff [Fail[RuntimeError]] {
    let mut dist: [u64; 528] = [0; 528];
    dist[0] = 1;
    for k in 1..=n {
        let mut s: u64 = 528;
        loop {
            if s <= k { break; }
            s = wrapping_sub(s, 1);
            dist[s] = wrapping_add(dist[s], dist[wrapping_sub(s, k)]);
        }
    }
    let mut total: u64 = 0;
    for i in 0..528 {
        total = wrapping_add(total, dist[i]);
    }
    total
}
fn main() -> u64 { handle Fail[RuntimeError] in { ranks(32) } with { return x => x, raise(e, k) => 0, } }",
        // The subsets of {1, ..., 32} with a sum below 528: all but the
        // whole set, whose sum is 528.
        (1 << 32) - 1,
    );
}

#[test]
fn array_source_formats_on_one_line() {
    let source = "struct S {\n    a: [u64; 4],\n}\n\nfn f(in a: [u64; 4]) -> [u8; 2] {\n    let b = [0; 2];\n    b\n}\n";
    let formatted = vesper_fmt::format_source(source, Default::default()).unwrap();
    assert_eq!(formatted.output, source);
}

#[test]
fn review_cases_repeated_operands_receivers_and_limits() {
    // A repeated operand keeps its contextual element type.
    accepted("fn f() -> [f32; 2] { [-1.0; 2] } fn main() -> u64 { 0 }");
    // `len` of a temporary array.
    differential(
        "temporary_len",
        "fn make() -> [u64; 3] { [1, 2, 3] }
fn main() -> u64 { wrapping_add(make().len(), make()[2]) }",
        3 + 3,
    );
    // Nested array types and literals have one element budget in all.
    for source in [
        "fn f(a: [[u64; 65536]; 65536]) {}",
        "fn f() -> u64 { let a = [[0; 65536]; 65536]; 0 }",
    ] {
        assert!(
            !vesper::parser::parse(source).diagnostics.is_empty(),
            "{source}"
        );
    }
    // Native code does not track call targets stored into an array.
    rejects(
        "fn one() -> u64 { 1 } fn two() -> u64 { 2 }
fn main() -> u64 { let mut a = [one, one]; a[0] = two; let f = a[0]; f() }",
        vesper::arrays::ARRAY_OPERATION,
    );
}
