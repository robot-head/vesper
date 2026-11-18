//! `Write` destinations built in place (port speed lane 2, #1022).
//!
//! On the native optimize path `place = T { .. }` builds `T` in `place`
//! itself when nothing reads `place`, and nothing can fault visibly, between
//! the first store into it and the write; otherwise the value is built in a
//! temporary and copied once. Every program runs in the evaluator, the JIT
//! with the optimizer on and off, x86-64 and AArch64 objects and, where a C
//! compiler links, an executable; all must agree. Recursive helpers stay
//! calls, so the in-place paths run in the JIT as well as in inlined forms.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Runs `main` everywhere and returns the agreed result.
fn differential(name: &str, source: &str) -> u64 {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(jit, evaluated, "{name}: JIT (optimize {optimize})");
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (obj, _) = native::object_with_options(&c.world, "main", Some(target), true)
            .unwrap_or_else(|e| panic!("{name}: {target}: {e}"));
        assert_eq!(&obj[..4], b"\x7fELF");
    }
    let expected: u64 = evaluated.parse().unwrap();
    if links_executables() {
        let dir = std::env::temp_dir().join(format!(
            "vesper-write-in-place-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{name}.ves"));
        fs::write(&file, source).unwrap();
        let exe: PathBuf = dir.join(name);
        let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{name}: build failed: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        let run = Command::new(&exe).output().unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(run.status.code(), Some((expected & 0xff) as i32), "{name}");
    }
    expected
}

/// The stack slots of the one emitted function that takes `params` `i64`
/// parameters; each test program gives its function of interest a unique
/// count. The x86-64 object of the optimize path is the one inspected.
fn stack_slots(source: &str, params: usize) -> usize {
    let c = accepted(source);
    let (_, clif) =
        native::object_with_options(&c.world, "main", Some("x86_64-unknown-linux-gnu"), true)
            .unwrap_or_else(|e| panic!("object: {e}"));
    let head = format!("u0:0({})", vec!["i64"; params].join(", "));
    let bodies: Vec<&str> = clif
        .split("function ")
        .filter(|f| f.starts_with(&head))
        .collect();
    assert_eq!(bodies.len(), 1, "one function with {params} parameters");
    bodies[0].matches("explicit_slot").count()
}

const WRITES: &str = r#"
copy struct Rec {
    a: u64,
    b: u64,
    c: u64
}

copy struct Inner {
    x: u64,
    y: u64
}

copy struct Outer {
    inner: Inner,
    k: u64
}

copy enum Shape {
    Dot(u64),
    Box(u64, u64, u64)
}

copy struct Q {
    n: u64,
    f: Rec,
    o: Option[u64],
    out: Outer,
    s: Shape,
    p: Outer
}

fn set_all(inout q: Q, a: u64, b: u64, c: u64, depth: u64) {
    if depth > 0 {
        set_all(inout q, a, b, c, wrapping_sub(depth, 1));
        return;
    }
    q.f = Rec { a: a, b: b, c: c };
    q.o = Option::Some(wrapping_add(a, b));
    q.out = Outer { inner: Inner { x: c, y: a }, k: b };
    q.s = Shape::Box(a, b, c);
}

fn swap_scalars(inout q: Q) {
    q.f = Rec { a: q.f.c, b: q.f.a, c: q.f.b };
    q.out = Outer { inner: Inner { x: q.out.inner.y, y: q.out.inner.x }, k: q.out.k };
}

fn swap_aggregates(inout q: Q) {
    q.p = Outer { inner: q.out.inner, k: q.p.k };
    q.out = Outer { inner: Inner { x: q.out.k, y: q.out.inner.x }, k: q.out.inner.y };
}

fn score(q: Q) -> u64 {
    let o: u64 = match q.o {
        Option::Some(v) => v,
        Option::None => 0
    };
    let s: u64 = match q.s {
        Shape::Dot(v) => v,
        Shape::Box(a, b, c) => wrapping_add(wrapping_mul(a, 7), wrapping_add(wrapping_mul(b, 11), c))
    };
    let f = wrapping_add(wrapping_mul(q.f.a, 3), wrapping_add(wrapping_mul(q.f.b, 5), wrapping_mul(q.f.c, 13)));
    let out = wrapping_add(wrapping_mul(q.out.inner.x, 17), wrapping_add(wrapping_mul(q.out.inner.y, 19), wrapping_mul(q.out.k, 23)));
    let p = wrapping_add(wrapping_mul(q.p.inner.x, 29), wrapping_add(wrapping_mul(q.p.inner.y, 31), wrapping_mul(q.p.k, 37)));
    wrapping_add(wrapping_add(o, s), wrapping_add(f, wrapping_add(out, p)))
}

fn main() -> u64 {
    let mut q = Q {
        n: 1,
        f: Rec { a: 0, b: 0, c: 0 },
        o: Option::None,
        out: Outer { inner: Inner { x: 0, y: 0 }, k: 0 },
        s: Shape::Dot(4),
        p: Outer { inner: Inner { x: 1, y: 2 }, k: 3 }
    };
    set_all(inout q, 2, 3, 4, 2);
    let first = score(q);
    swap_scalars(inout q);
    let second = score(q);
    swap_aggregates(inout q);
    let third = score(q);
    let mut acc = Inner { x: 1, y: 2 };
    let n: u64 = 5;
    for i in 0..n {
        acc = Inner { x: acc.y, y: wrapping_add(acc.x, i) };
    }
    wrapping_add(wrapping_add(first, wrapping_mul(second, 3)), wrapping_add(wrapping_mul(third, 5), wrapping_add(acc.x, acc.y)))
}
"#;

/// Field writes through `inout q` (structs, an `Option`, a nested struct
/// and a variant), writes whose operands read the destination as scalars
/// (built in place: every scalar is read before the first store) or as
/// aggregates (kept in a temporary), and a whole-local write in a loop.
#[test]
fn writes_built_in_place_agree() {
    assert_eq!(differential("writes", WRITES), 4968);
}

const SLOTS: &str = r#"
copy struct Rec {
    a: u64,
    b: u64,
    c: u64
}

copy struct Inner {
    x: u64,
    y: u64
}

copy struct Outer {
    inner: Inner,
    k: u64
}

copy struct Q {
    f: Rec,
    o: Option[u64],
    out: Outer,
    p: Outer
}

fn placed(inout q: Q, a: u64, b: u64, c: u64, d: u64, e: u64, depth: u64) {
    if depth > 0 {
        placed(inout q, a, b, c, d, e, wrapping_sub(depth, 1));
        return;
    }
    q.f = Rec { a: a, b: b, c: c };
    q.o = Option::Some(d);
    q.out = Outer { inner: Inner { x: e, y: a }, k: b };
}

fn kept(inout q: Q, a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, depth: u64) {
    if depth > 0 {
        kept(inout q, a, b, c, d, e, f, wrapping_sub(depth, 1));
        return;
    }
    q.p = Outer { inner: q.out.inner, k: wrapping_add(q.p.k, a) };
}

fn main() -> u64 {
    let mut q = Q { f: Rec { a: 0, b: 0, c: 0 }, o: Option::None, out: Outer { inner: Inner { x: 0, y: 0 }, k: 0 }, p: Outer { inner: Inner { x: 1, y: 2 }, k: 3 } };
    placed(inout q, 1, 2, 3, 4, 5, 1);
    kept(inout q, 1, 2, 3, 4, 5, 6, 1);
    wrapping_add(q.p.inner.x, q.f.c)
}
"#;

/// `placed` builds all three writes in `q` with no stack storage. `kept`
/// copies the aggregate `q.out.inner` into `q.p`: an operand read from the
/// destination's root, so the rule keeps the temporary.
#[test]
fn the_rule_places_fresh_operands_and_keeps_aggregate_reads() {
    assert_eq!(differential("slots", SLOTS), 8);
    assert_eq!(stack_slots(SLOTS, 7), 0, "placed: every write in place");
    assert!(
        stack_slots(SLOTS, 8) > 0,
        "kept: an aggregate operand reads q"
    );
}

const FAULTS: &str = r#"
copy struct Inner {
    a: u64,
    b: u64
}

copy struct Outer {
    inner: Inner,
    n: u64
}

copy struct Q {
    p: Outer,
    m: u64
}

fn may(v: i64, depth: i64) -> u64 eff [Fail[i64]] {
    if depth > 0 {
        return may(v, wrapping_sub(depth, 1));
    }
    if v < 0 {
        Fail::raise(v)
    }
    7
}

fn set(inout q: Q, v: i64) eff [Fail[i64]] {
    q.p = Outer { inner: Inner { a: 9, b: 8 }, n: may(v, 2) };
}

fn set_checked(inout q: Q, x: u64, y: u64) eff [Fail[ArithOverflow]] {
    q.p = Outer { inner: Inner { a: x, b: y }, n: x + y };
}

fn main() -> u64 {
    let mut q = Q { p: Outer { inner: Inner { a: 1, b: 2 }, n: 3 }, m: 0 };
    let first: u64 = handle Fail[i64] in { set(inout q, -5); 0 } with { return v => v, raise(e, k) => 77, };
    let after_fail = wrapping_add(wrapping_mul(q.p.inner.a, 100), wrapping_add(wrapping_mul(q.p.inner.b, 10), q.p.n));
    let second: u64 = handle Fail[i64] in { set(inout q, 5); 0 } with { return v => v, raise(e, k) => 77, };
    let after_ok = wrapping_add(wrapping_mul(q.p.inner.a, 100), wrapping_add(wrapping_mul(q.p.inner.b, 10), q.p.n));
    let big: u64 = 18446744073709551615;
    let third: u64 = handle Fail[ArithOverflow] in { set_checked(inout q, big, 2); 0 } with { return v => v, raise(e, k) => 55, };
    let after_overflow = wrapping_add(wrapping_mul(q.p.inner.a, 100), wrapping_add(wrapping_mul(q.p.inner.b, 10), q.p.n));
    wrapping_add(wrapping_add(wrapping_add(first, second), third), wrapping_add(wrapping_add(after_fail, after_ok), after_overflow))
}
"#;

/// A write whose operand fails (a call that raises, checked arithmetic that
/// overflows) after part of the destination could be built leaves the old
/// value: the handler reads `q.p` unchanged.
#[test]
fn a_failing_operand_leaves_the_destination_whole() {
    assert_eq!(differential("faults", FAULTS), 2229);
}

/// The same write into a local that dies with the function is built in
/// place even around a call: a fault leaves it unobservable.
#[test]
fn locals_build_around_calls() {
    let source = r#"
copy struct Inner {
    a: u64,
    b: u64
}

copy struct Outer {
    inner: Inner,
    n: u64
}

fn twice(x: u64, depth: u64) -> u64 {
    if depth > 0 {
        return twice(x, wrapping_sub(depth, 1));
    }
    wrapping_mul(x, 2)
}

fn main() -> u64 {
    let mut o = Outer { inner: Inner { a: 1, b: 2 }, n: 3 };
    let n: u64 = 6;
    for i in 0..n {
        o = Outer { inner: Inner { a: o.n, b: o.inner.a }, n: twice(wrapping_add(o.inner.b, i), 1) };
    }
    wrapping_add(wrapping_mul(o.inner.a, 1000), wrapping_add(wrapping_mul(o.inner.b, 10), o.n))
}
"#;
    let value = differential("locals", source);
    assert!(value > 0);
}
