//! Disjoint borrows (§13, self-hosting SH3 lane 2, #1241): field paths
//! through sequence elements (`xs[i].f.g = v`, `f(inout xs[i].f)`), two
//! element views of one sequence in one call (distinct constant indices, or
//! a run-time distinctness check), `xs.swap(i, j)`, copy elements lent
//! `inout`, and the refusals that stay. The evaluator, both JIT
//! configurations and an executable agree, and the JIT frees every sequence
//! exactly once.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime, seq::native_runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-disjoint-borrows-{}-{}",
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

/// `main` that runs `body` under a `Fail` handler (element indexing raises
/// `IndexOutOfBounds`).
fn handled(items: &str) -> String {
    format!(
        "{items}
fn main() -> u64 {{ handle Fail[RuntimeError] in {{ body() }} with {{ return x => x, raise(e, k) => 999, }} }}
"
    )
}

const STATE: &str = "struct State { slots: ChunkSeq[u32], n: u64 }
fn fresh(n: u64) -> State { State { slots: ChunkSeq::new[u32](), n: n } }
";

fn refused(body: &str, code: &str, needle: &str) {
    let c = checked(&format!("{STATE}{body}\nfn main() -> u64 {{ 0 }}"));
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(needle)),
        "{body}: expected {code} ({needle}): {:#?}",
        c.diagnostics
    );
}

/// Repro (G1): assignment through an element, `xs[0].n = 1`, nested field
/// paths `xs[i].inner.k = v`, and values that read the same sequence.
#[test]
fn assignment_through_an_element() {
    let source = handled(&format!(
        "{STATE}struct Outer {{ inner: Inner, s: State }}
struct Inner {{ k: u64, tag: u32 }}
fn body() -> u64 eff [Fail[RuntimeError]] {{
    let mut xs = ChunkSeq::new[State]();
    xs.push(fresh(0));
    xs.push(fresh(5));
    xs[0].n = 1;
    xs[1].n = wrapping_add(xs[1].n, 10);
    xs[0].slots = ChunkSeq::new[u32]();
    let mut os = ChunkSeq::new[Outer]();
    os.push(Outer {{ inner: Inner {{ k: 3, tag: 1 }}, s: fresh(2) }});
    let i: u64 = 0;
    os[i].inner.k = wrapping_add(os[i].inner.k, 4);
    os[i].inner.tag = 9;
    os[i].s.n = wrapping_add(xs[1].n, 1);
    let mut cs = ChunkSeq::new[Inner]();
    cs.push(Inner {{ k: 1, tag: 2 }});
    cs[0].k = 40;
    let r = wrapping_add(xs[0].n, wrapping_mul(xs[1].n, 10));
    let o = wrapping_add(os[0].inner.k, wrapping_add(wrapping_mul(widen_u64(os[0].inner.tag), 100), wrapping_mul(os[0].s.n, 1000)));
    wrapping_add(wrapping_add(r, wrapping_mul(o, 1000)), wrapping_mul(cs[0].k, 1000000000))
}}"
    ));
    // xs = [n 1, n 15]; os[0] = k 7, tag 9, s.n 16; cs[0].k = 40.
    let r = 1 + 15 * 10;
    let o = 7 + 900 + 16_000;
    assert_eq!(
        differential("assign_path", &source),
        r + o * 1000 + 40 * 1_000_000_000
    );
}

/// Repro (G1): lending a field through an element, `f(inout xs[0].slots)`,
/// methods on it, and reading a copy field of a non-copy element.
#[test]
fn lending_a_field_through_an_element() {
    let source = handled(&format!(
        "{STATE}fn fill(inout s: ChunkSeq[u32], k: u32) {{ s.push(k); s.push(wrapping_add(k, 1)); }}
fn count(in s: ChunkSeq[u32]) -> u64 {{ s.len() }}
fn body() -> u64 eff [Fail[RuntimeError]] {{
    let mut xs = ChunkSeq::new[State]();
    xs.push(fresh(0));
    xs.push(fresh(7));
    fill(inout xs[0].slots, 3);
    fill(inout xs[1].slots, 8);
    xs[1].slots.push(1);
    let c = wrapping_add(count(in xs[0].slots), wrapping_mul(xs[1].slots.len(), 10));
    let n = xs[1].n;
    let first = widen_u64(xs[1].slots[2]);
    wrapping_add(wrapping_add(c, wrapping_mul(n, 100)), wrapping_mul(first, 10000))
}}"
    ));
    assert_eq!(differential("lend_path", &source), 2 + 30 + 700 + 10_000);
}

/// Repro (G2): two element views of one sequence in one call with distinct
/// constant indices, and `xs.swap(i, j)`.
#[test]
fn distinct_constant_elements_and_seq_swap() {
    let source = handled(&format!(
        "{STATE}fn mv(inout a: State, in b: State) {{ a.n = wrapping_add(a.n, b.n); a.slots.push(9); }}
fn body() -> u64 eff [Fail[RuntimeError]] {{
    let mut xs = ChunkSeq::new[State]();
    xs.push(fresh(1));
    xs.push(fresh(2));
    xs.push(fresh(3));
    swap(inout xs[0], inout xs[1]);
    mv(inout xs[2], in xs[0]);
    xs.swap(0, 2);
    let i: u64 = 1;
    xs.swap(i, i);
    let mut ws = ChunkSeq::new[u64]();
    ws.push(1); ws.push(2); ws.push(3);
    ws.swap(0, 2);
    swap(inout ws[0], inout ws[1]);
    let a = wrapping_add(xs[0].n, wrapping_add(wrapping_mul(xs[1].n, 10), wrapping_mul(xs[2].n, 100)));
    let b = wrapping_add(xs[0].slots.len(), wrapping_mul(xs[2].slots.len(), 10));
    let w = wrapping_add(ws[0], wrapping_add(wrapping_mul(ws[1], 10), wrapping_mul(ws[2], 100)));
    wrapping_add(wrapping_add(a, wrapping_mul(b, 1000)), wrapping_mul(w, 100000))
}}"
    ));
    // swap -> [2, 1, 3]; mv xs[2] += xs[0] -> [2, 1, 5 (1 slot)];
    // xs.swap(0, 2) -> [5*, 1, 2]. ws: [3, 2, 1] -> [2, 3, 1].
    let a = 5 + 10 + 200;
    let b = 1;
    let w = 2 + 30 + 100;
    assert_eq!(
        differential("distinct", &source),
        a + b * 1000 + w * 100_000
    );
}

/// Repro (G2): `f(inout xs[i], in xs[j])` where `i != j` is not provable
/// is checked at run time: equal indices raise `IndexOutOfBounds` like an
/// index past the end.
#[test]
fn run_time_distinct_indices() {
    let source = handled(&format!(
        "{STATE}fn mv(inout a: State, in b: State) {{ a.n = wrapping_add(a.n, b.n); }}
fn step(inout xs: ChunkSeq[State], i: u64, j: u64) eff [Fail[RuntimeError]] {{ mv(inout xs[i], in xs[j]) }}
fn body() -> u64 eff [Fail[RuntimeError]] {{
    let mut xs = ChunkSeq::new[State]();
    xs.push(fresh(1));
    xs.push(fresh(20));
    step(inout xs, 0, 1);
    step(inout xs, 1, 0);
    step(inout xs, 1, 1);
    xs[1].n
}}"
    ));
    assert_eq!(differential("run_time_distinct", &source), 999);
    let ok = handled(&format!(
        "{STATE}fn mv(inout a: State, in b: State) {{ a.n = wrapping_add(a.n, b.n); }}
fn body() -> u64 eff [Fail[RuntimeError]] {{
    let mut xs = ChunkSeq::new[State]();
    xs.push(fresh(1));
    xs.push(fresh(20));
    let mut i: u64 = 0;
    loop {{ if i >= 2 {{ break; }} mv(inout xs[i], in xs[wrapping_sub(1, i)]); i = wrapping_add(i, 1); }}
    wrapping_add(xs[0].n, wrapping_mul(xs[1].n, 100))
}}"
    ));
    assert_eq!(differential("run_time_distinct_ok", &ok), 21 + 41 * 100);
}

/// Repro (G3): copy elements lent `inout` are borrowed in place.
#[test]
fn copy_elements_lent_inout() {
    let source = handled(
        "fn inc(inout x: u64) { x = wrapping_add(x, 5); }
fn inc32(inout x: u32) { x = wrapping_add(x, 7); }
struct P { a: u64, b: u64 }
fn bump(inout p: P) { p.a = wrapping_add(p.a, p.b); }
fn body() -> u64 eff [Fail[RuntimeError]] {
    let mut xs = ChunkSeq::new[u64]();
    xs.push(1); xs.push(2);
    inc(inout xs[1]);
    let mut ns = ChunkSeq::new[u32]();
    ns.push(1); ns.push(2);
    inc32(inout ns[0]);
    let mut ps = ChunkSeq::new[P]();
    ps.push(P { a: 1, b: 2 });
    bump(inout ps[0]);
    swap(inout xs[0], inout xs[1]);
    wrapping_add(wrapping_add(xs[0], wrapping_mul(xs[1], 10)), wrapping_add(wrapping_mul(widen_u64(ns[0]), 100), wrapping_mul(ps[0].a, 1000)))
}",
    );
    assert_eq!(differential("copy_inout", &source), 7 + 10 + 800 + 3000);
}

/// Repro (gap 4 of SH2 lane 14): a field of a lent receiver. `in st` reaches
/// every field, so lending `st.err` `inout` beside it stays refused, with a
/// diagnostic that says how to split the borrow.
#[test]
fn field_of_a_lent_receiver_is_refused_clearly() {
    refused(
        "struct St { names: ChunkSeq[u32], err: u64 }
fn put_name(in st: St, inout err: u64, x: u32) { err = wrapping_add(err, st.names.len()); }
fn f(inout st: St) { put_name(in st, inout st.err, 3) }",
        "E0301",
        "overlapping arguments",
    );
    let c = checked(
        "struct St { names: ChunkSeq[u32], err: u64 }
fn put_name(in st: St, inout err: u64, x: u32) { err = wrapping_add(err, st.names.len()); }
fn f(inout st: St) { put_name(in st, inout st.err, 3) }
fn main() -> u64 { 0 }",
    );
    assert!(
        c.diagnostics.iter().any(|d| d
            .help
            .as_deref()
            .is_some_and(|h| h.contains("reaches every field, including 'st.err'"))),
        "{:#?}",
        c.diagnostics
    );
}

/// Repro (G4): an element lent to a call raises `IndexOutOfBounds`, so with
/// a live linear value in scope it stays `E0402`; the total `try_get_mut`
/// form is the way to write it.
#[test]
fn raising_view_with_live_linear_value() {
    refused(
        "linear struct Ticket { id: u64 }
fn stamp(inout s: State) { s.n = 1; }
fn f(inout s: ChunkSeq[State]) eff [Fail[RuntimeError]] {
    let t = Ticket { id: 1 };
    stamp(inout s[0]);
    let Ticket { id } = t;
}",
        "E0402",
        "",
    );
    let c = checked(&format!(
        "{STATE}linear struct Ticket {{ id: u64 }}
fn stamp(inout s: State) {{ s.n = 1; }}
fn f(inout s: ChunkSeq[State]) {{
    let t = Ticket {{ id: 1 }};
    match s.try_get_mut(0) {{ Some(x) => stamp(inout x), None => {{}} }}
    let Ticket {{ id }} = t;
}}
fn main() -> u64 {{ 0 }}"
    ));
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
}

/// Overlap stays refused where it is not provably disjoint.
#[test]
fn overlapping_element_borrows_stay_refused() {
    for (body, code, needle) in [
        (
            "fn g(inout a: State, in b: State) {}
fn f(inout xs: ChunkSeq[State]) eff [Fail[RuntimeError]] { g(inout xs[0], in xs[0]) }",
            "E0301",
            "overlapping",
        ),
        (
            "fn g(inout a: ChunkSeq[u32], in b: ChunkSeq[State]) {}
fn f(inout xs: ChunkSeq[State]) eff [Fail[RuntimeError]] { g(inout xs[0].slots, in xs) }",
            "E0301",
            "",
        ),
        (
            "fn f(inout xs: ChunkSeq[State]) eff [Fail[RuntimeError]] { xs[0].n = xs.len(); xs.clear(); }
fn g(in xs: ChunkSeq[State]) eff [Fail[RuntimeError]] { xs[0].n = 1; }",
            "E0307",
            "",
        ),
    ] {
        refused(body, code, needle);
    }
}

/// G9: functions that hold non-copy element views inline. A matched
/// `try_get` is a borrow wherever it is spliced, so the caller's drop
/// elaboration never frees the element; the JIT's frees prove it.
#[test]
fn functions_with_element_views_inline() {
    let source = handled(&format!(
        "{STATE}fn n_at(in xs: ChunkSeq[State], i: u64) -> u64 {{
    match xs.try_get(i) {{ Some(s) => wrapping_add(s.n, s.slots.len()), None => 0 }}
}}
fn bump_at(inout xs: ChunkSeq[State], i: u64) {{
    match xs.try_get_mut(i) {{ Some(s) => {{ s.n = wrapping_add(s.n, 1); s.slots.push(1); }}, None => {{}} }}
}}
fn set_n(inout xs: ChunkSeq[State], i: u64, v: u64) eff [Fail[RuntimeError]] {{ xs[i].n = v; }}
fn body() -> u64 eff [Fail[RuntimeError]] {{
    let mut xs = ChunkSeq::new[State]();
    xs.push(fresh(1));
    xs.push(fresh(2));
    let mut t: u64 = 0;
    let mut i: u64 = 0;
    loop {{
        if i >= 6 {{ break; }}
        bump_at(inout xs, i & 1);
        t = wrapping_add(t, n_at(in xs, i & 1));
        i = wrapping_add(i, 1);
    }}
    set_n(inout xs, 0, 50);
    wrapping_add(t, wrapping_mul(n_at(in xs, 0), 1000))
}}"
    ));
    let c = accepted(&source);
    let mut program = vesper::ir::lower_world(&c.world);
    vesper::ir_opt::optimize(&c.world, &mut program).unwrap();
    let report = vesper::ir_inline::inline(
        &c.world,
        &mut program,
        &vesper::ir_inline::Budget::default(),
    );
    for callee in ["n_at", "bump_at", "set_n"] {
        assert!(
            report.callees.get(callee).is_some_and(|n| *n > 0),
            "{callee} was not inlined: {:?}",
            report.refused.get(callee)
        );
    }
    // n after each bump at index i & 1 (0, 1, 0, 1, ..): xs[0] and xs[1]
    // gain 1 to n and one slot per bump.
    let mut n = [1u64, 2];
    let mut slots = [0u64, 0];
    let mut t = 0;
    for i in 0..6 {
        n[i & 1] += 1;
        slots[i & 1] += 1;
        t += n[i & 1] + slots[i & 1];
    }
    assert_eq!(
        differential("inline_views", &source),
        t + (50 + slots[0]) * 1000
    );
}
