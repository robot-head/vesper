//! Variadic C functions through `.vsys` (SH6 lane 13, #1408): the JIT, the
//! evaluator's host and an executable all call libc's `snprintf` and
//! `fcntl`, whose variadic arguments Apple arm64 reads from the stack.
#![cfg(unix)]
mod support;
use std::{fs, path::PathBuf, process::Command};

fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
    ))
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
    let out = support::vesper()
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
