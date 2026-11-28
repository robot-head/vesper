//! Native drops of function-local and by-value `ChunkSeq[T]` and `String`
//! values (§13, Stage 1 perf lane 2, #932).
//!
//! Every program runs in the evaluator, the Cranelift JIT (optimizer on and
//! off) and, where a C compiler links, an executable and a C-ABI library.
//! The JIT must free every sequence itself (nothing is left for its
//! teardown) and never free one that is not live; the library's live
//! allocation counter must return to its start.
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
            "vesper-seq-drop-test-{}-{}",
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

/// Export ABI v2 by-value parameters: a sequence the caller makes with
/// `seq_new` and a string lent as borrowed bytes are owned by the callee,
/// which frees them (and any buffer it grows), so the live count returns to
/// its start.
#[test]
fn by_value_sequences_and_strings_cross_the_export_abi() {
    if !links() {
        return;
    }
    let temp = Temp::new();
    let file = temp.0.join("bv.ves");
    fs::write(
        &file,
        "pub fn take(sink s: ChunkSeq[u64]) -> u64 { let mut t = s; t.push(9); t.len() }\n\
         pub fn words(sink s: String) -> u64 { let mut t = s; t.push_str(in \" grown well past the borrowed bytes\"); t.len() }\n\
         pub fn keep(sink s: String) -> u64 { s.len() }\n",
    )
    .unwrap();
    let object = temp.0.join("bv.o");
    vesper(&[
        "build".as_ref(),
        "--lib".as_ref(),
        file.as_os_str(),
        "-o".as_ref(),
        object.as_os_str(),
    ]);
    let driver = r#"#include <stdio.h>
#include <stdint.h>
typedef struct { uint64_t len, cap, data; } H;
int64_t vesper_bv_seq_new(int64_t, int64_t);
int64_t vesper_bv_live_allocations(void);
uint64_t vesper_bv_take(uint64_t);
uint64_t vesper_bv_words(uint64_t *);
uint64_t vesper_bv_keep(uint64_t *);
int main(void) {
  int64_t before = vesper_bv_live_allocations();
  uint64_t sum = 0;
  for (int r = 0; r < 100; r++) {
    H *h = (H *)vesper_bv_seq_new(4, 8);
    uint64_t *d = (uint64_t *)h->data; d[0] = 1; d[1] = 2; h->len = 2;
    sum += vesper_bv_take((uint64_t)h);
    uint64_t w[3] = {5, 0, (uint64_t)"hello"};
    sum += vesper_bv_words(w);
    uint64_t k[3] = {5, 0, (uint64_t)"hello"};
    sum += vesper_bv_keep(k);
  }
  printf("%lld %lld %llu\n", (long long)before, (long long)vesper_bv_live_allocations(), (unsigned long long)sum);
  return 0;
}
"#;
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
        "{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0 0 4800");
}
