//! Empty sequences without a header (port speed lane 1, #1021): `new` and
//! `with_capacity(0)` share one static header and allocate nothing; the
//! first growth makes a real header, which generated code writes back to
//! the sequence's place (a local, a field path, an `inout` parameter of a
//! function, closure or inlined call, handler state, an export ABI handle).
//!
//! Every program runs in the evaluator and the Cranelift JIT (optimizer on
//! and off): the JIT must free every allocation itself and never free one
//! that is not live. Where a C compiler links, each program also runs as an
//! executable, and C-ABI libraries check the live allocation counter.
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
            "vesper-empty-seq-test-{}-{}",
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

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
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

fn vesper(args: &[&std::ffi::OsStr]) {
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "vesper {args:?} failed\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Runs `main` of `source` in the evaluator, the JIT and an executable and
/// returns its value. The JIT must leave nothing for teardown and drop
/// nothing twice.
fn differential(name: &str, source: &str) -> u64 {
    differential_with(name, source, true)
}

/// [`differential`], optionally without the JIT teardown check (for a drop
/// gap that main has too).
fn differential_with(name: &str, source: &str, no_teardown: bool) -> u64 {
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
        assert_eq!(jit, evaluated, "{name}: JIT (optimize {optimize})");
        assert_eq!(
            if no_teardown {
                native_runtime::teardown_frees()
            } else {
                0
            },
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
        vesper(&[
            "build".as_ref(),
            file.as_os_str(),
            "-o".as_ref(),
            exe.as_os_str(),
        ]);
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

const SUM: &str = r#"
fn u(x: u64) -> u64 { x }
fn sum(in s: ChunkSeq[u64]) -> u64 {
    let mut total: u64 = s.len();
    for i in u(0)..s.len() {
        total = match s.try_get(i) {
            Option::Some(v) => wrapping_add(total, v),
            Option::None => wrapping_add(total, 1000),
        };
    }
    total
}
"#;

/// Every operation that reads or writes an empty sequence without growing
/// it, then growth from the empty header through each kind of place.
const PLACES: &str = r#"
struct P { xs: ChunkSeq[u64], n: u64 }
struct Q { p: P, ys: ChunkSeq[u64] }
fn add(inout s: ChunkSeq[u64], n: u64) {
    for i in u(0)..n { s.push(wrapping_add(i, 7)); }
}
fn add_p(inout p: P, n: u64) {
    for i in u(0)..n { p.xs.push(i); }
    p.n = wrapping_add(p.n, n);
}
fn one(inout s: ChunkSeq[u64]) { s.push(42); }
fn view_len(in s: Slice[u64]) -> u64 { s.len() }
fn main() -> u64 {
    let mut a: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let mut total: u64 = sum(in a);
    a.clear();
    a.truncate(3);
    a.reserve(0);
    let none: ChunkSeq[u64] = ChunkSeq::new[u64]();
    a.extend(in none.as_slice());
    total = wrapping_add(total, sum(in a));
    total = wrapping_add(total, match a.pop() { Option::Some(v) => v, Option::None => 3 });
    total = wrapping_add(total, match a.try_get(0) { Option::Some(v) => v, Option::None => 5 });
    total = wrapping_add(total, view_len(in a.as_slice()));
    add(inout a, 3);
    one(inout a);
    total = wrapping_add(total, sum(in a));
    let mut q = Q { p: P { xs: ChunkSeq::new[u64](), n: 0 }, ys: ChunkSeq::with_capacity[u64](0) };
    q.p.xs.push(5);
    q.ys.push(9);
    add_p(inout q.p, 10);
    add(inout q.ys, 6);
    one(inout q.p.xs);
    total = wrapping_add(total, sum(in q.p.xs));
    total = wrapping_add(total, sum(in q.ys));
    total = wrapping_add(total, q.p.n);
    let mut b: ChunkSeq[u64] = ChunkSeq::new[u64]();
    if total > 5 { b.push(1); }
    for i in u(0)..u(20) { if i > 10 { b.push(i); } }
    total = wrapping_add(total, sum(in b));
    let mut r: ChunkSeq[u64] = ChunkSeq::new[u64]();
    r.reserve(9);
    r.push(2);
    r.extend(in b.as_slice());
    total = wrapping_add(total, sum(in r));
    let mut c: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let mut d: ChunkSeq[u64] = ChunkSeq::new[u64]();
    d.push(8);
    swap(inout c, inout d);
    d.push(6);
    c.push(7);
    let old = replace(inout c, ChunkSeq::new[u64]());
    c.push(1);
    total = wrapping_add(total, wrapping_add(sum(in c), wrapping_add(sum(in d), sum(in old))));
    total & 255
}
"#;

#[test]
fn empty_sequences_grow_through_every_place() {
    differential("places", &format!("{SUM}{PLACES}"));
}

/// Closure `inout` parameters, sequences of sequences, and
/// byte sequences (`extend_ascii_lowercase`, bytes pushed from empty).
const NESTED: &str = r#"
fn main() -> u64 {
    let mut total: u64 = 0;
    let f = |inout s: ChunkSeq[u64], v: u64| s.push(v);
    let mut a: ChunkSeq[u64] = ChunkSeq::new[u64]();
    f(inout a, 4);
    f(inout a, 5);
    total = wrapping_add(total, sum(in a));
    let mut outer: ChunkSeq[ChunkSeq[u64]] = ChunkSeq::new[ChunkSeq[u64]]();
    outer.push(ChunkSeq::new[u64]());
    let mut c: ChunkSeq[u64] = ChunkSeq::new[u64]();
    c.push(11);
    outer.push(c);
    outer.push(ChunkSeq::with_capacity[u64](0));
    for i in u(0)..u(4) {
        total = match outer.pop() {
            Option::Some(s) => wrapping_add(total, wrapping_add(i, sum(in s))),
            Option::None => wrapping_add(total, 100),
        };
    }
    let mut bytes: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let mut lower: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in u(0)..u(40) { bytes.push(wrapping_u8(wrapping_add(i, 65))); }
    lower.extend_ascii_lowercase(in bytes.as_slice());
    let mut again: ChunkSeq[u8] = ChunkSeq::new[u8]();
    again.extend_ascii_lowercase(in lower[0..0]);
    for i in u(0)..lower.len() {
        total = match lower.try_get(i) {
            Option::Some(v) => wrapping_add(total, widen_u64(v)),
            Option::None => total,
        };
    }
    total = wrapping_add(total, again.len());
    total & 255
}
"#;

#[test]
fn empty_sequences_grow_in_closures_and_nests() {
    differential("nested", &format!("{SUM}{NESTED}"));
}

/// Handler state that starts empty and grows in a clause. The final state
/// is not dropped (a drop gap main has too, with the header it allocated),
/// so only the values are compared.
const HANDLER: &str = r#"
linear effect Log { fn log(v: u64) -> u64; }
fn logs() -> u64 eff [Log] { wrapping_add(Log::log(3), Log::log(4)) }
fn main() -> u64 {
    let got = handle Log state s = ChunkSeq::new[u64]() in { logs() } with {
        return (x, s) => wrapping_add(x, sum(in s)),
        log(v, s, k) => { let mut t = s; t.push(v); k(v, t) },
    };
    got & 255
}
"#;

#[test]
fn empty_handler_state_grows_in_a_clause() {
    differential_with("handler", &format!("{SUM}{HANDLER}"), false);
}

/// Many sequences that are never pushed, and many pushed once, in a loop:
/// the JIT's live counter returns to its start without any teardown.
const CHURN: &str = r#"
struct Row { keys: ChunkSeq[u64], values: ChunkSeq[u64], tags: ChunkSeq[u8] }
fn row(n: u64) -> Row {
    let mut r = Row { keys: ChunkSeq::new[u64](), values: ChunkSeq::new[u64](), tags: ChunkSeq::new[u8]() };
    if n % 3 == 0 { r.keys.push(n); }
    if n % 5 == 0 { r.tags.push(1); }
    r
}
fn main() -> u64 {
    let mut total: u64 = 0;
    for n in u(0)..u(500) {
        let r = row(n);
        total = wrapping_add(total, wrapping_add(sum(in r.keys), wrapping_add(r.values.len(), r.tags.len())));
    }
    total & 255
}
"#;

#[test]
fn never_pushed_sequences_churn_without_leaks() {
    differential("churn", &format!("{SUM}{CHURN}"));
}
