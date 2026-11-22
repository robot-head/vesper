//! Variadic C functions through `.vsys` (SH6 lane 13, #1408): the JIT, the
//! evaluator's host and an executable all call libc's `snprintf` and
//! `fcntl`, whose variadic arguments Apple arm64 reads from the stack.
#![cfg(unix)]
use std::{fs, path::PathBuf, process::Command};
use vesper::ffi_lowering::{self, Handler, Imports};

unsafe extern "C" {
    fn snprintf(buf: *mut u8, n: usize, format: *const u8, ...) -> i32;
}

fn target() -> (&'static str, &'static str) {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("aarch64-unknown-linux-gnu", "aapcs64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "darwin64")
    } else {
        ("x86_64-unknown-linux-gnu", "sysv64")
    }
}

fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
    ))
}

const SNPRINTF: &str = "    fn snprintf(nocapture inout buf: CSlice[u8], nocapture in format: CStr, ..., a: i64, b: i64) -> ForeignVal[i32];";

/// `snprintf(buf, 32, "%ld:%ld", a, b)`, then the digits back as a number:
/// a wrong variadic slot prints garbage instead of `a` and `b`.
const SOURCE: &str = "fn digits(in buf: ChunkSeq[u8], n: u64) -> u64 {\n    \
     let mut out: u64 = 0;\n    let mut i: u64 = 0;\n    loop {\n        if i >= n { break; }\n        \
     match buf.try_get(i) {\n            Option::Some(c) => {\n                \
     if c != 58 { out = wrapping_add(wrapping_mul(out, 10), widen_u64(wrapping_sub(c, 48))); }\n            }\n            \
     Option::None => {}\n        }\n        i = wrapping_add(i, 1);\n    }\n    out\n}\n\n\
     pub fn main() -> u64 eff [Ffi[LibC]] {\n    let mut buf: ChunkSeq[u8] = ChunkSeq::new[u8]();\n    \
     buf.resize(32, 0);\n    let format = \"%ld:%ld\".as_bytes();\n    \
     match LibC::snprintf(inout buf.as_slice_mut(), in format, 4321, 98) {\n        \
     Result::Ok(n) => digits(in buf, wrapping_u64(n)),\n        Result::Err(_) => 1,\n    }\n}\n";

fn library() -> String {
    let (t, a) = target();
    format!(
        "sys library LibC (target = \"{t}\", abi = \"{a}\", link = \"dynamic\", name = \"c\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {{\n{SNPRINTF}\n}}\n"
    )
}

#[test]
fn the_jit_and_the_evaluator_call_snprintf() {
    if !host_jit() {
        return;
    }
    let mut imports = Imports::new();
    imports.add(&library(), Handler::Direct).unwrap();
    let checked = ffi_lowering::check_source(SOURCE, &imports).unwrap_or_else(|e| panic!("{e}"));
    let symbols = [("snprintf", snprintf as *const u8)];
    for optimize in [true, false] {
        let v = checked
            .jit_with_options("main", &symbols, optimize)
            .unwrap();
        assert_eq!(v.to_string(), "432198", "optimize {optimize}");
    }
    let host = ffi_lowering::EvalHost::new(&imports, &symbols).unwrap();
    let v = checked.evaluate("main", |call| host.call(call)).unwrap();
    assert_eq!(v.to_string(), "432198");
}

/// A scratch directory, removed on drop.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An executable sets `O_NONBLOCK` on a pipe through `fcntl(fd, F_SETFL,
/// flags)` and reads it back with `F_GETFL`; it exits 0 when the flag stuck.
#[test]
fn an_executable_calls_fcntl() {
    if !host_jit() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("vesper-vsys-variadic-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let t = Temp(dir);
    let app = t.0.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(app.join("sys")).unwrap();
    fs::write(
        app.join("Loom.toml"),
        "[package]\nname = \"@t/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[LibC]\"]\nvsys = [\"sys/libc.vsys\"]\n",
    )
    .unwrap();
    fs::write(
        app.join("sys/libc.vsys"),
        "sys library LibC (target = \"@TARGET@\", abi = \"@ABI@\", link = \"dynamic\", name = \"c\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n    \
         fn pipe(nocapture inout fds: CSlice[u32]) -> ForeignVal[i32];\n    \
         fn fcntl(fd: i32, cmd: i32, ..., arg: i64) -> ForeignVal[i32];\n}\n",
    )
    .unwrap();
    // F_GETFL = 3, F_SETFL = 4 on Linux and macOS; O_NONBLOCK differs.
    let nonblock = if cfg!(target_os = "macos") { 4 } else { 2048 };
    fs::write(
        app.join("src/main.ves"),
        format!(
            "pub fn main() -> u64 eff [Ffi[LibC]] {{\n    let mut fds: ChunkSeq[u32] = ChunkSeq::new[u32]();\n    \
             fds.resize(2, 0);\n    if LibC::pipe(inout fds.as_slice_mut()) != 0 {{ return 10; }}\n    \
             let fd = match fds.try_get(0) {{ Option::Some(fd) => wrapping_i32(fd), Option::None => return 11 }};\n    \
             let before = LibC::fcntl(fd, 3, 0);\n    \
             if LibC::fcntl(fd, 4, {nonblock}) != 0 {{ return 12; }}\n    \
             let after = LibC::fcntl(fd, 3, 0);\n    \
             if (after & {nonblock}) != 0 && (before & {nonblock}) == 0 {{ 0 }} else {{ 13 }}\n}}\n"
        ),
    )
    .unwrap();
    let exe = t.0.join("fcntl.bin");
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&app)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = Command::new(&exe).status().unwrap();
    assert_eq!(status.code(), Some(0));
}
