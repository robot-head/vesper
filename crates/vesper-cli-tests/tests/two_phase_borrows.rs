//! Two-phase borrows (§4.3, port gaps lane 3, #986): the `inout` receiver
//! loan of a builtin or method call is reserved while the call's arguments
//! are evaluated and activated only at the call. Arguments may read the
//! receiver's place and see its value before the call; writes, moves and
//! `inout` borrows of it in the arguments stay refused (`E0410` in the Place
//! IR check, `E0301` in the checker). The evaluator, both JIT
//! configurations and an executable agree.
mod support;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-two-phase-{}-{}",
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

/// Lean extraction reads the receiver for the argument before it applies
/// the update, so it agrees with the backends.
#[test]
fn lean_extraction_reads_before_the_update() {
    let temp = Temp::new();
    let file = temp.0.join("lean.ves");
    fs::write(
        &file,
        "fn inc(inout s: ChunkSeq[u64], i: u64) -> bool eff [Fail[RuntimeError]] {
    s.try_set(i, wrapping_add(s.get(i), 1))
}
fn main() -> u64 { 0 }
",
    )
    .unwrap();
    let out = support::vesper()
        .arg("emit")
        .arg(&file)
        .arg("--emit")
        .arg("lean")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lean = String::from_utf8_lossy(&out.stdout);
    let def = lean
        .lines()
        .find(|l| l.starts_with("def «inc»"))
        .expect("inc is extracted");
    let read = def.find("getElem!").expect("the argument reads s");
    let write = def.find("setIfInBounds").expect("the call writes s");
    assert!(read < write, "{def}");
}
