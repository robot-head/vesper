//! Bound views and view receivers (§13, P3b gaps lane 9, #1094): an
//! immutable `let` may bind a string or slice view (`s.as_bytes()`,
//! `t[a..b]`, `q.as_slice()`) of an `in` parameter or of another bound view,
//! and a slice view may be the receiver of a reading `Slice` method
//! (`b[a..c].find_byte(x)`). Views stay second-class: they cannot be
//! returned, stored, moved or mutated, and views of places the function may
//! change stay refused. The evaluator, both JIT configurations and an
//! executable agree.
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
            "vesper-bound-views-{}-{}",
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
fn views_bind_to_immutable_locals() {
    let source = "fn sum(in b: Slice[u8]) -> u64 {
    let mut t: u64 = 0;
    for i in 0..b.len() {
        t = wrapping_add(t, match b.try_get(i) { Option::Some(x) => widen_u64(x), Option::None => 0 });
    }
    t
}
fn strs(in s: String, in t: Str) -> u64 {
    let a = s.as_bytes();
    let b = t[1..3];
    let c = b.as_bytes();
    let d = a[0..2];
    let e = s.as_str();
    wrapping_add(
        wrapping_add(sum(in a), wrapping_mul(sum(in c), 1000)),
        wrapping_mul(wrapping_add(wrapping_add(d.len(), b.len()), e.len()), 1000000),
    )
}
fn seqs(in s: ChunkSeq[u8]) -> u64 {
    let v = s.as_slice();
    let w = v[1..3];
    wrapping_add(sum(in w), wrapping_mul(w.len(), 100))
}
fn n(in s: Str) -> u64 {
    let b = s.as_bytes();
    wrapping_add(b.len(), match b.try_get(0) { Option::Some(x) => widen_u64(x), Option::None => 0 })
}
fn main() -> u64 {
    let mut q: ChunkSeq[u8] = ChunkSeq::new[u8]();
    q.push(5);
    q.push(6);
    q.push(7);
    let mut s = String::new();
    s.push_str(in \"hi\");
    let r = wrapping_add(strs(in s, in \"wxyz\"), wrapping_mul(seqs(in q), 1000000000));
    wrapping_add(r, wrapping_mul(n(in \"abc\"), 1000000000000))
}
";
    // strs: 104 + 105, 120 + 121, lengths 2 + 2 + 2; seqs: 6 + 7, 2.
    let expected = 209 + 241_000 + 6_000_000 + 213 * 1_000_000_000 + 100 * 1_000_000_000_000;
    assert_eq!(differential("bound", source), expected);
}

/// port-p3-aegis.md gap 6: slice views as receivers of reading methods.
#[test]
fn slice_views_are_method_receivers() {
    let source = "fn n(in b: Slice[u8]) -> u64 {
    let a = b[1..4].len();
    let c = match b[1..4].find_byte(3) { Option::Some(i) => i, Option::None => 99 };
    let d = match b[0..4].find_not_in(14, 0, false) { Option::Some(i) => i, Option::None => 77 };
    let e = match b[2..4].try_get(1) { Option::Some(x) => widen_u64(x), Option::None => 0 };
    wrapping_add(wrapping_add(wrapping_mul(a, 1000), wrapping_mul(c, 100)), wrapping_add(wrapping_mul(d, 10), e))
}
fn m(in s: ChunkSeq[u8]) -> u64 { wrapping_add(s[1..3].len(), s.as_slice().len()) }
fn main() -> u64 {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    s.push(1);
    s.push(2);
    s.push(3);
    s.push(4);
    wrapping_add(n(in s[0..4]), wrapping_mul(m(in s), 10000))
}
";
    // find_not_in with the class {1, 2, 3} (mask 14) stops at byte 4.
    assert_eq!(
        differential("receivers", source),
        3000 + 100 + 30 + 4 + 60_000
    );
}

#[test]
fn views_of_changing_places_stay_refused() {
    for (body, code, needle) in [
        (
            "fn f() -> u64 { let mut s = String::new(); let b = s.as_bytes(); s.push_str(in \"x\"); b.len() }",
            "E1531",
            "second-class",
        ),
        (
            "fn f(inout s: String) -> u64 { let b = s.as_bytes(); s.push_str(in \"x\"); b.len() }",
            "E1531",
            "second-class",
        ),
        (
            "fn f(in s: Str) -> u64 { let mut b = s.as_bytes(); b.len() }",
            "E1531",
            "second-class",
        ),
        (
            "fn f(sink s: ChunkSeq[u8]) -> u64 { let b = s[0..1]; b.len() }",
            "E1521",
            "second-class",
        ),
        (
            "fn f(inout s: ChunkSeq[u8]) -> u64 { let b = s.as_slice(); b.len() }",
            "E1521",
            "second-class",
        ),
        (
            "fn f(in s: Str) -> Slice[u8] { let b = s.as_bytes(); b }",
            "E1520",
            "result",
        ),
        (
            "fn take(sink b: Slice[u8]) -> u64 { 0 }\nfn f(in s: Str) -> u64 { let b = s.as_bytes(); take(b) }",
            "E1520",
            "sink",
        ),
        // A bound view shares its source: it cannot be moved into a
        // mutable binding or written.
        (
            "fn f(in s: String) { let b = s.as_bytes(); let mut c = b; c.fill(255); }",
            "E0305",
            "",
        ),
        (
            "fn f(in s: String) { let b = s.as_bytes(); b.fill(255); }",
            "E0307",
            "shared",
        ),
        // A view receiver's source stays borrowed through the arguments.
        (
            "fn f(sink s: ChunkSeq[u8]) -> bool { match s[0..1].find_byte({ ChunkSeq::finish(s); 0 }) { Option::Some(_) => true, Option::None => false } }",
            "E0301",
            "conflicts with active argument loan",
        ),
        (
            "fn f(inout o: Slice[u8]) { o[0..2].fill(1); }",
            "E1521",
            "updates its receiver",
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
