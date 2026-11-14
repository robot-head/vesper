//! `vesper build --codegen verified` end to end.
use std::path::PathBuf;
use std::process::Command;
use vesper::verified_codegen::Target;

fn vesper() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/verified_codegen")
        .join(name)
}
fn scratch() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "vesper-verified-codegen-cli-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn builds_and_runs_on_the_host() {
    let dir = scratch();
    let exe = dir.join("fib");
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
        let exe = dir.join("overflow");
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

#[test]
fn rejects_bad_flags_and_unsupported_programs() {
    let dir = scratch();
    for args in [
        vec!["build", "--codegen", "cranelift"],
        vec!["run", "--codegen", "verified"],
        vec!["build", "--codegen", "verified", "--verify-stack"],
        vec![
            "build",
            "--codegen",
            "verified",
            "--target",
            "riscv64gc-unknown-linux-gnu",
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
    std::fs::write(&source, "fn main() -> f64 { 1.5 }\n").unwrap();
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
