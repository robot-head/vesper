//! `-> ForeignVal[Option[CStr]]` (SH6 lane 13, #1408, gap G1): a C string
//! the library returns is copied into a fresh `ChunkSeq[u8]` (`NULL` is
//! `None`), in the JIT, the evaluator's host and an executable that reads
//! an environment variable with `getenv`.
mod support;

fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    ))
}

/// An executable reads `VESPER_G1_PROBE` with libc's `getenv`, and a
/// variable that is not set.
#[cfg(unix)]
#[test]
fn an_executable_reads_an_environment_variable() {
    use std::{fs, process::Command};
    if !host_jit() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("vesper-cstr-result-{}", std::process::id()));
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
         fn getenv(nocapture in name: CStr) -> ForeignVal[Option[CStr]];\n}\n",
    )
    .unwrap();
    // 40 + the value's length when it is "abc"; 1 when it differs.
    fs::write(
        app.join("src/main.ves"),
        "fn is_abc(in s: Slice[u8]) -> bool {\n    if s.len() != 3 { return false; }\n    \
         match (s.try_get(0), s.try_get(1), s.try_get(2)) {\n        \
         (Option::Some(97), Option::Some(98), Option::Some(99)) => true,\n        _ => false,\n    }\n}\n\n\
         pub fn main() -> u64 eff [Ffi[LibC]] {\n    \
         let unset: u64 = match LibC::getenv(in \"VESPER_G1_UNSET\".as_bytes()) {\n        \
         Result::Ok(Option::None) => 0,\n        _ => return 2,\n    };\n    \
         match LibC::getenv(in \"VESPER_G1_PROBE\".as_bytes()) {\n        \
         Result::Ok(Option::Some(v)) => if is_abc(in v.as_slice()) { wrapping_add(wrapping_add(40, unset), v.len()) } else { 1 },\n        \
         _ => 3,\n    }\n}\n",
    )
    .unwrap();
    let exe = dir.join("getenv.bin");
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
    let status = Command::new(&exe)
        .env("VESPER_G1_PROBE", "abc")
        .env_remove("VESPER_G1_UNSET")
        .status()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(status.code(), Some(43));
}
