//! The native sequence runtime (port gaps lane 1, #984): one allocation for
//! a header and its first elements, growth out of the inline room, separate
//! buffers for large capacities, `reserve`, strings, and sequences moved
//! into a handle and dropped.
//!
//! Every program runs in the evaluator and the Cranelift JIT (optimizer on
//! and off): the JIT must free every allocation itself and never free one
//! that is not live. Where a C compiler links, each program also runs as an
//! executable, and as a C-ABI library whose live allocation counter must
//! return to its start and count a header with inline elements once.
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
            "vesper-seq-runtime-test-{}-{}",
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

/// Growth from the inline room, from no room, and of a separate buffer;
/// every element survives each move.
const GROWTH: &str = r#"
fn u(x: u64) -> u64 { x }
copy struct W { a: u64, b: u64, c: u64, d: u64, e: u64, f: u64 }
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
fn fill(n: u64) -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..n { s.push(wrapping_mul(i, 3)); }
    sum(in s)
}
fn bytes(n: u64) -> u64 {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in u(0)..n { s.push(wrapping_u8(i)); }
    let mut total: u64 = s.len();
    for i in u(0)..s.len() {
        total = match s.try_get(i) {
            Option::Some(v) => wrapping_add(total, widen_u64(v)),
            Option::None => wrapping_add(total, 1000),
        };
    }
    total
}
fn wide(n: u64) -> u64 {
    // 48-byte elements: `new` has no inline room for them.
    let mut s: ChunkSeq[W] = ChunkSeq::new[W]();
    for i in u(0)..n { s.push(W { a: i, b: i, c: i, d: i, e: i, f: wrapping_add(i, 1) }); }
    let mut total: u64 = s.len();
    for i in u(0)..s.len() {
        total = match s.try_get(i) {
            Option::Some(w) => wrapping_add(total, wrapping_add(w.a, w.f)),
            Option::None => wrapping_add(total, 1000),
        };
    }
    total
}
fn sized(cap: u64, n: u64) -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::with_capacity[u64](cap);
    for i in u(0)..n { s.push(wrapping_add(i, 1)); }
    sum(in s)
}
fn main() -> u64 {
    let mut total: u64 = 0;
    for n in u(0)..u(40) {
        total = wrapping_add(total, fill(n));
        total = wrapping_add(total, bytes(wrapping_mul(n, 3)));
        total = wrapping_add(total, wide(n));
    }
    total = wrapping_add(total, sized(0, 5));
    total = wrapping_add(total, sized(2, 2));
    total = wrapping_add(total, sized(2, 9));
    total = wrapping_add(total, sized(64, 64));
    total = wrapping_add(total, sized(64, 65));
    total = wrapping_add(total, sized(65, 300));
    total = wrapping_add(total, sized(1000, 10));
    total = wrapping_add(total, sized(10000, 3));
    total = wrapping_add(total, sized(10000, 10001));
    total & 255
}
"#;

#[test]
fn growth_keeps_every_element_and_frees_once() {
    differential("growth", GROWTH);
}

const RESERVE: &str = r#"
fn u(x: u64) -> u64 { x }
fn reserved(first: u64, more: u64, n: u64) -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.reserve(first);
    for i in u(0)..n { s.push(i); }
    s.reserve(more);
    s.reserve(0);
    for i in u(0)..n { s.push(i); }
    let mut total: u64 = s.len();
    for i in u(0)..s.len() {
        total = match s.try_get(i) {
            Option::Some(v) => wrapping_add(total, v),
            Option::None => wrapping_add(total, 1000),
        };
    }
    total
}
fn main() -> u64 {
    let mut total: u64 = 0;
    for n in u(0)..u(20) {
        total = wrapping_add(total, reserved(n, wrapping_mul(n, 7), n));
        total = wrapping_add(total, reserved(100, 0, n));
    }
    total & 255
}
"#;

#[test]
fn reserve_is_a_capacity_hint() {
    differential("reserve", RESERVE);
}

#[test]
fn reserve_takes_one_u64_on_a_sequence() {
    for (source, code) in [
        (
            "fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); s.reserve(); s.len() }",
            "E0207",
        ),
        (
            "fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); s.reserve(1, 2); s.len() }",
            "E0207",
        ),
    ] {
        let p = parser::parse(source);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        let c = check::check(&p.module);
        assert!(
            c.diagnostics.iter().any(|d| d.code == code),
            "{source}: {:#?}",
            c.diagnostics
        );
    }
}

/// Strings grow through the same runtime; their buffers are always
/// separate, and literal bytes are copied before the first write.
const STRINGS: &str = r#"
fn u(x: u64) -> u64 { x }
fn text(n: u64) -> u64 {
    let mut s = String::new();
    s.push_str(in "seed");
    for i in u(0)..n { s.push_str(in "ab"); }
    s.reserve(n);
    let mut t = String::new();
    t.push_str(in s.as_str());
    wrapping_add(t.len(), s.len())
}
fn literal(n: u64) -> u64 {
    let mut s = "a literal";
    for i in u(0)..n { s.push_char(66); }
    s.len()
}
fn main() -> u64 {
    let mut total: u64 = 0;
    for n in u(0)..u(30) {
        total = wrapping_add(total, text(n));
        total = wrapping_add(total, literal(n));
    }
    total & 255
}
"#;

#[test]
fn strings_grow_and_free_through_the_runtime() {
    differential("strings", STRINGS);
}
