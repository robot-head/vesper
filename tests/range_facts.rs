//! Port speed lane 7 (#1027): range facts from guards. A dominating
//! comparison (`s < 64`, `s <= 63`, `64 > s`, `!(s >= 64)`, conjunctions, an
//! early return) bounds an integer local until the next write to it, and
//! `x & m` / `x % m` bound their result. The bounds make `<<`, `>>`, `/`, `%`
//! and `narrow_k` total, so a guarded helper needs no handler and inlines.
//!
//! Every accepted program runs in the evaluator (optimized and not), the JIT
//! (optimized and not), object emission for x86-64 and AArch64 and, where a
//! C driver links executables, an AOT executable, and all must agree.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, ir_inline, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-range-facts-{}-{}",
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

/// `source` fails to check because an operation still needs a `Fail`
/// effect the function does not declare.
fn needs_effect(source: &str) {
    let c = checked(source);
    assert!(
        !c.diagnostics.is_empty(),
        "expected an undeclared Fail effect for {source}"
    );
    assert!(
        c.diagnostics.iter().all(|d| d.message.contains("Fail")),
        "{source}: {:?}",
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
    assert!(
        c.world.overflow_ops.is_empty() && c.world.partial_ops.is_empty(),
        "{name}: every guarded operation is total: {:?} {:?}",
        c.world.overflow_ops,
        c.world.partial_ops
    );
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
        for optimize in [true, false] {
            let (obj, _) = native::object_with_options(&c.world, "main", Some(target), optimize)
                .unwrap_or_else(|e| panic!("{name}: {target} optimize={optimize}: {e}"));
            assert_eq!(&obj[..4], b"\x7fELF");
        }
    }
    if links_executables() {
        assert_eq!(
            executable_status(name, source),
            Some((expected & 0xff) as i32),
            "{name}: executable"
        );
    }
}

/// A fold over amounts 0..80, so both sides of every guard run.
const DRIVER: &str = "fn main() -> u64 { let mut acc: u64 = 14695981039346656037; \
    for i in 0..80 { let s: u64 = wrapping_u64(i); acc = wrapping_add(wrapping_mul(acc, 1099511628211), f(acc, s)); } acc }";

fn fold(f: impl Fn(u64, u64) -> u64) -> u64 {
    let mut acc: u64 = 14695981039346656037;
    for s in 0..80 {
        acc = acc.wrapping_mul(1099511628211).wrapping_add(f(acc, s));
    }
    acc
}

fn shl(x: u64, s: u64) -> u64 {
    if s < 64 { x << s } else { 0 }
}
fn shr(x: u64, s: u64) -> u64 {
    if s < 64 { x >> s } else { 0 }
}

/// A test name, the Vesper `f`, and its Rust model.
type Case = (&'static str, &'static str, fn(u64, u64) -> u64);

#[test]
fn guarded_shifts_are_total() {
    let cases: [Case; 9] = [
        (
            "lt",
            "fn f(x: u64, s: u64) -> u64 { if s < 64 { x << s } else { 0 } }",
            shl,
        ),
        (
            "le",
            "fn f(x: u64, s: u64) -> u64 { if s <= 63 { x >> s } else { 0 } }",
            shr,
        ),
        (
            "gt_flipped",
            "fn f(x: u64, s: u64) -> u64 { if 64 > s { x << s } else { 0 } }",
            shl,
        ),
        (
            "else_branch",
            "fn f(x: u64, s: u64) -> u64 { if s >= 64 { 0 } else { x >> s } }",
            shr,
        ),
        (
            "not",
            "fn f(x: u64, s: u64) -> u64 { if !(s >= 64) { x << s } else { 0 } }",
            shl,
        ),
        (
            "early_return",
            "fn f(x: u64, s: u64) -> u64 { if s >= 64 { return 0; } x >> s }",
            shr,
        ),
        (
            "copied",
            "fn f(x: u64, s: u64) -> u64 { if s < 64 { let t = s; x << t } else { 0 } }",
            shl,
        ),
        (
            "mask",
            "fn f(x: u64, s: u64) -> u64 { x << (s & 63) }",
            |x, s| x << (s & 63),
        ),
        (
            "rem",
            "fn f(x: u64, s: u64) -> u64 { x >> (s % 64) }",
            |x, s| x >> (s % 64),
        ),
    ];
    for (name, f, model) in cases {
        let source = format!("{f} {DRIVER}");
        differential(name, &source, fold(model));
    }
}

/// A signed amount needs both bounds: `0 <= s` and `s < bits`.
#[test]
fn signed_amounts_need_both_bounds() {
    let source = format!(
        "fn f(x: u64, u: u64) -> u64 {{ let s: i64 = wrapping_sub(wrapping_i64(u), 8); \
            if s >= 0 && s < 64 {{ x << s }} else {{ 1 }} }} {DRIVER}"
    );
    differential(
        "signed",
        &source,
        fold(|x, u| {
            let s = u as i64 - 8;
            if (0..64).contains(&s) { x << s } else { 1 }
        }),
    );
    needs_effect("fn f(x: u64, s: i64) -> u64 { if s < 64 { x << s } else { 0 } }");
    needs_effect("fn f(x: u64, s: i64) -> u64 { if s >= 0 { x << s } else { 0 } }");
}

/// A narrower kind's width is the bound; the guard must be below it.
#[test]
fn bounds_follow_the_shifted_kind() {
    let source = format!(
        "fn f(x: u64, s: u64) -> u64 {{ let y: u32 = wrapping_u32(x); \
            if s < 32 {{ widen_u64(y >> s) }} else {{ 7 }} }} {DRIVER}"
    );
    differential(
        "u32",
        &source,
        fold(|x, s| {
            if s < 32 {
                u64::from((x as u32) >> s)
            } else {
                7
            }
        }),
    );
    needs_effect("fn f(x: u32, s: u64) -> u32 { if s < 33 { x << s } else { 0 } }");
    needs_effect("fn f(x: u64, s: u64) -> u64 { if s < 65 { x << s } else { 0 } }");
}

#[test]
fn guarded_division_and_narrowing_are_total() {
    let source = format!(
        "fn f(x: u64, s: u64) -> u64 {{ \
            let d: i64 = wrapping_sub(wrapping_i64(s), 40); let a: i64 = wrapping_i64(x); \
            let q: i64 = if d > 0 {{ a / d }} else {{ 3 }}; \
            let r: i64 = if d < -1 {{ a % d }} else {{ 5 }}; \
            let n: u64 = if s < 256 {{ widen_u64(narrow_u8(s)) }} else {{ 9 }}; \
            let u: u64 = if s > 0 {{ x / s }} else {{ 11 }}; \
            wrapping_add(wrapping_add(wrapping_u64(q), wrapping_u64(r)), wrapping_add(n, u)) }} {DRIVER}"
    );
    differential(
        "division",
        &source,
        fold(|x, s| {
            let d = s as i64 - 40;
            let a = x as i64;
            let q = if d > 0 { a / d } else { 3 };
            let r = if d < -1 { a % d } else { 5 };
            let n = if s < 256 { s } else { 9 };
            let u = x.checked_div(s).unwrap_or(11);
            (q as u64)
                .wrapping_add(r as u64)
                .wrapping_add(n.wrapping_add(u))
        }),
    );
    // `d < 0` admits -1, so MIN / -1 can still overflow.
    needs_effect("fn f(a: i64, d: i64) -> i64 { if d < 0 { a / d } else { 0 } }");
    needs_effect("fn f(a: u64, d: u64) -> u64 { if d < 10 { a / d } else { 0 } }");
    needs_effect("fn f(x: u64) -> u8 { if x <= 256 { narrow_u8(x) } else { 0 } }");
}

/// Any write to the guarded local, or a condition that could write it,
/// drops the range.
#[test]
fn writes_drop_the_range() {
    for source in [
        "fn f(x: u64, s: u64) -> u64 { let mut t = s; if t < 64 { t = wrapping_add(t, 1); x << t } else { 0 } }",
        "fn f(x: u64, s: u64) -> u64 { let mut t = s; if t < 64 { t += 1; x << t } else { 0 } }",
        "fn bump(inout t: u64) { t = wrapping_add(t, 64); } \
            fn f(x: u64, s: u64) -> u64 { let mut t = s; if t < 64 { bump(inout t); x << t } else { 0 } }",
        "fn f(x: u64, s: u64, c: bool) -> u64 { let mut t = s; if t < 64 { if c { t = 70; } x << t } else { 0 } }",
        "fn f(x: u64, s: u64) -> u64 { let mut t = s; let mut y = x; \
            if t < 64 { for i in 0..3 { y = y << t; t = wrapping_add(t, 30); } } y }",
        "fn f(x: u64, s: u64) -> u64 { let mut t = s; if t < 64 && { t = 100; true } { x << t } else { 0 } }",
        "fn f(x: u64, s: u64, c: bool) -> u64 { if s < 64 || c { x << s } else { 0 } }",
        "fn f(x: u64, s: u64) -> u64 { if s < 64 { 0 } else { x << s } }",
        "fn f(x: u64, s: u64) -> u64 { if s != 64 { x << s } else { 0 } }",
        // An abort may skip the body's write, so the join forgets its range.
        "fn f(x: u64, s: u64, c: bool) -> u64 { let mut t: u64 = 100; \
            let r: i64 = handle Fail[i64] in { if c { Fail::raise(1) } t = 5; 0 } with { return v => v, raise(e, k) => e, }; x << t }",
    ] {
        needs_effect(source);
    }
    // A write that keeps the value in range keeps a range.
    let source = format!(
        "fn f(x: u64, s: u64) -> u64 {{ let mut t = s; if t < 64 {{ t = t & 31; x << t }} else {{ 0 }} }} {DRIVER}"
    );
    differential(
        "rewrite",
        &source,
        fold(|x, s| if s < 64 { x << (s & 31) } else { 0 }),
    );
    // Both branches of a join bound the value.
    let source = format!(
        "fn f(x: u64, s: u64) -> u64 {{ let mut t: u64 = 0; if s < 10 {{ t = 3; }} else {{ t = 60; }} x << t }} {DRIVER}"
    );
    differential(
        "join",
        &source,
        fold(|x, s| x << if s < 10 { 3 } else { 60 }),
    );
}

/// The guarded helper has no handler, so it inlines into its caller as a
/// plain shift.
#[test]
fn guarded_helpers_inline() {
    let source = "fn shl(x: u64, s: u64) -> u64 { if s < 64 { x << s } else { 0 } } \
        fn main() -> u64 { let mut acc: u64 = 1; for i in 0..70 { let s: u64 = wrapping_u64(i); acc = wrapping_add(acc, shl(acc, s)); } acc }";
    let c = accepted(source);
    let mut program = vesper::ir::lower_world(&c.world);
    vesper::ir_opt::optimize(&c.world, &mut program).unwrap();
    let report = ir_inline::inline(&c.world, &mut program, &ir_inline::Budget::default());
    assert_eq!(report.callees.get("shl"), Some(&1), "{report:?}");
    let main = &program.functions["main"];
    let instructions = main.blocks.iter().flat_map(|b| &b.instructions);
    assert!(
        !instructions
            .clone()
            .any(|i| matches!(&i.op, vesper::ir::Op::Call { .. })),
        "{main:?}"
    );
    assert!(!instructions.clone().any(|i| i.fallible), "{main:?}");
}
