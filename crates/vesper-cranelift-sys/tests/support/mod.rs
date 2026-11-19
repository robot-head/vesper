//! Shared helpers: load `@vesper/cranelift` with a fixture, check it against
//! the host's `.vsys` target and run entries under the JIT with the shim's
//! symbols bound.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use vesper::ffi_lowering::{self, ForeignChecked, Handler, Imports};
use vesper::runtime::Value;

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

/// The package plus `extra`, checked for `target`.
pub fn checked_with(extra: &str, target: (&str, &str)) -> ForeignChecked {
    let package = package_source();
    let mut imports = Imports::new();
    imports
        .add(&vsys(&package, target), Handler::Direct)
        .unwrap();
    let source = format!("{package}\n{extra}");
    ffi_lowering::check_source(&source, &imports).unwrap_or_else(|e| panic!("{e}"))
}

pub fn word(v: &Value) -> u64 {
    match v {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        other => panic!("not a word: {other:?}"),
    }
}

/// Run `entry` under the JIT with the shim bound; returns its word.
pub fn jit(checked: &ForeignChecked, entry: &str) -> u64 {
    let symbols = vesper_cranelift_sys::symbols();
    let v = checked
        .jit(entry, &symbols)
        .unwrap_or_else(|e| panic!("{entry}: {e}"));
    word(&v)
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
