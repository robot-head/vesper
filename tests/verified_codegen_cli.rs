//! `vesper build --codegen verified` end to end.
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use vesper::verified_codegen::Target;

fn vesper() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
fn builds_and_runs_on_the_host() {
    let dir = scratch("run");
    // Windows starts executables by their `.exe` name.
    let exe = dir.join(format!("fib{}", std::env::consts::EXE_SUFFIX));
    let host = Target::host();
    let mut cmd = vesper();
    cmd.arg("build")
        .arg(fixture("fibonacci.ves"))
        .args(["--codegen", "verified", "-o"]);
    cmd.arg(&exe);
    if host.is_none() {
        // Not a supported host: cross-build the x86-64 Linux executable.
        cmd.args(["--target", "x86_64-unknown-linux-gnu"]);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("verified codegen"), "{stdout}");
    if host.is_some() {
        // fib(20) = 6765; the exit status is its low byte.
        let status = Command::new(&exe).status().unwrap();
        assert_eq!(status.code(), Some(6765 % 256));
        // An unhandled overflow exits with the documented status.
        let exe = dir.join(format!("overflow{}", std::env::consts::EXE_SUFFIX));
        let out = vesper()
            .arg("build")
            .arg(fixture("overflow.ves"))
            .args(["--codegen", "verified", "-o"])
            .arg(&exe)
            .output()
            .unwrap();
        assert!(out.status.success());
        let status = Command::new(&exe).status().unwrap();
        assert_eq!(
            status.code(),
            Some(i32::from(vesper::verified_codegen::EXIT_OVERFLOW))
        );
    } else {
        eprintln!("host is not a verified-codegen target; run unavailable");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
