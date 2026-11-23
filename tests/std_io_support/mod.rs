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

/// The host's `@OS@` in a per-target `.vsys` path.
pub fn host_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

/// The `.vsys` source of package `lib` for this host: its per-target file
/// (`fs-linux-x86_64.vsys`, `env-linux.vsys`) when it has one, else
/// `env.vsys`.
pub fn vsys(lib: &str) -> String {
    let (t, a) = host_target();
    let dir = std_dir().join(lib);
    let os = host_os();
    let arch = std::env::consts::ARCH;
    let path = [
        format!("{lib}-{os}-{arch}.vsys"),
        format!("{lib}-{os}.vsys"),
        format!("{lib}.vsys"),
    ]
    .into_iter()
    .map(|f| dir.join(f))
    .find(|p| p.exists())
    .unwrap_or_else(|| dir.join(format!("{lib}.vsys")));
    fs::read_to_string(path)
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

/// The C library functions the std packages import directly (`link =
/// "dynamic"`, name `c`), as `vesper jit` binds them from the process.
#[cfg(unix)]
fn libc_symbols() -> Vec<(&'static str, *const u8)> {
    use std::ffi::{c_char, c_void};
    unsafe extern "C" {
        fn getenv(name: *const c_char) -> *mut c_char;
        fn getcwd(buf: *mut c_char, size: usize) -> *mut c_char;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
        fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
        fn strchr(s: *const c_char, c: i32) -> *mut c_char;
        fn strstr(haystack: *const c_char, needle: *const c_char) -> *mut c_char;
        fn open(path: *const c_char, flags: i32, ...) -> i32;
        fn read(fd: i32, buf: *mut c_void, n: usize) -> isize;
        fn write(fd: i32, buf: *const c_void, n: usize) -> isize;
        fn fsync(fd: i32) -> i32;
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        fn close(fd: i32) -> i32;
        fn fstat(fd: i32, st: *mut c_void) -> i32;
        fn stat(path: *const c_char, st: *mut c_void) -> i32;
        fn lstat(path: *const c_char, st: *mut c_void) -> i32;
        fn rename(from: *const c_char, to: *const c_char) -> i32;
        fn unlink(path: *const c_char) -> i32;
        fn rmdir(path: *const c_char) -> i32;
        fn mkdir(path: *const c_char, mode: u32) -> i32;
        fn getpid() -> i32;
        fn opendir(path: *const c_char) -> *mut c_void;
        fn readdir(dir: *mut c_void) -> *mut c_void;
        fn closedir(dir: *mut c_void) -> i32;
        fn bcopy(src: *const c_void, dst: *mut c_void, n: usize);
        fn openat(dirfd: i32, path: *const c_char, flags: i32, ...) -> i32;
        fn unlinkat(dirfd: i32, path: *const c_char, flags: i32) -> i32;
        fn fdopendir(fd: i32) -> *mut c_void;
        fn memset(dst: *mut c_void, byte: i32, n: usize) -> *mut c_void;
        fn posix_spawn_file_actions_init(fa: *mut c_void) -> i32;
        fn posix_spawn_file_actions_destroy(fa: *mut c_void) -> i32;
        fn posix_spawn_file_actions_adddup2(fa: *mut c_void, fd: i32, to: i32) -> i32;
        fn posix_spawn_file_actions_addopen(
            fa: *mut c_void,
            fd: i32,
            path: *const c_char,
            flags: i32,
            mode: u32,
        ) -> i32;
        fn posix_spawn_file_actions_addchdir_np(fa: *mut c_void, path: *const c_char) -> i32;
        fn posix_spawnattr_init(attr: *mut c_void) -> i32;
        fn posix_spawnattr_destroy(attr: *mut c_void) -> i32;
        fn posix_spawnattr_setflags(attr: *mut c_void, flags: i16) -> i32;
        fn posix_spawnattr_setsigmask(attr: *mut c_void, set: *const c_void) -> i32;
        fn posix_spawnattr_setsigdefault(attr: *mut c_void, set: *const c_void) -> i32;
        fn sigemptyset(set: *mut c_void) -> i32;
        fn sigaddset(set: *mut c_void, sig: i32) -> i32;
        fn posix_spawn(
            pid: *mut i32,
            path: *const c_char,
            fa: *const c_void,
            attr: *const c_void,
            argv: *const *mut c_char,
            envp: *const *mut c_char,
        ) -> i32;
        fn posix_spawnp(
            pid: *mut i32,
            file: *const c_char,
            fa: *const c_void,
            attr: *const c_void,
            argv: *const *mut c_char,
            envp: *const *mut c_char,
        ) -> i32;
        #[cfg_attr(target_os = "macos", link_name = "pipe")]
        #[cfg_attr(not(target_os = "macos"), link_name = "pipe2")]
        fn make_pipe(fds: *mut i32, flags: i32) -> i32;
        fn poll(fds: *mut c_void, n: usize, timeout: i32) -> i32;
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
        fn kill(pid: i32, sig: i32) -> i32;
        fn access(path: *const c_char, mode: i32) -> i32;
        fn exit(code: i32) -> !;
        #[cfg_attr(target_os = "macos", link_name = "__error")]
        #[cfg_attr(not(target_os = "macos"), link_name = "__errno_location")]
        fn errno_address() -> *mut i32;
    }
    vec![
        ("open", open as *const u8),
        ("read", read as *const u8),
        ("write", write as *const u8),
        ("fsync", fsync as *const u8),
        ("fcntl", fcntl as *const u8),
        ("close", close as *const u8),
        ("fstat", fstat as *const u8),
        ("stat", stat as *const u8),
        ("lstat", lstat as *const u8),
        ("rename", rename as *const u8),
        ("unlink", unlink as *const u8),
        ("rmdir", rmdir as *const u8),
        ("mkdir", mkdir as *const u8),
        ("getpid", getpid as *const u8),
        ("opendir", opendir as *const u8),
        ("readdir", readdir as *const u8),
        ("closedir", closedir as *const u8),
        ("bcopy", bcopy as *const u8),
        ("openat", openat as *const u8),
        ("unlinkat", unlinkat as *const u8),
        ("fdopendir", fdopendir as *const u8),
        ("memset", memset as *const u8),
        (
            "posix_spawn_file_actions_init",
            posix_spawn_file_actions_init as *const u8,
        ),
        (
            "posix_spawn_file_actions_destroy",
            posix_spawn_file_actions_destroy as *const u8,
        ),
        (
            "posix_spawn_file_actions_adddup2",
            posix_spawn_file_actions_adddup2 as *const u8,
        ),
        (
            "posix_spawn_file_actions_addopen",
            posix_spawn_file_actions_addopen as *const u8,
        ),
        (
            "posix_spawn_file_actions_addchdir_np",
            posix_spawn_file_actions_addchdir_np as *const u8,
        ),
        ("posix_spawnattr_init", posix_spawnattr_init as *const u8),
        (
            "posix_spawnattr_destroy",
            posix_spawnattr_destroy as *const u8,
        ),
        (
            "posix_spawnattr_setflags",
            posix_spawnattr_setflags as *const u8,
        ),
        (
            "posix_spawnattr_setsigmask",
            posix_spawnattr_setsigmask as *const u8,
        ),
        (
            "posix_spawnattr_setsigdefault",
            posix_spawnattr_setsigdefault as *const u8,
        ),
        ("sigemptyset", sigemptyset as *const u8),
        ("sigaddset", sigaddset as *const u8),
        ("posix_spawn", posix_spawn as *const u8),
        ("posix_spawnp", posix_spawnp as *const u8),
        (
            if cfg!(target_os = "macos") {
                "pipe"
            } else {
                "pipe2"
            },
            make_pipe as *const u8,
        ),
        ("poll", poll as *const u8),
        ("waitpid", waitpid as *const u8),
        ("kill", kill as *const u8),
        ("access", access as *const u8),
        ("exit", exit as *const u8),
        (
            if cfg!(target_os = "macos") {
                "__error"
            } else {
                "__errno_location"
            },
            errno_address as *const u8,
        ),
        ("getenv", getenv as *const u8),
        ("getcwd", getcwd as *const u8),
        ("dlsym", dlsym as *const u8),
        ("memcpy", memcpy as *const u8),
        ("strchr", strchr as *const u8),
        ("strstr", strstr as *const u8),
    ]
}
#[cfg(windows)]
fn libc_symbols() -> Vec<(&'static str, *const u8)> {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCommandLineW() -> *const u16;
        fn GetCurrentDirectoryW(n_buffer_length: u32, lp_buffer: *mut u16) -> u32;
        fn GetEnvironmentVariableW(lp_name: *const u16, lp_buffer: *mut u16, n_size: u32) -> u32;
        fn GetEnvironmentStringsW() -> *mut u16;
        fn FreeEnvironmentStringsW(lpsz_environment_block: *mut u16) -> i32;
        fn lstrlenW(lp_string: *const u16) -> i32;
        fn WideCharToMultiByte(
            code_page: u32,
            dw_flags: u32,
            lp_wide_char_str: *const u16,
            cch_wide_char: i32,
            lp_multi_byte_str: *mut u8,
            cb_multi_byte: i32,
            lp_default_char: *const c_void,
            lp_used_default_char: *mut i32,
        ) -> i32;
    }
    vec![
        ("GetCommandLineW", GetCommandLineW as *const u8),
        ("GetCurrentDirectoryW", GetCurrentDirectoryW as *const u8),
        (
            "GetEnvironmentVariableW",
            GetEnvironmentVariableW as *const u8,
        ),
        (
            "GetEnvironmentStringsW",
            GetEnvironmentStringsW as *const u8,
        ),
        (
            "FreeEnvironmentStringsW",
            FreeEnvironmentStringsW as *const u8,
        ),
        ("lstrlenW", lstrlenW as *const u8),
        ("WideCharToMultiByte", WideCharToMultiByte as *const u8),
    ]
}
#[cfg(not(any(unix, windows)))]
fn libc_symbols() -> Vec<(&'static str, *const u8)> {
    vec![]
}

/// Every symbol the three libraries import on this host: the host shim's
/// and the C library's.
pub fn symbols() -> Vec<(&'static str, *const u8)> {
    let mut s = vesper_std_host::symbols();
    s.extend(libc_symbols());
    s
}

/// Run `entry` in `mode`; panics with the error. Checks that every host
/// handle the run created was finalized.
pub fn run(checked: &ForeignChecked, entry: &str, mode: Mode) -> Value {
    let before = vesper_std_host::live_handles();
    let got = match mode {
        Mode::Jit | Mode::JitNoOpt => checked
            .jit_with_options(entry, &symbols(), mode == Mode::Jit)
            .unwrap_or_else(|e| panic!("{entry} ({mode:?}): {e}")),
        Mode::Evaluator => {
            let host = ffi_lowering::EvalHost::new(&imports(), &symbols())
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
