//! `nocapture in argv: CStrList` (SH6 lane 13, #1408, gap G2): one source
//! `Slice[u8]` of NUL-ended strings becomes C's NULL-terminated
//! `char *const argv[]`, in the JIT, the evaluator's host and an executable
//! that spawns a process with `posix_spawnp`.
mod support;

fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    ))
}

/// An executable spawns `/bin/sh -c "exit 7"` with `posix_spawnp`, whose
/// `argv` and (empty) `envp` are `CStrList`s, and waits for it.
#[cfg(unix)]
#[test]
fn an_executable_spawns_a_process() {
    use std::{fs, process::Command};
    if !host_jit() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("vesper-cstr-list-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let app = dir.join("app");
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
         fn posix_spawnp(nocapture inout pid: i32, nocapture in file: CStr, actions: u64, attributes: u64, \
         nocapture in argv: CStrList, nocapture in envp: CStrList) -> ForeignVal[i32];\n    \
         fn waitpid(pid: i32, nocapture inout status: i32, options: i32) -> ForeignVal[i32];\n}\n",
    )
    .unwrap();
    // argv = "sh\0-c\0exit 7\0", envp = "" (no variables).
    fs::write(
        app.join("src/main.ves"),
        "fn text(inout s: ChunkSeq[u8], in t: Str) {\n    let b = t.as_bytes();\n    let mut i: u64 = 0;\n    loop {\n        \
         if i >= b.len() { break; }\n        match b.try_get(i) { Option::Some(c) => s.push(c), Option::None => {} }\n        \
         i = wrapping_add(i, 1);\n    }\n    s.push(0);\n}\n\n\
         pub fn main() -> u64 eff [Ffi[LibC]] {\n    let mut argv: ChunkSeq[u8] = ChunkSeq::new[u8]();\n    \
         text(inout argv, in \"sh\");\n    text(inout argv, in \"-c\");\n    text(inout argv, in \"exit 7\");\n    \
         let envp: ChunkSeq[u8] = ChunkSeq::new[u8]();\n    let mut pid: i32 = 0;\n    let file = \"sh\".as_bytes();\n    \
         let spawned = match LibC::posix_spawnp(inout pid, in file, 0, 0, in argv.as_slice(), in envp.as_slice()) {\n        \
         Result::Ok(r) => r,\n        Result::Err(_) => return 101,\n    };\n    if spawned != 0 { return 102; }\n    \
         let mut status: i32 = 0;\n    if LibC::waitpid(pid, inout status, 0) != pid { return 103; }\n    \
         // WEXITSTATUS: bits 8-15 on Linux and macOS.\n    wrapping_u64(wrapping_shr_i32(status, 8) & 255)\n}\n",
    )
    .unwrap();
    let exe = dir.join("spawn.bin");
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
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(status.code(), Some(7));
}
