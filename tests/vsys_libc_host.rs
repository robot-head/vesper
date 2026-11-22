//! `vesper jit`, `run` and `test` bind a `.vsys` library that imports the C
//! library itself (`link = "dynamic", name = "c"`) from the process, the way
//! the linker binds it in a `vesper build` executable (SH6 lane 13, #1408,
//! gap G5). A library that cannot be loaded is refused.
#![cfg(unix)]
use std::{fs, path::PathBuf, process::Command};

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn package(tag: &str, link: &str) -> Temp {
    let dir = std::env::temp_dir().join(format!("vesper-libc-host-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("app/src")).unwrap();
    fs::create_dir_all(dir.join("app/sys")).unwrap();
    fs::write(
        dir.join("app/Loom.toml"),
        "[package]\nname = \"@t/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[LibC]\"]\nvsys = [\"sys/libc.vsys\"]\n",
    )
    .unwrap();
    fs::write(
        dir.join("app/sys/libc.vsys"),
        format!(
            "sys library LibC (target = \"@TARGET@\", abi = \"@ABI@\", link = \"dynamic\", name = \"{link}\", \
             artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {{\n    \
             fn access(nocapture in path: CStr, mode: i32) -> ForeignVal[i32] errno;\n    \
             fn snprintf(nocapture inout buf: CSlice[u8], nocapture in format: CStr, ..., a: i64) -> ForeignVal[i32];\n}}\n"
        ),
    )
    .unwrap();
    // ENOENT (2) from a missing path, plus the length of "%ld" of 12345 (5).
    fs::write(
        dir.join("app/src/main.ves"),
        "pub fn main() -> u64 eff [Ffi[LibC]] {\n    let path = \"vesper-no-such-dir/x\".as_bytes();\n    \
         let code = match LibC::access(in path, 0) {\n        Result::Ok(pair) => { let (_r, code) = pair; widen_u64(code) }\n        \
         Result::Err(_) => 100,\n    };\n    let mut buf: ChunkSeq[u8] = ChunkSeq::new[u8]();\n    buf.resize(16, 0);\n    \
         let format = \"%ld\".as_bytes();\n    let n = match LibC::snprintf(inout buf.as_slice_mut(), in format, 12345) {\n        \
         Result::Ok(n) => wrapping_u64(n),\n        Result::Err(_) => 200,\n    };\n    wrapping_add(wrapping_mul(code, 10), n)\n}\n",
    )
    .unwrap();
    Temp(dir)
}

fn vesper(command: &str, t: &Temp) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg(command)
        .arg(t.0.join("app"))
        .current_dir(&t.0)
        .output()
        .unwrap()
}

#[test]
fn jit_and_run_bind_libc_from_the_process() {
    if !cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        return;
    }
    let t = package("bind", "c");
    for command in ["jit", "run"] {
        let out = vesper(command, &t);
        assert!(
            out.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "25",
            "{command}"
        );
    }
}

#[test]
fn a_dynamic_library_that_does_not_load_is_refused() {
    let t = package("refuse", "vesper_no_such_library");
    let out = vesper("jit", &t);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("could not be loaded"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
