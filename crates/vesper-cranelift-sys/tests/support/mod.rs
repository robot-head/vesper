//! Shared helpers: the `@vesper/cranelift` package and its fixtures, the
//! host's `.vsys` target, and (in [`stage2`]) Stage 2 builds of the
//! fixtures run against the Cranelift component.
#![allow(dead_code)]

/// The Stage 2 gate's harness: the committed seed and how to run it.
#[path = "../../../vesper-stage2-gate/tests/support/mod.rs"]
pub mod gate;
pub mod stage2;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The handle counters are process-wide: scenarios that read them run one
/// at a time.
pub static SERIAL: Mutex<()> = Mutex::new(());

pub fn repo() -> PathBuf {
    // Cargo runs tests in the crate directory; Bazel in the runfiles root.
    for base in [
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        PathBuf::from("."),
    ] {
        if base.join("packages/vesper-cranelift/src/lib.ves").exists() {
            return base;
        }
    }
    panic!("cannot find packages/vesper-cranelift");
}
pub fn package_source() -> String {
    std::fs::read_to_string(repo().join("packages/vesper-cranelift/src/lib.ves")).unwrap()
}
pub fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        repo()
            .join("crates/vesper-cranelift-sys/tests/fixtures")
            .join(name),
    )
    .unwrap()
}

/// The host's `.vsys` target and calling convention, if `vesper-ffi` has one.
pub fn host_target() -> Option<(&'static str, &'static str)> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("x86_64-unknown-linux-gnu", "sysv64"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(("aarch64-unknown-linux-gnu", "aapcs64"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(("aarch64-apple-darwin", "darwin64"))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(("x86_64-pc-windows-msvc", "win64"))
    } else {
        None
    }
}

/// The `sys library` of the package's `// @export vsys` directive.
pub fn vsys(source: &str, target: (&str, &str)) -> String {
    let mut out = String::new();
    for line in source.lines() {
        if let Some(rest) = line.strip_prefix("// | ") {
            out.push_str(rest);
            out.push('\n');
        } else if line == "// |" {
            out.push('\n');
        }
    }
    assert!(out.contains("sys library Cranelift"), "no .vsys directive");
    out.replace("@TARGET@", target.0).replace("@ABI@", target.1)
}

pub struct Temp(pub PathBuf);
impl Temp {
    pub fn new(tag: &str) -> Temp {
        let base = std::env::var_os("TEST_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!(
            "vesper-cranelift-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
