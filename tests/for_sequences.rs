//! `for` over fixed arrays, string-literal arrays, `ChunkSeq`s, slices and
//! views (SH3 lane 4, #1243; src/loops/for_sequences.rs). The checker
//! rewrites each loop to an index loop over a range, so the evaluator, both
//! JIT configurations and an executable agree; the body may read but not
//! change what it iterates.
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
            "vesper-for-sequences-{}-{}",
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

#[test]
fn string_literal_arrays() {
    let source = r#"fn count(in s: Str) -> u64 { s.len() }
fn main() -> u64 {
    let mut n: u64 = 0;
    for s in ["a", "bc", "def"] {
        n = wrapping_add(n, count(s));
    }
    let mut first: u64 = 0;
    for s in ["skip", "xy", "stop", "never"] {
        if s == "skip" { continue; }
        if s == "stop" { break; }
        first = wrapping_add(first, wrapping_mul(s.len(), 100));
    }
    wrapping_add(n, first)
}
"#;
    assert_eq!(differential("literals", source), 6 + 200);
}

#[test]
fn sequences_slices_and_views() {
    let source = r#"fn sum(in s: Slice[u64]) -> u64 {
    let mut t: u64 = 0;
    for x in s { t = wrapping_add(t, x); }
    t
}
fn make(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new();
    for i in 0..n { s.push(wrapping_mul(i, 1000)); }
    s
}
fn main() -> u64 {
    let mut xs: ChunkSeq[u64] = ChunkSeq::new();
    xs.push(1);
    xs.push(2);
    xs.push(3);
    let mut n: u64 = 0;
    for x in xs {
        for y in xs { n = wrapping_add(n, wrapping_mul(x, y)); }
    }
    n = wrapping_add(n, sum(xs[1..3]));
    for x in xs[0..2] { n = wrapping_add(n, wrapping_mul(x, 10)); }
    for x in xs.as_slice() { n = wrapping_add(n, x); }
    for x in make(3) { n = wrapping_add(n, x); }
    xs.push(4);
    for x in xs {
        if x == 2 { continue; }
        n = wrapping_add(n, wrapping_mul(x, 100000));
    }
    n
}
"#;
    // 36 + 5 + 30 + 6 + 3000 + 800000
    assert_eq!(differential("sequences", source), 803_077);
}

#[test]
fn arrays_fields_and_patterns() {
    let source = r#"struct Bag { items: ChunkSeq[u64], pairs: [(u64, u64); 2] }
impl Bag {
    fn total(in self) -> u64 {
        let mut t: u64 = 0;
        for x in self.items { t = wrapping_add(t, x); }
        for (a, b) in self.pairs { t = wrapping_add(t, wrapping_mul(a, b)); }
        t
    }
}
fn main() -> u64 {
    let a: [u64; 3] = [100, 200, 300];
    let mut n: u64 = 0;
    for v in a { n = wrapping_add(n, v); }
    for v in [1, 2, 3, 4] { n = wrapping_add(n, v); }
    let mut items: ChunkSeq[u64] = ChunkSeq::new();
    items.push(7);
    let bag = Bag { items: items, pairs: [(2, 3), (4, 5)] };
    wrapping_add(n, bag.total())
}
"#;
    assert_eq!(differential("arrays", source), 600 + 10 + 7 + 26);
}

#[test]
fn string_bytes() {
    let source = r#"fn bytes(in s: Str) -> u64 {
    let mut t: u64 = 0;
    for b in s.as_bytes() { t = wrapping_add(t, widen_u64(b)); }
    let v = s.as_bytes();
    for b in v[1..v.len()] { t = wrapping_add(t, widen_u64(b)); }
    t
}
fn main() -> u64 {
    let mut o = String::new();
    o.push_str("ab");
    let s = o;
    let mut t: u64 = 0;
    for b in s.as_bytes() { t = wrapping_add(t, widen_u64(b)); }
    wrapping_add(t, bytes("ab"))
}
"#;
    assert_eq!(differential("bytes", source), 195 + 195 + 98);
}

#[test]
fn non_copy_elements_are_lent() {
    let source = r#"fn count(in s: String) -> u64 { s.len() }
struct W { names: ChunkSeq[String] }
fn total(in w: W) -> u64 {
    let mut n: u64 = 0;
    for s in w.names { n = wrapping_add(n, count(s)); }
    n
}
fn main() -> u64 {
    let mut q: ChunkSeq[String] = ChunkSeq::new();
    let mut a = String::new();
    a.push_str("ab");
    let mut b = String::new();
    b.push_str("cde");
    q.push(a);
    q.push(b);
    let mut n: u64 = 0;
    for s in q {
        n = wrapping_add(n, count(s));
        if s == "ab" { n = wrapping_add(n, 100); }
    }
    for _ in q { n = wrapping_add(n, 1000); }
    let q2 = q;
    for s in q2[1..2] { n = wrapping_add(n, wrapping_mul(count(s), 10000)); }
    let w = W { names: q2 };
    wrapping_add(n, total(w))
}
"#;
    assert_eq!(differential("non_copy", source), 32110);
}

#[test]
fn rejected_loops() {
    for (body, code, needle) in [
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); for x in xs { xs.push(x); } 0 }",
            "E0301",
            "conflicts with the 'for' loop",
        ),
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); for x in xs { xs = ChunkSeq::new(); } 0 }",
            "E0301",
            "conflicts with the 'for' loop",
        ),
        (
            "fn g(sink s: ChunkSeq[u64]) -> u64 { s.len() }\nfn f() -> u64 { let xs: ChunkSeq[u64] = ChunkSeq::new(); let mut n = 0; for x in xs { n = g(xs); } n }",
            "E0301",
            "conflicts with the 'for' loop",
        ),
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); for x in xs[0..1] { xs.push(1); } 0 }",
            "E0301",
            "conflicts with the 'for' loop",
        ),
        (
            "fn f() -> u64 { let mut ys: ChunkSeq[String] = ChunkSeq::new(); for y in ys[0..0] { } 0 }",
            "E0213",
            "non-copy",
        ),
        (
            "fn take(sink s: String) -> u64 { s.len() }\nfn f() -> u64 { let ys: ChunkSeq[String] = ChunkSeq::new(); let mut n = 0; for y in ys { n = take(y); } n }",
            "E0305",
            "",
        ),
        (
            "fn f() -> u64 { let ys: ChunkSeq[(String, u64)] = ChunkSeq::new(); for (y, n) in ys { } 0 }",
            "E0213",
            "non-copy",
        ),
        ("fn f() -> u64 { for z in 5 { } 0 }", "E0213", "iterates"),
        // Calls are not static-text literals: an array of `Str` results is
        // not iterable (its elements would not be evaluated before the loop).
        (
            "fn name() -> Str { \"a\" }\nfn f() -> u64 { for s in [name(), name()] { } 0 }",
            "E0213",
            "non-copy",
        ),
        (
            "fn f() -> u64 { let mut q: ChunkSeq[Option[u64]] = ChunkSeq::new(); q.push(Option::None); for Option::Some(x) in q { } 0 }",
            "E0210",
            "irrefutable",
        ),
        (
            "fn f() -> u64 { let a: [Option[u64]; 2] = [Option::None, Option::None]; for Option::Some(x) in a { } 0 }",
            "E0210",
            "irrefutable",
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
