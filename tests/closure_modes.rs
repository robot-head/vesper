//! Closure parameter modes and lent closures (§4.2, self-hosting SH3 lane
//! 3, #1242). A closure may take `in` and `inout` parameters. A closure
//! literal written as the argument for a function parameter that is passed
//! `in` and whose function type is not `copy` is lent: it borrows the names
//! it uses `in` for that call instead of capturing them, so it may use the
//! caller's borrowed parameters and non-copy locals without moving them.
//! The parameter it is lent to may be called any number of times, and only
//! called or lent again. The evaluator, both JIT configurations and an
//! executable agree, and the JIT frees every sequence exactly once.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, fip, native, parser, runtime::Runtime, seq::native_runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-closure-modes-{}-{}",
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
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check::check(&p.module)
}

fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    let program = vesper::ir::lower_world(&c.world);
    let d = vesper::ir_check::validate(&c.world, &program);
    assert!(d.is_empty(), "Place IR check: {d:#?}");
    c
}

fn links() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Runs `main` in the evaluator, the JIT (optimizer on and off) and, where
/// a C compiler links, an executable; all must agree. The JIT must free
/// every sequence itself and never free one that is not live.
fn differential(name: &str, source: &str) -> u64 {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    for optimize in [true, false] {
        let stray = native_runtime::stray_frees();
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            jit, evaluated,
            "{name}: JIT (optimize {optimize}) vs evaluator"
        );
        assert_eq!(
            native_runtime::teardown_frees(),
            0,
            "{name}: JIT (optimize {optimize}) left sequences for teardown"
        );
        assert_eq!(
            native_runtime::stray_frees(),
            stray,
            "{name}: JIT (optimize {optimize}) freed a sequence that was not live"
        );
    }
    let expected: u64 = evaluated.parse().unwrap();
    if links() {
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
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(
            run.status.code(),
            Some((expected & 0xff) as i32),
            "{name}: executable stderr={}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    expected
}

fn refused(source: &str, code: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code}, got {:#?}",
        c.diagnostics
    );
}

const STATE: &str = "struct State { slots: ChunkSeq[u32], n: u64 }
fn fresh(n: u64) -> State {
    let mut s = State { slots: ChunkSeq::new[u32](), n: n };
    let mut i: u64 = 0;
    loop {
        if i >= n % 4 {
            break;
        }
        s.slots.push(wrapping_u32(i));
        i = wrapping_add(i, 1);
    }
    s
}
struct Tab[V] { vals: ChunkSeq[V] }
impl [V: mod affine] Tab[V] {
    pub fn with_ref[R, e: eff](in self, i: u64, f: fn(in V) -> R eff [e]) -> Option[R] eff [e] {
        match self.vals.try_get(i) {
            Option::Some(v) => Option::Some(f(in v)),
            Option::None => Option::None,
        }
    }
    pub fn with_mut[R, e: eff](inout self, i: u64, f: fn(inout V) -> R eff [e]) -> Option[R] eff [e] {
        match self.vals.try_get_mut(i) {
            Option::Some(v) => Option::Some(f(inout v)),
            Option::None => Option::None,
        }
    }
}
fn or0(x: Option[u64]) -> u64 {
    match x {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
";

/// Repro 1: `in` and `inout` closure parameters of a struct that owns a
/// sequence.
#[test]
fn in_and_inout_closure_parameters() {
    let source = format!(
        "{STATE}fn main() -> u64 {{
    let mut s = fresh(41);
    s.slots.push(7);
    let get = |in v: State| wrapping_add(v.n, v.slots.len());
    let set = |inout v: State| {{ v.n = 1; v.slots.push(9); }};
    let a = get(in s);
    set(inout s);
    let b = get(in s);
    wrapping_add(wrapping_mul(a, 100), b)
}}"
    );
    assert_eq!(differential("modes", &source), 4304);
}

/// Repro 2: a collection's `with_ref`/`with_mut` take lent closures that
/// read the caller's non-copy local and borrowed parameter in place,
/// several times, with an inferred parameter type and an effect row.
#[test]
fn lent_closures_borrow_the_callers_names() {
    let source = format!(
        "{STATE}fn probe(in t: Tab[State], in key: State) -> u64 {{
    or0(t.with_ref(0, |in v| wrapping_add(v.n, key.slots.len())))
}}
fn bump(inout t: Tab[State], in key: State) -> u64 {{
    or0(t.with_mut(1, |inout v| {{ v.n = wrapping_add(v.n, key.n); v.slots.push(1); v.slots.len() }}))
}}
fn main() -> u64 {{
    let mut t = Tab {{ vals: ChunkSeq::new[State]() }};
    t.vals.push(fresh(41));
    t.vals.push(fresh(6));
    let key = fresh(3);
    let mut total: u64 = 0;
    for i in 0..5 {{
        total = wrapping_add(total, probe(in t, in key));
        total = wrapping_add(total, or0(t.with_ref(i % 3, |in v: State| wrapping_add(v.n, key.n))));
    }}
    total = wrapping_add(total, bump(inout t, in key));
    let r = handle Fail[RuntimeError] in {{
        widen_u64(match t.with_ref(1, |in v| v.slots[wrapping_sub(key.n, 1)]) {{ Option::Some(x) => x, Option::None => 0 }})
    }} with {{ return x => x, raise(e, k) => 500, }};
    let missing = handle Fail[RuntimeError] in {{
        widen_u64(match t.with_ref(0, |in v| v.slots[key.n]) {{ Option::Some(x) => x, Option::None => 0 }})
    }} with {{ return x => x, raise(e, k) => 500, }};
    wrapping_add(wrapping_add(wrapping_add(total, r), missing), wrapping_add(key.n, or0(t.with_ref(1, |in v| v.n))))
}}"
    );
    // probe: 5 * (41 + 3); with_ref over indexes 0, 1, 2, 0, 1: 2 * (44 +
    // 9); bump: 3 slots; r: slots[2] = 1; missing: slots[3] raises, 500;
    // key 3 + n 9.
    assert_eq!(
        differential("borrow", &source),
        220 + 106 + 3 + 1 + 500 + 12
    );
}

/// A lent function parameter may be lent again: passed on, or used by a
/// lent closure, and called any number of times.
#[test]
fn lent_parameters_pass_on() {
    let source = format!(
        "{STATE}fn apply(f: fn(u64) -> u64) -> u64 {{ f(1) }}
fn twice(f: fn(u64) -> u64) -> u64 {{ wrapping_add(apply(f), apply(|x: u64| f(f(x)))) }}
fn main() -> u64 {{
    let s = fresh(5);
    wrapping_add(twice(|x: u64| wrapping_add(x, s.slots.len())), twice(|x: u64| wrapping_add(x, s.n)))
}}"
    );
    // twice(+1): 2 + 3; twice(+5): 6 + 11.
    assert_eq!(differential("pass_on", &source), 22);
}

/// A `copy` value lent `in` (every by-value parameter is) is captured as a
/// copy by any closure; that was `E0310`.
#[test]
fn copy_parameters_are_captured_by_copy() {
    let source = "fn apply(f: fn@copy(u64) -> u64, x: u64) -> u64 { f(x) }
fn g(y: u64) -> u64 { let h = |x: u64| wrapping_add(x, y); wrapping_add(apply(h, 1), h(2)) }
fn main() -> u64 { g(40) }";
    assert_eq!(differential("copy_capture", source), 83);
}

#[test]
fn lent_closures_and_parameters_are_second_class() {
    let tab = STATE;
    // The closure borrows `t` while the call lends it `inout`.
    refused(
        &format!(
            "{tab}fn main() -> u64 {{ let mut t = Tab {{ vals: ChunkSeq::new[State]() }}; or0(t.with_mut(0, |inout v| {{ v.n = t.vals.len(); 0 }})) }}"
        ),
        "E0301",
    );
    // ... or moves it.
    refused(
        &format!(
            "{tab}fn apply(f: fn(u64) -> u64, sink s: State) -> u64 {{ f(1) }} fn main() -> u64 {{ let s = fresh(1); apply(|x: u64| s.n, s) }}"
        ),
        "E0301",
    );
    // A lent parameter cannot escape: stored, bound, returned or captured.
    for body in [
        "copy struct Cb { f: fn@copy(u64) -> u64 } fn keep(f: fn(u64) -> u64) -> Cb { Cb { f: f } }",
        "fn keep(f: fn(u64) -> u64) -> u64 { let g = f; g(1) }",
        "fn keep(f: fn(u64) -> u64) -> fn(u64) -> u64 { f }",
    ] {
        refused(&format!("{tab}{body} fn main() -> u64 {{ 0 }}"), "E0305");
    }
    refused(
        &format!(
            "{tab}fn keep(f: fn(u64) -> u64) -> u64 {{ let h = |x: u64| f(x); h(1) }} fn main() -> u64 {{ 0 }}"
        ),
        "E0310",
    );
    // Only a closure literal, a copy function or a lent parameter is lent:
    // a closure value that owns its captures is called at most once.
    refused(
        &format!(
            "{tab}fn apply(f: fn(u64) -> u64) -> u64 {{ f(1) }} fn main() -> u64 {{ let s = fresh(1); let h = |x: u64| wrapping_add(x, s.slots.len()); apply(h) }}"
        ),
        "E0310",
    );
    // Inside the lent closure the borrowed names are read-only.
    refused(
        &format!(
            "{tab}fn apply(f: fn(u64) -> u64) -> u64 {{ f(1) }} fn main() -> u64 {{ let mut s = fresh(1); apply(|x: u64| {{ s.n = 2; x }}) }}"
        ),
        "E0307",
    );
    refused(
        &format!(
            "{tab}fn take(sink s: State) -> u64 {{ s.n }} fn apply(f: fn(u64) -> u64) -> u64 {{ f(1) }} fn main() -> u64 {{ let s = fresh(1); apply(|x: u64| take(s)) }}"
        ),
        "E0305",
    );
}

/// A lent closure borrows a `Box` tree: the closure walks it in place, the
/// caller still owns it afterwards, and the JIT frees every box once.
#[test]
fn lent_closures_borrow_box_trees() {
    let source = "enum Expr {
    Lit(u64),
    Neg(Box[Expr]),
    Add(Box[Expr], Box[Expr])
}
fn eval(in e: Expr) -> u64 {
    match e {
        Expr::Lit(n) => n,
        Expr::Neg(x) => match x { Box(y) => wrapping_sub(0, eval(in y)) },
        Expr::Add(a, b) => match a { Box(x) => match b { Box(y) => wrapping_add(eval(in x), eval(in y)) } },
    }
}
fn apply(f: fn(u64) -> u64) -> u64 { wrapping_add(f(1), f(2)) }
fn main() -> u64 {
    let t = Expr::Add(Box::new(Expr::Lit(40)), Box::new(Expr::Neg(Box::new(Expr::Lit(4)))));
    let r = apply(|x: u64| wrapping_add(x, eval(in t)));
    wrapping_add(r, eval(in t))
}";
    for optimize in [true, false] {
        let c = accepted(source);
        fip::native_runtime::reset_counts();
        let got = native::jit_with_options(&c.world, "main", optimize).unwrap();
        assert_eq!(got.to_string(), "111", "optimize {optimize}");
        assert!(fip::native_runtime::alloc_count() > 0);
        assert_eq!(
            fip::native_runtime::free_count(),
            fip::native_runtime::alloc_count(),
            "optimize {optimize}: every box freed once"
        );
    }
    assert_eq!(differential("box_tree", source), 111);
}
