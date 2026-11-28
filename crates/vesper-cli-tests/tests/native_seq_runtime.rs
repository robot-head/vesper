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
mod support;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

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

fn links() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn vesper(args: &[&std::ffi::OsStr]) {
    let out = support::vesper().args(args).output().unwrap();
    assert!(
        out.status.success(),
        "vesper {args:?} failed\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Builds `source` as a C-ABI library and runs the C `body` (which may use
/// `live()`, the library's live allocation count, and `check(cond)`) in a
/// driver. Returns the driver's standard output, or `None` without a C
/// compiler.
fn library(name: &str, source: &str, declarations: &str, body: &str) -> Option<String> {
    if !links() {
        return None;
    }
    let temp = Temp::new();
    let file = temp.0.join(format!("{name}.ves"));
    // A sequence parameter makes the library use export ABI v2, which
    // exports the live allocation counter.
    fs::write(
        &file,
        format!("{source}\npub fn probe(in s: ChunkSeq[u64]) -> u64 {{ s.len() }}\n"),
    )
    .unwrap();
    let object = temp.0.join(format!("{name}.o"));
    vesper(&[
        "build".as_ref(),
        "--lib".as_ref(),
        file.as_os_str(),
        "-o".as_ref(),
        object.as_os_str(),
    ]);
    let driver = format!(
        "#include <stdio.h>\n#include <stdint.h>\n#include <stdlib.h>\n\
         int64_t vesper_{name}_live_allocations(void);\n\
         static int64_t live(void) {{ return vesper_{name}_live_allocations(); }}\n\
         #define check(c) do {{ if (!(c)) {{ printf(\"failed: %s (live %lld)\\n\", #c, (long long)live()); return 1; }} }} while (0)\n\
         {declarations}\nint main(void) {{\n{body}\n  printf(\"ok\\n\");\n  return 0;\n}}\n"
    );
    let c = temp.0.join("driver.c");
    fs::write(&c, driver).unwrap();
    let exe = temp.0.join("driver");
    let cc = Command::new("cc")
        .arg(&c)
        .arg(&object)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        cc.status.success(),
        "{name}: cc failed: {}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let out = Command::new(&exe).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success() && text.contains("ok"),
        "{name}: driver failed: {text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(text)
}

/// Sequences moved into a struct and out of a function: the library's live
/// allocation counter sees none for an empty sequence (#1021), one per
/// sequence with inline room, two once a sequence has grown out of it, and
/// none after the drop.
const HANDLES: &str = r#"
pub struct S { a: ChunkSeq[u64], b: ChunkSeq[u64], c: ChunkSeq[u64] }
fn u(x: u64) -> u64 { x }
pub fn make() -> S {
    S { a: ChunkSeq::new[u64](), b: ChunkSeq::new[u64](), c: ChunkSeq::new[u64]() }
}
pub fn make_grown(n: u64) -> S {
    let mut a: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..n { a.push(i); }
    let mut b: ChunkSeq[u64] = ChunkSeq::with_capacity[u64](n);
    for i in u(0)..n { b.push(i); }
    S { a: a, b: b, c: ChunkSeq::with_capacity[u64](10000) }
}
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
pub fn total(in s: S) -> u64 {
    wrapping_add(wrapping_add(sum(in s.a), sum(in s.b)), s.c.len())
}
pub fn churn(n: u64) -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..n { s.push(i); }
    s.reserve(n);
    s.len()
}
"#;

#[test]
fn library_counts_and_frees_every_allocation() {
    let declarations = "typedef struct { uint64_t w[3]; } S;\n\
        void vesper_handles_make(S *out);\n\
        void vesper_handles_make_grown(S *out, uint64_t n);\n\
        uint64_t vesper_handles_total(const S *s);\n\
        void vesper_handles_S_drop(S *s);\n\
        uint64_t vesper_handles_churn(uint64_t n);";
    let body = r#"
  int64_t start = live();
  S s;
  vesper_handles_make(&s);
  check(live() == start);
  check(vesper_handles_total(&s) == 0);
  vesper_handles_S_drop(&s);
  check(live() == start);
  /* a: 4 inline elements; b: 4 inline (with_capacity(4)); c: separate. */
  vesper_handles_make_grown(&s, 4);
  check(live() == start + 4);
  check(vesper_handles_total(&s) == 8 + 6 + 6);
  vesper_handles_S_drop(&s);
  check(live() == start);
  /* a grew out of its inline room (2); b's 50 elements fit inline (1); c (2). */
  vesper_handles_make_grown(&s, 50);
  check(live() == start + 5);
  vesper_handles_S_drop(&s);
  /* b's 800 bytes still fit inline; c's 80000 bytes do not. */
  vesper_handles_make_grown(&s, 100);
  check(live() == start + 5);
  vesper_handles_S_drop(&s);
  check(live() == start);
  for (uint64_t n = 0; n < 200; n++) check(vesper_handles_churn(n) == n);
  check(live() == start);
"#;
    library("handles", HANDLES, declarations, body);
}
