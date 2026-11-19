//! Two-phase `inout` argument loans (§4.3, P3b gaps lane 9, #1094): every
//! `inout` argument loan of a call, not only the receiver, is reserved while
//! the call's other arguments are evaluated. Arguments may read the place
//! (through a nested `in` call or a scalar copy) and see its value before the
//! call; writes, moves, `inout` loans and live `in` loans of the place stay
//! refused. The evaluator, both JIT configurations and an executable agree.
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
            "vesper-two-phase-args-{}-{}",
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

/// The Chorus, Pact, eBPF and Loom audit repros from the port reports.
#[test]
fn port_repros_read_the_reserved_argument() {
    // port-p3b-chorus.md gap 1.
    let chorus = "struct C { x: u64, y: u64 }
fn g(in c: C) -> u64 { c.x }
fn ga(in c: C, n: u64) -> u64 { wrapping_add(c.y, n) }
fn f(inout c: C, v: u64) { c.x = wrapping_add(wrapping_mul(c.x, 10), v); }
fn h(inout c: C) { f(inout c, g(in c)); }
fn main() -> u64 {
    let mut c = C { x: 4, y: 7 };
    h(inout c);
    f(inout c, ga(in c, 3));
    c.x
}
";
    // c.x = 40 + 4 = 44, then 440 + 10.
    assert_eq!(differential("chorus", chorus), 450);
    // port-p3-pact.md gap 1 and the Loom audit field read.
    let pact = "fn get(in s: ChunkSeq[u64], i: u64) -> u64 {
    match s.try_get(i) { Option::Some(x) => x, Option::None => 0 }
}
fn put(inout s: ChunkSeq[u64], v: u64) { s.push(v); }
struct St { s: ChunkSeq[u64], n: u64 }
fn note(inout st: St, v: u64) { st.n = wrapping_add(wrapping_mul(st.n, 100), v); st.s.push(v); }
fn main() -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.push(5);
    put(inout s, get(in s, 0));
    put(inout s, s.len());
    let mut st = St { s: ChunkSeq::new[u64](), n: 3 };
    note(inout st, st.n);
    note(inout st, st.s.len());
    note(inout st, get(in st.s, 0));
    let r = wrapping_add(wrapping_mul(get(in s, 2), 1000000), wrapping_add(wrapping_mul(get(in s, 1), 10000000), st.n));
    let St { s: t, n: _ } = st;
    r
}
";
    // s = [5, 5, 2]; st.n = 3 -> 303 -> 30301 -> 3030103.
    assert_eq!(
        differential("pact", pact),
        50_000_000 + 2_000_000 + 3_030_103
    );
    // port-p3-ebpf.md gap 7: a verifier state read in a nested call, with
    // several reserved loans in one call.
    let ebpf = "struct V { regs: ChunkSeq[u64], pc: u64 }
fn reg(in v: V, i: u64) -> u64 { match v.regs.try_get(i) { Option::Some(x) => x, Option::None => 0 } }
fn set(inout v: V, i: u64, x: u64) -> bool { v.pc = wrapping_add(v.pc, 1); v.regs.try_set(i, x) }
fn at(in s: ChunkSeq[u64], i: u64) -> u64 { match s.try_get(i) { Option::Some(x) => x, Option::None => 0 } }
fn both(inout a: V, inout b: ChunkSeq[u64], x: u64, y: u64) { a.pc = wrapping_add(a.pc, x); b.push(y); }
fn main() -> u64 {
    let mut v = V { regs: ChunkSeq::new[u64](), pc: 0 };
    v.regs.push(1); v.regs.push(2);
    let ok = set(inout v, 1, wrapping_add(reg(in v, 0), reg(in v, 1)));
    let mut log: ChunkSeq[u64] = ChunkSeq::new[u64]();
    both(inout v, inout log, reg(in v, 1), wrapping_add(log.len(), v.pc));
    both(inout v, inout log, v.pc, log.len());
    let r = wrapping_add(wrapping_mul(v.pc, 1000), wrapping_add(wrapping_mul(reg(in v, 1), 100), wrapping_add(wrapping_mul(at(in log, 0), 10), at(in log, 1))));
    let V { regs: t, pc: _ } = v;
    if ok { r } else { 0 }
}
";
    // regs = [1, 3], pc = 1; pc = 4, log = [1]; pc = 8, log = [1, 1].
    assert_eq!(differential("ebpf", ebpf), 8000 + 300 + 10 + 1);
}

/// Scalars read from a reserved place are copied before the callee runs,
/// even when the callee writes the place before it reads the parameter.
#[test]
fn scalar_reads_see_the_value_before_the_call() {
    let source = "struct Q { a: u64, b: bool, f: f64 }
fn f(inout q: Q, in a: u64, b: bool, f: f64) -> u64 {
    q.a = 100;
    q.b = false;
    q.f = 0.5;
    let fb: u64 = if f > 1.0 { 7 } else { 0 };
    let bb: u64 = if b { 1 } else { 0 };
    wrapping_add(wrapping_mul(a, 10), wrapping_add(bb, fb))
}
fn main() -> u64 {
    let mut q = Q { a: 3, b: true, f: 2.0 };
    let r = f(inout q, q.a, q.b, q.f);
    wrapping_add(wrapping_mul(r, 1000), q.a)
}
";
    assert_eq!(differential("scalars", source), 38_100);
}

#[test]
fn writes_moves_inout_and_live_loans_stay_refused() {
    for (body, code, needle) in [
        (
            "fn put(inout s: ChunkSeq[u64], v: u64) { s.push(v); }
fn f(inout s: ChunkSeq[u64]) { put(inout s, { s.clear(); 1 }); }",
            "E0301",
            "conflicts with active argument loan",
        ),
        // port-p3b-warp.md side note: the inner `inout` loan of a nested
        // call is a write of the reserved place.
        (
            "fn w8(inout out: ChunkSeq[u64], at: u64, v: u64) -> u64 { out.push(v); wrapping_add(at, 1) }
fn f(inout o: ChunkSeq[u64]) -> u64 { w8(inout o, w8(inout o, 0, 1), 2) }",
            "E0301",
            "conflicts with active argument loan",
        ),
        (
            "fn take(sink s: ChunkSeq[u64]) -> u64 { s.len() }
fn put(inout s: ChunkSeq[u64], v: u64) { s.push(v); }
fn f(inout s: ChunkSeq[u64]) { put(inout s, take(s)); }",
            "E0305",
            "",
        ),
        (
            "fn two(inout a: ChunkSeq[u64], inout b: ChunkSeq[u64]) { }
fn f(inout s: ChunkSeq[u64]) { two(inout s, inout s); }",
            "E0301",
            "overlapping arguments",
        ),
        // An `in` loan that is live during the call may not overlap.
        (
            "fn rd(inout b: ChunkSeq[u64], in a: ChunkSeq[u64]) { }
fn f(inout s: ChunkSeq[u64]) { rd(inout s, in s); }",
            "E0301",
            "overlapping arguments",
        ),
        // Copy aggregates may be lent by address, so they stay refused.
        (
            "copy struct P { x: u64, y: u64 }
struct Q { p: P, n: u64 }
fn g(inout q: Q, p: P) { q.p.x = 1; }
fn f(inout q: Q) { g(inout q, q.p); }",
            "E0301",
            "overlapping arguments",
        ),
        (
            "struct C { x: u64 }
fn set(inout c: C) -> u64 { c.x = 0; 1 }
fn f(inout c: C, v: u64) { c.x = v; }
fn h(inout c: C) { f(inout c, set(inout c)); }",
            "E0301",
            "conflicts with active argument loan",
        ),
    ] {
        let c = checked(&format!("{body}\nfn main() -> u64 {{ 0 }}"));
        assert!(
            c.diagnostics
                .iter()
                .any(|d| d.code == code && d.message.contains(needle)),
            "{body}: expected {code} ({needle}): {:#?}",
            c.diagnostics
        );
    }
}

/// The Place IR check agrees: a nested write of a reserved argument is
/// refused there too, and the accepted programs pass it.
#[test]
fn place_ir_check_agrees() {
    let accepted_src = "struct C { x: u64, y: u64 }
fn g(in c: C) -> u64 { c.x }
fn f(inout c: C, v: u64) { c.x = v; }
fn h(inout c: C) { f(inout c, g(in c)); f(inout c, c.y); }
fn main() -> u64 { 0 }
";
    let c = accepted(accepted_src);
    let program = vesper::ir::lower_world(&c.world);
    let d = vesper::ir_check::validate(&c.world, &program);
    assert!(d.is_empty(), "{d:#?}");
}
