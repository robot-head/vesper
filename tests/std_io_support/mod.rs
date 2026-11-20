//! Shared harness of tests/std_fs.rs, std_process.rs and std_env.rs
//! (self-hosting SH0 lane 3, #1125): a driver package over
//! `@vesper-std/fs`, `process` and `env`, checked against their three
//! `.vsys` imports, and run
//!
//! * in the JIT with the native optimizer on and off, the imports bound to
//!   the real host shim (`vesper_std_host::symbols`), and
//! * in the evaluator, through the compiler's `ffi_lowering::EvalHost`
//!   (the host of `vesper run`): it marshals each foreign call's source
//!   values into the `.vsys` C ABI (driven by the parsed library
//!   descriptors) and calls the same host functions, so both paths touch
//!   the real file system.
//!
//! Each run must leave no live host handle behind (the leak check).
#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use vesper::ffi_lowering::{self, ForeignChecked, Handler, Imports};
use vesper::runtime::Value;

/// The packages and their `.vsys` libraries.
pub const LIBRARIES: [&str; 3] = ["fs", "process", "env"];

/// JIT and evaluator runs share the process-wide handle counters, so they
/// run one at a time.
pub static SERIAL: Mutex<()> = Mutex::new(());

/// The std packages: tests run from the repository root (under Bazel too,
/// where the root package's files are the test's data).
pub fn std_dir() -> PathBuf {
    fs::canonicalize("packages/vesper-std").expect("run from the repository root")
}

/// The host's `.vsys` target and calling convention. Any value checks; the
/// JIT needs the real one.
pub fn host_target() -> (&'static str, &'static str) {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("aarch64-unknown-linux-gnu", "aapcs64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "darwin64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        ("x86_64-pc-windows-msvc", "win64")
    } else {
        ("x86_64-unknown-linux-gnu", "sysv64")
    }
}

/// Whether this host has a `.vsys` target the JIT can call.
pub fn jit_host() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    ))
}

/// The `.vsys` source of package `lib` for this host.
pub fn vsys(lib: &str) -> String {
    let (t, a) = host_target();
    fs::read_to_string(std_dir().join(lib).join(format!("{lib}.vsys")))
        .unwrap()
        .replace("@TARGET@", t)
        .replace("@ABI@", a)
}

pub fn imports() -> Imports {
    let mut imports = Imports::new();
    for lib in LIBRARIES {
        imports.add(&vsys(lib), Handler::Direct).unwrap();
    }
    imports
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A temporary directory, removed on drop.
pub struct Temp(pub PathBuf);
impl Temp {
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
    /// The directory as a Vesper string literal's contents.
    pub fn lit(&self) -> String {
        lit(&self.0)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `path` with `/` separators, escaped for a Vesper string literal.
pub fn lit(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "/")
        .replace('"', "\\\"")
}

/// A driver package `@local/std-io-driver` whose `src/main.ves` is
/// `source`, depending on the three std I/O packages by absolute path.
pub fn driver(source: &str) -> Temp {
    let temp = Temp::new("std-io-driver");
    let std = std_dir();
    let mut deps = String::new();
    for name in LIBRARIES {
        deps.push_str(&format!(
            "\"@vesper-std/{name}\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
            lit(&std.join(name))
        ));
    }
    let manifest = format!(
        "[package]\nname = \"@local/std-io-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\
         capability_ceiling = [\"Fail\", \"Alloc\", \"Ffi[StdFs]\", \"Ffi[StdProcess]\", \"Ffi[StdEnv]\"]\n\n\
         [dependencies]\n{deps}"
    );
    fs::write(temp.0.join("Loom.toml"), manifest).unwrap();
    fs::create_dir_all(temp.0.join("src")).unwrap();
    fs::write(temp.0.join("src/main.ves"), source).unwrap();
    temp
}

/// Load and check `dir` against the three imports.
pub fn check(dir: &Path) -> ForeignChecked {
    let assembled = vesper::packages::load(dir).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    ffi_lowering::check_module(&assembled.module, &imports()).unwrap_or_else(|e| {
        let shown: Vec<_> = e
            .diagnostics
            .iter()
            .map(|d| assembled.sources.localize(d))
            .collect();
        panic!("{e}: {shown:?}")
    })
}

/// Check `source` as a driver package.
pub fn check_driver(source: &str) -> ForeignChecked {
    let pkg = driver(source);
    check(pkg.path())
}

/// One way of running an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Jit,
    JitNoOpt,
    Evaluator,
}
pub const MODES: [Mode; 3] = [Mode::Jit, Mode::JitNoOpt, Mode::Evaluator];

/// Run `entry` in `mode`; panics with the error. Checks that every host
/// handle the run created was finalized.
pub fn run(checked: &ForeignChecked, entry: &str, mode: Mode) -> Value {
    let before = vesper_std_host::live_handles();
    let got = match mode {
        Mode::Jit | Mode::JitNoOpt => checked
            .jit_with_options(entry, &vesper_std_host::symbols(), mode == Mode::Jit)
            .unwrap_or_else(|e| panic!("{entry} ({mode:?}): {e}")),
        Mode::Evaluator => {
            let host = ffi_lowering::EvalHost::new(&imports(), &vesper_std_host::symbols())
                .unwrap_or_else(|e| panic!("{entry}: {e}"));
            checked
                .evaluate(entry, |call| host.call(call))
                .unwrap_or_else(|e| panic!("{entry} ({mode:?}): {e}"))
        }
    };
    assert_eq!(
        vesper_std_host::live_handles(),
        before,
        "{entry} ({mode:?}): live handles [file, dir, command, child]"
    );
    got
}

/// The u64 an entry returned.
pub fn word(v: &Value) -> u64 {
    match v {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        Value::SInt(n, _) => *n as u64,
        other => panic!("not a word: {other:?}"),
    }
}

/// Run `entry` in every mode the host supports and require one answer.
pub fn agree(checked: &ForeignChecked, entry: &str, modes: &[Mode]) -> u64 {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut answer = None;
    for &mode in modes {
        if mode != Mode::Evaluator && !jit_host() {
            continue;
        }
        let got = word(&run(checked, entry, mode));
        match answer {
            None => answer = Some((mode, got)),
            Some((first, want)) => assert_eq!(got, want, "{entry}: {mode:?} vs {first:?}"),
        }
    }
    answer.expect("no mode ran").1
}
