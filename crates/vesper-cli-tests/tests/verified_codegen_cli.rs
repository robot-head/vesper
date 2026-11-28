//! `vesper build --codegen verified` end to end.
mod support;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn vesper() -> Command {
    support::vesper()
}
fn fixture(name: &str) -> PathBuf {
    support::repo_root()
        .join("tests/fixtures/verified_codegen")
        .join(name)
}
/// A fresh scratch directory per call. The tests run in parallel and macOS
/// `SystemTime` has only microsecond resolution, so the name also carries the
/// test's tag and a process-wide counter; `create_dir` makes a collision fail
/// instead of two tests sharing (and one deleting) the same directory.
fn scratch(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "vesper-verified-codegen-cli-{tag}-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap();
    dir
}

#[test]
fn rejects_bad_flags_and_unsupported_programs() {
    let dir = scratch("reject");
    for args in [
        vec!["build", "--codegen", "cranelift"],
        vec!["run", "--codegen", "verified"],
        vec!["build", "--codegen", "verified", "--verify-stack"],
        vec![
            "build",
            "--codegen",
            "verified",
            "--target",
            "powerpc64le-unknown-linux-gnu",
        ],
    ] {
        let out = vesper()
            .args(&args)
            .arg(fixture("fibonacci.ves"))
            .arg("-o")
            .arg(dir.join("x"))
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?} should fail");
    }
    let source = dir.join("float.ves");
    std::fs::write(
        &source,
        "fn r(a: f64, b: f64) -> f64 { a % b }\nfn main() -> f64 { r(1.5, 1.0) }\n",
    )
    .unwrap();
    let out = vesper()
        .arg("build")
        .arg(&source)
        .args([
            "--codegen",
            "verified",
            "--target",
            "x86_64-unknown-linux-gnu",
            "-o",
        ])
        .arg(dir.join("y"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unsupported Float"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--target riscv64gc-unknown-linux-gnu` cross-builds a static RV64 ELF;
/// it runs under `qemu-riscv64` when that is installed.
#[test]
fn builds_rv64_executables() {
    let dir = scratch("rv64");
    let exe = dir.join("fib");
    let out = vesper()
        .arg("build")
        .arg(fixture("fibonacci.ves"))
        .args([
            "--codegen",
            "verified",
            "--target",
            "riscv64gc-unknown-linux-gnu",
            "-o",
        ])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&exe).unwrap();
    assert_eq!(&bytes[..4], b"\x7fELF");
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 243, "EM_RISCV");
    let qemu = std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .flat_map(|d| [d.join("qemu-riscv64"), d.join("qemu-riscv64-static")])
            .find(|q| q.is_file())
    });
    match qemu {
        Some(q) if cfg!(target_os = "linux") => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let status = Command::new(q).arg(&exe).status().unwrap();
            assert_eq!(status.code(), Some(6765 % 256));
        }
        _ => eprintln!("qemu-riscv64 unavailable; RV64 run skipped"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
