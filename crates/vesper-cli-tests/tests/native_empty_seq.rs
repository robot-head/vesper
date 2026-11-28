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

/// A library whose empty sequences allocate nothing: in a struct, returned
/// by value to C, and grown through an `inout` handle, after which C holds
/// the new handle.
const LIBRARY: &str = r#"
pub struct S { a: ChunkSeq[u64], b: ChunkSeq[u64], c: ChunkSeq[u64] }
pub fn make() -> S {
    S { a: ChunkSeq::new[u64](), b: ChunkSeq::new[u64](), c: ChunkSeq::with_capacity[u64](0) }
}
pub fn empty() -> ChunkSeq[u64] { ChunkSeq::new[u64]() }
pub fn grow(inout s: ChunkSeq[u64], n: u64) { for i in u(0)..n { s.push(i); } }
pub fn grow_b(inout s: S, n: u64) { for i in u(0)..n { s.b.push(i); } }
pub fn total(in s: S) -> u64 { wrapping_add(wrapping_add(sum(in s.a), sum(in s.b)), s.c.len()) }
"#;

#[test]
fn library_empty_sequences_allocate_nothing() {
    let declarations = "typedef struct { uint64_t w[3]; } S;\n\
        void vesper_empty_make(S *out);\n\
        void vesper_empty_S_drop(S *s);\n\
        uint64_t vesper_empty_total(const S *s);\n\
        uint64_t vesper_empty_empty(void);\n\
        void vesper_empty_grow(uint64_t *s, uint64_t n);\n\
        void vesper_empty_grow_b(S *s, uint64_t n);\n\
        uint64_t vesper_empty_probe(uint64_t s);\n\
        int64_t vesper_empty_seq_free(int64_t h);";
    let body = r#"
  int64_t start = live();
  S s;
  vesper_empty_make(&s);
  check(live() == start);
  check(s.w[0] == s.w[1] && s.w[1] == s.w[2]);
  check(vesper_empty_total(&s) == 0);
  vesper_empty_S_drop(&s);
  check(live() == start);
  for (int i = 0; i < 100; i++) { vesper_empty_make(&s); vesper_empty_S_drop(&s); }
  check(live() == start);
  /* Growing a field from the empty header: one header with inline room. */
  vesper_empty_make(&s);
  uint64_t shared = s.w[1];
  vesper_empty_grow_b(&s, 3);
  check(s.w[1] != shared && s.w[0] == shared);
  check(live() == start + 1);
  check(vesper_empty_total(&s) == 3 + 3);
  vesper_empty_S_drop(&s);
  check(live() == start);
  /* A handle returned by value, then grown through `inout` from C. */
  uint64_t h = vesper_empty_empty();
  check(live() == start);
  check(((uint64_t *)h)[0] == 0 && vesper_empty_probe(h) == 0);
  vesper_empty_grow(&h, 0);
  check(h == shared);
  vesper_empty_grow(&h, 4);
  check(h != shared && vesper_empty_probe(h) == 4);
  check(live() == start + 1);
  vesper_empty_grow(&h, 1);
  check(vesper_empty_probe(h) == 5 && live() == start + 2);
  vesper_empty_seq_free(h);
  vesper_empty_seq_free(shared);
  check(live() == start);
"#;
    library("empty", &format!("{SUM}{LIBRARY}"), declarations, body);
}
