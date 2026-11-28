//! Keep-writes for abortable effects across stationary `inout` (manuscript
//! §4.3 `PostCall_abort`, issue #935). An abortable effect (`Fail`, `Cancel`,
//! any non-linear effect) may leave a call while an `inout` argument is
//! live, and the caller's place then holds the last value the callee wrote.
//! The checker accepts and rejects the cases below, and the evaluator, the
//! Cranelift JIT, an AOT executable and the extracted Lean model agree on the
//! post-abort value.
mod support;
use std::process::Command;

#[test]
#[cfg_attr(
    target_os = "windows",
    ignore = "vesper build uses a Unix cc linker; MSVC executable linking is not implemented"
)]
fn aot_executable_keeps_writes() {
    let source = "fn bump(inout x: i64, limit: i64) eff [Fail[i64]] { x = wrapping_add(x, 1); if x > limit { Fail::raise(x) } x = wrapping_add(x, 10); } \
                  fn outer(inout x: i64) eff [Fail[i64]] { x = wrapping_add(x, 2); bump(inout x, 0); x = 99; } \
                  fn main() -> i64 { let mut x = 1; let r = handle Fail[i64] in { outer(inout x); 0 } with { return v => v, raise(e, k) => e, }; wrapping_add(wrapping_mul(x, 10), r) }";
    let dir = std::env::temp_dir().join(format!("vesper-inout-abort-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("keep.ves");
    let exe = dir.join("keep");
    std::fs::write(&src, source).unwrap();
    let o = support::vesper()
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let status = Command::new(&exe).status().unwrap();
    assert_eq!(status.code(), Some(44));
    let _ = std::fs::remove_dir_all(dir);
}
