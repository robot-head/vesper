//! Values built in the home of a block parameter (port speed lane 2,
//! #1022).
//!
//! A value that reaches its consumer through a block parameter (an `if` or
//! `match` that picks between constructors, a callee inlined at IR level
//! whose `return` jumps to a join block, a `let` read once) is built in the
//! consumer's home: the function's result, a field of a constructor, or a
//! write destination. Every program runs in the evaluator, the JIT with the
//! optimizer on and off, x86-64 and AArch64 objects and, where a C compiler
//! links, an executable; all must agree.
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
            "vesper-block-param-homes-test-{}-{}",
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

const JOINS: &str = r#"
copy struct Rec {
    a: u64,
    b: u64,
    c: u64
}

copy enum Shape {
    Dot(u64),
    Box(u64, u64, u64)
}

copy struct Holder {
    s: Shape,
    n: u64
}

copy struct Q {
    o: Option[u64],
    r: Rec,
    h: Holder
}

fn pick(c: bool, x: u64) -> Shape {
    let s = if c { Shape::Dot(x) } else { Shape::Box(x, wrapping_add(x, 1), wrapping_add(x, 2)) };
    s
}

fn pick3(k: u64, x: u64) -> Shape {
    if k == 0 {
        Shape::Dot(x)
    } else if k == 1 {
        Shape::Box(x, x, 1)
    } else {
        Shape::Box(k, x, 2)
    }
}

fn make(a: u64, b: u64) -> Rec {
    if a > b {
        return Rec { a: a, b: b, c: 1 };
    }
    Rec { a: b, b: a, c: 2 }
}

fn holder(k: u64, x: u64) -> Holder {
    Holder {
        s: match k {
            0 => Shape::Dot(x),
            _ => Shape::Box(k, x, k)
        },
        n: wrapping_add(x, 100)
    }
}

fn update(inout q: Q, c: bool, x: u64, depth: u64) {
    if depth > 0 {
        update(inout q, c, x, wrapping_sub(depth, 1));
        return;
    }
    q.o = if c { Option::Some(x) } else { Option::None };
    q.r = make(x, 5);
    q.h = Holder { s: if c { Shape::Dot(q.h.n) } else { Shape::Box(q.h.n, x, x) }, n: x };
}

fn rotate(inout q: Q) {
    q.r = if q.r.a > q.r.b { Rec { a: q.r.b, b: q.r.c, c: q.r.a } } else { make(q.r.c, q.r.a) };
}

fn value(s: Shape) -> u64 {
    match s {
        Shape::Dot(v) => v,
        Shape::Box(a, b, c) => wrapping_add(wrapping_mul(a, 7), wrapping_add(wrapping_mul(b, 11), c))
    }
}

fn qscore(q: Q) -> u64 {
    let o: u64 = match q.o {
        Option::Some(v) => wrapping_add(v, 1000),
        Option::None => 1
    };
    wrapping_add(o, wrapping_add(wrapping_add(wrapping_mul(q.r.a, 3), wrapping_add(wrapping_mul(q.r.b, 5), q.r.c)), wrapping_add(value(q.h.s), wrapping_mul(q.h.n, 13))))
}

fn main() -> u64 {
    let mut total: u64 = 0;
    let n: u64 = 4;
    for i in 0..n {
        total = wrapping_add(total, value(pick(i == 2, i)));
        total = wrapping_add(wrapping_mul(total, 3), value(pick3(i, wrapping_add(i, 7))));
        let h = holder(i, wrapping_add(i, 3));
        total = wrapping_add(total, wrapping_add(value(h.s), h.n));
        let r = make(i, 2);
        total = wrapping_add(total, wrapping_add(r.a, wrapping_mul(r.c, 9)));
    }
    let mut q = Q { o: Option::None, r: Rec { a: 1, b: 2, c: 3 }, h: Holder { s: Shape::Dot(0), n: 6 } };
    update(inout q, true, 9, 1);
    total = wrapping_add(wrapping_mul(total, 5), qscore(q));
    update(inout q, false, 2, 0);
    total = wrapping_add(wrapping_mul(total, 5), qscore(q));
    rotate(inout q);
    total = wrapping_add(wrapping_mul(total, 5), qscore(q));
    rotate(inout q);
    total = wrapping_add(wrapping_mul(total, 5), qscore(q));
    total
}
"#;

/// An `if` bound to a local and returned, a three-way `if`, a `match` in a
/// field of a struct literal, a callee with an early `return` inlined into
/// a write, a write of an `if` between constructors that read the
/// destination, and loops around all of them.
#[test]
fn values_through_block_parameters_agree() {
    assert_eq!(differential("joins", JOINS), 6339340);
}

/// `pick` builds both arms in its result buffer, with no stack storage.
#[test]
fn both_arms_are_built_in_the_result() {
    let source = r#"
copy enum Shape {
    Dot(u64),
    Box(u64, u64, u64)
}

fn pick(c: u64, x: u64, y: u64, z: u64, depth: u64) -> Shape {
    if depth > 0 {
        return pick(c, x, y, z, wrapping_sub(depth, 1));
    }
    let s = if c == 0 { Shape::Dot(x) } else { Shape::Box(x, y, z) };
    s
}

fn main() -> u64 {
    let a = match pick(0, 3, 4, 5, 2) {
        Shape::Dot(v) => v,
        Shape::Box(a, b, c) => wrapping_add(wrapping_add(a, b), c)
    };
    let b = match pick(1, 3, 4, 5, 2) {
        Shape::Dot(v) => v,
        Shape::Box(a, b, c) => wrapping_add(wrapping_add(a, b), c)
    };
    wrapping_add(wrapping_mul(a, 100), b)
}
"#;
    assert_eq!(differential("pick", source), 312);
    // `pick`'s five arguments and its result buffer.
    assert_eq!(stack_slots(source, 6), 0);
}

/// Values that reach a join from inside a loop, and a join whose value
/// feeds the next iteration, stay correct (loop headers keep copies).
#[test]
fn joins_inside_loops_agree() {
    let source = r#"
copy struct P {
    a: u64,
    b: u64
}

fn step(p: P, i: u64) -> P {
    if wrapping_add(p.a, i) > 10 {
        P { a: p.b, b: i }
    } else {
        P { a: wrapping_add(p.a, 1), b: wrapping_add(p.b, p.a) }
    }
}

fn main() -> u64 {
    let mut p = P { a: 1, b: 2 };
    let mut total: u64 = 0;
    let n: u64 = 12;
    for i in 0..n {
        p = step(p, i);
        let q = if p.a > p.b { P { a: p.b, b: p.a } } else { p };
        total = wrapping_add(wrapping_mul(total, 3), wrapping_add(q.a, q.b));
    }
    total
}
"#;
    let value = differential("loops", source);
    assert!(value > 0);
}

/// An `else if` ladder behind `||` conditions (the Ignis
/// `graph_add_texture` shape): every arm's variant reaches the result
/// through a chain of five nested joins, and each must be built in it. A
/// plan that homed the parameters but not the deepest arms (its search
/// depth ran out on one path and not on the other) returned garbage here.
#[test]
fn long_else_if_ladders_agree() {
    let source = r#"
copy enum Outcome {
    Added(u32),
    ZeroDimension,
    CountOverflow,
    TooManySubresources,
    FinalUndefined,
    TooManyResources
}

copy enum Mode {
    Undefined,
    General
}

fn add(mips: u32, slices: u32, planes: u32, has_final: bool, mode: Mode, budget: u64, depth: u64) -> Outcome {
    if depth > 0 {
        return add(mips, slices, planes, has_final, mode, budget, wrapping_sub(depth, 1));
    }
    if mips == 0 || slices == 0 || planes == 0 {
        Outcome::ZeroDimension
    } else {
        let ms = wrapping_mul(widen_u64(mips), widen_u64(slices));
        let count = wrapping_mul(ms, widen_u64(planes));
        if ms > 0xffffffff || count > 0xffffffff {
            Outcome::CountOverflow
        } else if count > 1048576 {
            Outcome::TooManySubresources
        } else if has_final && mode == Mode::Undefined {
            Outcome::FinalUndefined
        } else if budget == 0 {
            Outcome::TooManyResources
        } else {
            Outcome::Added(wrapping_u32(count))
        }
    }
}

fn score(o: Outcome) -> u64 {
    match o {
        Outcome::Added(n) => wrapping_add(widen_u64(n), 100),
        Outcome::ZeroDimension => 1,
        Outcome::CountOverflow => 2,
        Outcome::TooManySubresources => 3,
        Outcome::FinalUndefined => 4,
        Outcome::TooManyResources => 5
    }
}

fn main() -> u64 {
    let mut total: u64 = 0;
    total = wrapping_add(wrapping_mul(total, 7), score(add(0, 1, 1, false, Mode::General, 1, 1)));
    total = wrapping_add(wrapping_mul(total, 7), score(add(4096, 4096, 4096, false, Mode::General, 1, 1)));
    total = wrapping_add(wrapping_mul(total, 7), score(add(2048, 1024, 1, false, Mode::General, 1, 1)));
    total = wrapping_add(wrapping_mul(total, 7), score(add(4, 2, 1, true, Mode::Undefined, 1, 1)));
    total = wrapping_add(wrapping_mul(total, 7), score(add(4, 2, 1, true, Mode::General, 0, 1)));
    total = wrapping_add(wrapping_mul(total, 7), score(add(4, 2, 3, true, Mode::General, 1, 1)));
    total
}
"#;
    assert_eq!(differential("ladder", source), 22993);
}

/// A fallible call in one arm of an `if` that feeds a field of a write
/// through `inout q`: the call may not be built in `q`, so the join and
/// the other arm lose that home with it, and the success path still copies
/// the call's result into the field (review of #1032).
#[test]
fn a_fallible_arm_takes_its_join_out_of_the_destination() {
    let source = r#"
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

fn make(v: i64, depth: i64) -> Inner eff [Fail[i64]] {
    if depth > 0 {
        return make(v, wrapping_sub(depth, 1));
    }
    if v < 0 {
        Fail::raise(v)
    }
    Inner { a: 40, b: 50 }
}

fn set(inout q: Q, c: bool, v: i64) eff [Fail[i64]] {
    q.p = Outer { inner: if c { make(v, 2) } else { Inner { a: 6, b: 7 } }, n: 9 };
}

fn digest(q: Q) -> u64 {
    wrapping_add(wrapping_mul(q.p.inner.a, 100), wrapping_add(wrapping_mul(q.p.inner.b, 10), q.p.n))
}

fn main() -> u64 {
    let mut q = Q { p: Outer { inner: Inner { a: 1, b: 2 }, n: 3 }, m: 0 };
    let a: u64 = handle Fail[i64] in { set(inout q, true, 5); 0 } with { return v => v, raise(e, k) => 77, };
    let first = digest(q);
    let b: u64 = handle Fail[i64] in { set(inout q, false, 5); 0 } with { return v => v, raise(e, k) => 77, };
    let second = digest(q);
    let c: u64 = handle Fail[i64] in { set(inout q, true, -1); 0 } with { return v => v, raise(e, k) => 77, };
    let third = digest(q);
    wrapping_add(wrapping_add(wrapping_add(a, b), c), wrapping_add(wrapping_mul(first, 1000000), wrapping_add(wrapping_mul(second, 1000), third)))
}
"#;
    assert_eq!(differential("fallible_arm", source), 4509679756);
}

/// A closure stored in a nested struct whose parent has no home: the
/// parent's stack slot is reached through more than one pointer, and each
/// carries the closure targets recorded on the earlier ones, so calling the
/// field resolves to that closure alone and not to an effectful closure of
/// an exited handler (review of #1032).
#[test]
fn closure_fields_of_slot_parents_keep_their_targets() {
    let source = r#"
copy struct Inner {
    f: fn@copy(i64) -> i64,
    k: i64
}

copy struct Outer {
    inner: Inner,
    n: i64
}

linear effect Ask {
    fn ask() -> i64;
}

fn other() -> i64 {
    handle Ask in {
        let g = |v: i64| eff [Ask] wrapping_add(v, Ask::ask());
        g(1)
    } with { return x => x, ask(k) => k(40), }
}

fn run(in o: Outer, x: i64) -> i64 {
    let i = o.inner;
    let h = i.f;
    h(x)
}

fn main() -> i64 {
    let base = other();
    let y: i64 = 5;
    let o = Outer { inner: Inner { f: |v: i64| wrapping_mul(v, y), k: 2 }, n: 3 };
    let r = run(in o, 7);
    wrapping_add(wrapping_add(r, base), wrapping_add(o.inner.k, o.n))
}
"#;
    assert_eq!(differential("closure_field", source), 81);
}
