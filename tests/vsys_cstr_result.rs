//! `-> ForeignVal[Option[CStr]]` (SH6 lane 13, #1408, gap G1): a C string
//! the library returns is copied into a fresh `ChunkSeq[u8]` (`NULL` is
//! `None`), in the JIT, the evaluator's host and an executable that reads
//! an environment variable with `getenv`.
use std::ffi::c_char;
use vesper::ffi_lowering::{self, Handler, Imports};

fn target() -> (&'static str, &'static str) {
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

fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    ))
}

/// `NULL`, "hello" or "": strings the library keeps.
extern "C" fn probe_str(which: u64) -> *const c_char {
    match which {
        1 => c"hello".as_ptr(),
        2 => c"".as_ptr(),
        _ => std::ptr::null(),
    }
}

const SOURCE: &str = "fn sum(in s: Slice[u8]) -> u64 {\n    let mut i: u64 = 0;\n    let mut t: u64 = 0;\n    loop {\n        \
     if i >= s.len() { break; }\n        match s.try_get(i) { Option::Some(c) => { t = wrapping_add(t, wrapping_u64(c)); }, Option::None => {} }\n        \
     i = wrapping_add(i, 1);\n    }\n    t\n}\n\n\
     pub fn run(which: u64) -> u64 eff [Ffi[Probe]] {\n    match Probe::probe_str(which) {\n        \
     Option::Some(s) => wrapping_add(1000000, wrapping_add(wrapping_mul(s.len(), 1000), sum(in s.as_slice()))),\n        \
     Option::None => 7,\n    }\n}\n\n\
     pub fn hello() -> u64 eff [Ffi[Probe]] { run(1) }\n\
     pub fn empty() -> u64 eff [Ffi[Probe]] { run(2) }\n\
     pub fn null() -> u64 eff [Ffi[Probe]] { run(0) }\n";

#[test]
fn a_returned_c_string_is_copied() {
    if !host_jit() {
        return;
    }
    let (t, a) = target();
    let library = format!(
        "sys library Probe (target = \"{t}\", abi = \"{a}\", link = \"static\", name = \"probe\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {{\n    \
         fn probe_str(which: u64) -> ForeignVal[Option[CStr]];\n}}\n"
    );
    let mut imports = Imports::new();
    imports.add(&library, Handler::Direct).unwrap();
    let checked = ffi_lowering::check_source(SOURCE, &imports).unwrap_or_else(|e| panic!("{e}"));
    let symbols = [("probe_str", probe_str as *const u8)];
    // "hello": 5 bytes summing to 532; "": none; NULL: `None`.
    let expected = [("hello", "1005532"), ("empty", "1000000"), ("null", "7")];
    for optimize in [true, false] {
        for (entry, want) in expected {
            let v = checked.jit_with_options(entry, &symbols, optimize).unwrap();
            assert_eq!(v.to_string(), want, "{entry} optimize {optimize}");
        }
    }
    let host = ffi_lowering::EvalHost::new(&imports, &symbols).unwrap();
    for (entry, want) in expected {
        let v = checked.evaluate(entry, |call| host.call(call)).unwrap();
        assert_eq!(v.to_string(), want, "{entry} evaluated");
    }
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
    let status = Command::new(&exe)
        .env("VESPER_G1_PROBE", "abc")
        .env_remove("VESPER_G1_UNSET")
        .status()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(status.code(), Some(43));
}
