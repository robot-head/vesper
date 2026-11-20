//! `String::push_str` copies its source with one reserve and one block copy
//! (self-hosting SH1 lane 4, #1147). Before this lane native code appended
//! byte by byte (self-host toolchain pilots gap 4); runs of at least
//! `SHORT_COPY` bytes now call `memmove` where the C library is available.
//! The evaluator, both JIT configurations and an executable agree on every
//! source shape: literals, whole strings, views, empty and long runs, and
//! growth past several reallocations.
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
            "vesper-string-copy-{}-{}",
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

/// Runs `main` in the evaluator, the JIT (optimizer on and off) and, where
/// a C compiler links, an executable; all must agree, and the JIT must free
/// every sequence it made.
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
        assert_eq!(native_runtime::stray_frees(), stray, "{name}: stray free");
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

/// FNV-1a over the bytes, plus the length, folded to the low byte for the
/// executable's status.
const DIGEST: &str = "fn digest(in s: Str) -> u64 {
    let mut h: u64 = 14695981039346656037;
    let n = s.len();
    for i in 0..n {
        match s.try_byte_at(i) {
            Option::Some(b) => {
                h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
            }
            Option::None => {}
        }
    }
    wrapping_add(h, n)
}
fn fold(h: u64) -> u64 {
    let a = h ^ (h >> 32);
    let b = a ^ (a >> 16);
    (b ^ (b >> 8)) & 255
}
";

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(1099511628211);
    }
    h.wrapping_add(bytes.len() as u64)
}

fn fold(h: u64) -> u64 {
    let a = h ^ (h >> 32);
    let b = a ^ (a >> 16);
    (b ^ (b >> 8)) & 255
}

#[test]
fn every_source_shape_agrees_on_every_backend() {
    let source = format!(
        "{DIGEST}fn grow(in t: Str, rounds: u64) -> String {{
    let mut s = String::new();
    for i in 0..rounds {{
        let k = i % 37;
        s.push_str(in t[0..k]);
        s.push_str(in \"\");
        s.push_str(in \"|\");
    }}
    s
}}
fn main() -> u64 {{
    let t = \"0123456789abcdefghijklmnopqrstuvwxyz\u{e9}\";
    let mut s = String::new();
    s.push_str(in \"\");
    s.push_str(in \"ab\");
    s.push_str(in \"h\u{e9}llo w\u{f6}rld, a literal longer than sixteen bytes\");
    s.push_str(in t);
    s.push_str(in t[3..21]);
    let doubled = s.concat(in t[0..2]);
    s.push_str(in doubled);
    let big = grow(in t, 300);
    s.push_str(in big);
    fold(digest(in s))
}}"
    );
    // The same text in Rust.
    let t = "0123456789abcdefghijklmnopqrstuvwxyz\u{e9}";
    let mut s = String::new();
    s.push_str("ab");
    s.push_str("h\u{e9}llo w\u{f6}rld, a literal longer than sixteen bytes");
    s.push_str(t);
    s.push_str(&t[3..21]);
    let copy = format!("{s}{}", &t[0..2]);
    s.push_str(&copy);
    let mut big = String::new();
    for i in 0..300 {
        big.push_str(&t[0..i % 37]);
        big.push('|');
    }
    s.push_str(&big);
    assert_eq!(differential("shapes", &source), fold(fnv(s.as_bytes())));
}

/// A returned string built from many appends crosses the JIT boundary
/// with the right bytes (not just the right digest).
#[test]
fn appended_strings_return_whole() {
    let source = "fn main() -> String {
    let mut s = String::with_capacity(4);
    for i in 0..100 {
        s.push_str(in \"abcdefghijklmnopqrstuvwxyz\");
        s.push_str(in \"-\");
    }
    s
}";
    let c = accepted(source);
    let want: String = std::iter::repeat_n("abcdefghijklmnopqrstuvwxyz-", 100).collect();
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(jit, evaluated, "optimize {optimize}");
    }
    assert!(evaluated.contains(&want), "{evaluated}");
}

/// The emitted code of `append` calls `memmove` for the long run instead of
/// only walking the bytes, with and without the optimizer.
#[test]
fn push_str_lowers_to_a_block_copy() {
    let source = "fn append(inout s: String, in t: Str) {
    s.push_str(in t);
}
fn main() -> u64 {
    let mut s = String::new();
    append(inout s, in \"a literal longer than the short-copy cutoff\");
    s.len()
}";
    let c = accepted(source);
    for optimize in [true, false] {
        let mut clif = String::new();
        let mut observe = |f: native::EmittedFunction| {
            if f.source == "append" {
                clif = f.function.display().to_string();
            }
        };
        native::object_with_observer(
            &c.world,
            "main",
            Some("x86_64-unknown-linux-gnu"),
            optimize,
            Some(&mut observe),
        )
        .unwrap();
        assert!(
            !clif.is_empty(),
            "optimize {optimize}: append was not emitted"
        );
        assert!(
            clif.to_ascii_lowercase().contains("memmove"),
            "optimize {optimize}: push_str does not call memmove\n{clif}"
        );
    }
}
