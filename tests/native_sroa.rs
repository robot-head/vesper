//! Copy-struct locals in registers (P3b gaps lane 5, #1090).
//!
//! On the native optimize path a local whose type is a struct or tuple of
//! scalar words is one variable per word: field reads and writes touch no
//! memory, an aggregate read stores the words into its home (the result,
//! a field of `Option::Some(s)`) or a fresh copy, and `Some(h) => h.f` on a
//! sequence element loads only the fields it reads. Every program runs in
//! the evaluator, the JIT with the optimizer on and off, x86-64 and AArch64
//! objects and, where a C compiler links, an executable; all must agree.
//! Recursive helpers stay calls, so the split paths run in the JIT as well
//! as in inlined forms.
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
            "vesper-sroa-test-{}-{}",
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

/// The machine code of the one emitted function that takes `params` `i64`
/// parameters, as CLIF after optimization.
fn function_clif(source: &str, params: usize) -> String {
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
    bodies[0].to_owned()
}

const NORMALIZE: &str = r#"
copy struct Tnum {
    value: u64,
    mask: u64
}

copy struct Scalar {
    umin: u64,
    umax: u64,
    smin: i64,
    smax: i64,
    var: Tnum
}

fn meet(a: Tnum, b: Tnum, depth: u64, salt: u64) -> Option[Tnum] {
    if depth > 0 {
        return meet(a, b, wrapping_sub(depth, 1), salt);
    }
    if (a.value & b.mask) == wrapping_add(77, salt) {
        return Option::None;
    }
    Option::Some(Tnum { value: a.value | (b.value & 15), mask: a.mask & b.mask })
}

fn normalize(input: Scalar, k: u64, depth: u64) -> Option[Scalar] {
    if depth > 0 {
        return normalize(input, k, wrapping_sub(depth, 1));
    }
    let mut s = input;
    let n: u64 = 3;
    for round in 0..n {
        s.umin = wrapping_add(s.umin, k);
        s.umax = s.umax | 64;
        s.smin = wrapping_sub(s.smin, 1);
        if s.umin > s.umax || s.smin > s.smax {
            return Option::None;
        }
        if s.smin >= 0 {
            s.smax = wrapping_add(s.smax, 2);
        } else if s.umin > 50 {
            s.smin = 0;
        }
        let probe = Tnum { value: s.umin, mask: s.umax };
        match meet(s.var, probe, 1, 0) {
            Option::Some(t) => {
                s.var = t;
            },
            Option::None => {
                return Option::None;
            },
        }
    }
    // A copy is independent of its source.
    let mut t = s;
    t.var.mask = 5;
    t.umin = 0;
    s.var.value = wrapping_add(s.var.value, t.var.mask);
    Option::Some(s)
}

fn score(o: Option[Scalar]) -> u64 {
    match o {
        Option::Some(s) => wrapping_add(
            wrapping_add(wrapping_mul(s.umin, 3), wrapping_mul(s.umax, 5)),
            wrapping_add(
                wrapping_add(wrapping_mul(s.var.value, 7), wrapping_mul(s.var.mask, 11)),
                wrapping_add(wrapping_mul(bits(s.smin), 13), wrapping_mul(bits(s.smax), 17))
            )
        ),
        Option::None => 1000
    }
}

fn bits(x: i64) -> u64 {
    if x < 0 { wrapping_add(1000000, 1) } else { 3 }
}

fn main() -> u64 {
    let base = Scalar { umin: 1, umax: 100, smin: -3, smax: 40, var: Tnum { value: 8, mask: 255 } };
    let mut acc: u64 = 0;
    let n: u64 = 6;
    for i in 0..n {
        acc = wrapping_add(acc, score(normalize(base, i, 1)));
        acc = wrapping_add(acc, score(normalize(base, wrapping_mul(i, 40), 0)));
    }
    let blocked = Scalar { umin: 1, umax: 100, smin: 3, smax: 4, var: Tnum { value: 77, mask: 255 } };
    acc = wrapping_add(acc, score(normalize(blocked, 0, 0)));
    let mut q = base;
    q.umin = 9;
    wrapping_add(acc, wrapping_add(q.umin, base.umin))
}
"#;

/// The `scalar_normalize` shape of the eBPF port: a mutable copy of a
/// nested copy struct updated field by field in a loop, early `None`
/// returns, an `Option[Scalar]` result, an `in` use of a nested field and a
/// whole-field write from a match binding, and a copy mutated apart from
/// its source.
#[test]
fn normalize_agrees() {
    assert_eq!(differential("normalize", NORMALIZE), 91019963);
}

/// The loop of `normalize` keeps `s` in registers: its fields are neither
/// loaded nor stored inside the loop, so the only memory traffic left is
/// the copy of `input` in, the `in` argument of `meet` and the result.
#[test]
fn normalize_keeps_fields_in_registers() {
    let clif = function_clif(NORMALIZE, 4);
    let loads = clif.matches(" = load.").count();
    let stores = clif.matches(" store ").count();
    eprintln!("normalize: {loads} loads, {stores} stores");
    assert!(loads <= 14, "{loads} loads:\n{clif}");
    assert!(stores <= 14, "{stores} stores:\n{clif}");
}

const KINDS: &str = r#"
copy struct Mixed {
    a: u8,
    b: bool,
    c: i32,
    d: f64,
    e: u16,
    f: f32
}

copy struct Two {
    x: u64,
    y: u64
}

copy struct Pair {
    left: Mixed,
    right: Two
}

fn tweak(inout m: Mixed, by: u8) {
    m.a = wrapping_add(m.a, by);
}

fn weigh(p: Pair, depth: u64) -> u64 {
    if depth > 0 {
        return weigh(p, wrapping_sub(depth, 1));
    }
    let b: u64 = if p.left.b { 1 } else { 0 };
    let d: u64 = if p.left.d > 2.5 { 10 } else { 20 };
    let f: u64 = if p.left.f > 1.5 { 100 } else { 200 };
    let c: u64 = if p.left.c < 0 { 1000 } else { 2000 };
    wrapping_add(
        wrapping_add(wrapping_add(b, d), wrapping_add(f, c)),
        wrapping_add(wrapping_add(narrow(p.left.a), wide(p.left.e)), wrapping_add(p.right.x, wrapping_mul(p.right.y, 3)))
    )
}

fn narrow(x: u8) -> u64 {
    if x > 200 { 7 } else if x > 8 { 70 } else { 700 }
}

fn wide(x: u16) -> u64 {
    if x == 7 { 5 } else { 50 }
}

fn build(x: u64) -> Pair {
    let mut p = Pair {
        left: Mixed { a: 250, b: false, c: 5, d: 1.0, e: 7, f: 1.0 },
        right: Two { x: x, y: 2 }
    };
    p.left.a = wrapping_add(p.left.a, 9);
    p.left.b = !p.left.b;
    p.left.c = wrapping_sub(p.left.c, 9);
    p.left.d = p.left.d * 3.0;
    p.left.f = p.left.f + 1.0;
    p.right.y = wrapping_add(p.right.y, x);
    p
}

fn main() -> u64 {
    let mut acc: u64 = 0;
    let n: u64 = 4;
    for i in 0..n {
        let p = build(i);
        acc = wrapping_add(acc, weigh(p, 1));
        // An `inout` use keeps a local in memory: still correct.
        let mut m = p.left;
        tweak(inout m, 3);
        acc = wrapping_add(acc, narrow(m.a));
    }
    acc
}
"#;

/// Narrow integers, `bool`, `f64` and `f32` words and nested structs; an `inout` use beside split locals.
#[test]
fn word_kinds_agree() {
    assert_eq!(differential("kinds", KINDS), 10112);
}

const ELEMENTS: &str = r#"
copy struct Hdr {
    base: u64,
    sources: u64,
    root: bool,
    children: u64,
    ie: bool,
    off: u64
}

fn total(in hs: ChunkSeq[Hdr], depth: u64) -> u64 {
    if depth > 0 {
        return total(in hs, wrapping_sub(depth, 1));
    }
    let mut acc: u64 = 0;
    let mut i: u64 = 0;
    loop {
        match hs.try_get(i) {
            Option::Some(h) => {
                acc = wrapping_add(acc, h.sources);
                if h.root {
                    acc = wrapping_add(acc, 1000);
                }
            },
            Option::None => {
                return acc;
            },
        }
        i = wrapping_add(i, 1);
    }
}

fn main() -> u64 {
    let mut hs: ChunkSeq[Hdr] = ChunkSeq::new[Hdr]();
    let n: u64 = 9;
    for i in 0..n {
        hs.push(Hdr { base: i, sources: wrapping_mul(i, 3), root: i == 4, children: 1, ie: false, off: 8 });
    }
    let first = total(in hs, 1);
    // A binding is a copy: replacing the element afterwards leaves it.
    let kept: u64 = match hs.try_get(2) {
        Option::Some(h) => {
            hs.try_set(2, Hdr { base: 0, sources: 500, root: false, children: 0, ie: true, off: 0 });
            wrapping_add(h.sources, h.base)
        },
        Option::None => 0
    };
    wrapping_add(wrapping_mul(first, 10), wrapping_add(kept, total(in hs, 0)))
}
"#;

/// `Some(h) => h.f` on a sequence element reads the element's fields, and
/// the binding stays a copy when the element is replaced afterwards.
#[test]
fn element_field_reads_agree() {
    assert_eq!(differential("elements", ELEMENTS), 12690);
}
