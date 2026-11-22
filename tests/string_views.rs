//! Bound string and sequence views of immutable owned bindings (SH3 lane 4,
//! #1243; src/strings/check.rs `bind_view`): `let v = s.as_bytes();` for a
//! `let` without `mut` holding a `String` or `ChunkSeq`, which cannot be
//! moved while the view is in scope. The evaluator, both JIT configurations
//! and an executable agree, and the JIT frees every sequence exactly once.
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
            "vesper-string-views-{}-{}",
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
fn views_of_immutable_owned_locals() {
    let source = r#"fn count(in s: Str) -> u64 { s.len() }
fn sum(in b: Slice[u64]) -> u64 {
    let mut t: u64 = 0;
    for x in b { t = wrapping_add(t, x); }
    t
}
fn main() -> u64 {
    let mut o = String::new();
    o.push_str("ab::cd");
    let s = o;
    let v = s.as_bytes();
    let w = s.as_str();
    let t = s[0..2];
    let mut n = wrapping_add(v.len(), count(w));
    n = wrapping_add(n, wrapping_mul(t.len(), 100));
    if t == "ab" { n = wrapping_add(n, 1000); }
    let mut q: ChunkSeq[u64] = ChunkSeq::new();
    q.push(5);
    q.push(6);
    let xs = q;
    let all = xs.as_slice();
    let tail = xs[1..2];
    n = wrapping_add(n, wrapping_mul(sum(all), 10000));
    wrapping_add(n, wrapping_mul(sum(tail), 1000000))
}
"#;
    assert_eq!(
        differential("locals", source),
        12 + 200 + 1000 + 110_000 + 6_000_000
    );
}

#[test]
fn the_last_segment_of_a_name_is_a_view() {
    // The last `::` segment of a name, found and compared in place.
    let source = r#"fn last_segment_start(in b: Slice[u8]) -> u64 {
    let mut at: u64 = 0;
    for i in 1..b.len() {
        if b.try_get(i) == Option::Some(58) && b.try_get(wrapping_sub(i, 1)) == Option::Some(58) {
            at = wrapping_add(i, 1);
        }
    }
    at
}
fn main() -> u64 {
    let mut o = String::new();
    o.push_str("std::seq::push");
    let name = o;
    let at = last_segment_start(name.as_bytes());
    let seg = name[at..name.len()];
    let mut n: u64 = at;
    if seg == "push" { n = wrapping_add(n, 100); }
    for s in ["pop", "push"] {
        if seg == s { n = wrapping_add(n, 1000); }
    }
    n
}
"#;
    assert_eq!(differential("segment", source), 10 + 100 + 1000);
}

#[test]
fn the_source_moves_after_the_view_scope() {
    let source = r#"fn consume(sink s: String) -> u64 { s.len() }
fn main() -> u64 {
    let mut o = String::new();
    o.push_str("abc");
    let s = o;
    let n = { let v = s.as_bytes(); v.len() };
    wrapping_add(n, consume(s))
}
"#;
    assert_eq!(differential("after", source), 6);
}

#[test]
fn views_of_changing_or_moved_sources_are_refused() {
    for (body, code, needle) in [
        (
            "fn take(sink s: String) -> u64 { s.len() }\nfn f() -> u64 { let mut o = String::new(); o.push_str(\"ab\"); let s = o; let v = s.as_bytes(); let k = take(s); wrapping_add(k, v.len()) }",
            "E1531",
            "while the view 'v'",
        ),
        (
            "fn take(sink s: ChunkSeq[u8]) -> u64 { s.len() }\nfn f() -> u64 { let q: ChunkSeq[u8] = ChunkSeq::new(); let v = q[0..0]; let k = take(q); wrapping_add(k, v.len()) }",
            "E1531",
            "while the view 'v'",
        ),
        // A shadow in a nested block does not end the outer view's loan.
        (
            "fn take(sink s: String) -> u64 { s.len() }\nfn f() -> u64 { let s = String::new(); let v = s.as_bytes(); { let v = 0; take(s); }; v.len() }",
            "E1531",
            "while the view 'v'",
        ),
        (
            "fn take(sink s: String) -> u64 { s.len() }\nfn f() -> u64 { let s = String::new(); let v = s.as_bytes(); let mut n = 0; for v in 0..1 { n = take(s); } wrapping_add(n, v.len()) }",
            "E1531",
            "while the view 'v'",
        ),
        (
            "fn f() -> u64 { let mut s = String::new(); let v = s.as_bytes(); v.len() }",
            "E1531",
            "second-class",
        ),
        (
            "fn f() -> u64 { let s = String::new(); let v = s.as_bytes(); s.push_str(\"x\"); v.len() }",
            "E0313",
            "",
        ),
        (
            "fn f() -> Str { let s = String::new(); let v = s.as_str(); v }",
            "E1530",
            "",
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
