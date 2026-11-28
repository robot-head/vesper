//! A `.vsys` import of a C function the runtime also imports (graphics
//! gap 5): `write(fd: i32, ..)` against the runtime's three-word `write`,
//! `exit(code: i32)` against its one-word `exit`. Both declarations name
//! one symbol, so the import is declared with the runtime's word signature
//! and called through its address with the C signature, in either order.
mod support;
use std::{fs, path::PathBuf, process::Command};

const VSYS: &str =
    "sys library LibC (target = \"@TARGET@\", abi = \"@ABI@\", link = \"dynamic\", name = \"c\", \
     artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {
    fn write(fd: i32, nocapture in buf: CSlice[u8]) -> ForeignVal[i64];
    fn malloc(size: usize) -> ForeignVal[u64];
    fn free(p: u64) -> ForeignVal[Unit];
}
";

/// `count` makes a `ChunkSeq`, whose runtime imports `write`, `malloc`
/// and `free` with word signatures: before `main` (the runtime declares
/// them first) or after it (the imports do).
fn main_source(seq_first: bool) -> String {
    let count = "fn count() -> u64 {\n    let mut a: ChunkSeq[u64] = ChunkSeq::new[u64]();\n    a.push(1);\n    a.push(2);\n    a.len()\n}\n";
    let main = "\
pub fn main() -> u64 eff [Ffi[LibC]] {
    let p = LibC::malloc(16);
    LibC::free(p);
    let n = LibC::write(1, in \"hi from vesper\\n\".as_bytes());
    wrapping_add(wrapping_u64(n), count())
}
";
    if seq_first {
        format!("{count}{main}")
    } else {
        format!("{main}{count}")
    }
}

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The gap's repro: `vesper build` links an executable whose runtime
/// declares `write` itself, and the import writes to stdout.
#[test]
fn an_executable_imports_libc_write() {
    if cfg!(windows) || cfg!(not(any(target_arch = "x86_64", target_arch = "aarch64"))) {
        return;
    }
    let dir =
        std::env::temp_dir().join(format!("vesper-ffi-runtime-imports-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let t = Temp(dir.clone());
    fs::create_dir_all(dir.join("app/src")).unwrap();
    fs::create_dir_all(dir.join("app/sys")).unwrap();
    fs::write(
        dir.join("app/Loom.toml"),
        "[package]\nname = \"@t/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[LibC]\"]\nvsys = [\"sys/libc.vsys\"]\n",
    )
    .unwrap();
    fs::write(dir.join("app/sys/libc.vsys"), VSYS).unwrap();
    for seq_first in [true, false] {
        fs::write(dir.join("app/src/main.ves"), main_source(seq_first)).unwrap();
        let exe = dir.join("app.bin");
        let out = support::vesper()
            .arg("build")
            .arg(dir.join("app"))
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), "hi from vesper\n");
        assert_eq!(run.status.code(), Some(17));
    }
    drop(t);
}
