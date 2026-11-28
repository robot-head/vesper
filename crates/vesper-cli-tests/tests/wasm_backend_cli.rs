//! `vesper build --target wasm32-wasip2`: the CLI writes a component that
//! Wasmtime 50 instantiates and runs.
mod support;
use std::process::Command;

fn vesper() -> Command {
    support::vesper()
}

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vesper-wasm-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn unsupported_source_fails_with_a_typed_error() {
    let out = temp("unhandled.wasm");
    let status = vesper()
        .args([
            "build",
            "--target",
            "wasm32-wasip2",
            "tests/fixtures/wasm_backend/unhandled.ves",
            "-o",
        ])
        .arg(&out)
        .output()
        .unwrap();
    assert!(!status.status.success());
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        stderr.contains("entry: the entry must handle effect Net"),
        "{stderr}"
    );
    assert!(!out.exists());
}

#[test]
fn native_only_flags_are_rejected_for_wasm() {
    let out = temp("tee.wasm");
    let status = vesper()
        .args([
            "build",
            "--target",
            "wasm32-wasip2",
            "--tee",
            "tests/fixtures/wasm_backend/shapes.ves",
            "-o",
        ])
        .arg(&out)
        .output()
        .unwrap();
    assert!(!status.status.success());
    assert!(String::from_utf8_lossy(&status.stderr).contains("native targets only"));
}
