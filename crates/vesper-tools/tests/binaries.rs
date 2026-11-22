//! Every `vesper-<tool>` binary runs on its own. This integration test also
//! makes `cargo test` build the tool executables, which the root package's
//! CLI tests reach through `vesper <tool>`.
use std::process::{Command, Stdio};

fn run(exe: &str, args: &[&str]) -> std::process::Output {
    Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("{exe}: {e}"))
}

#[test]
fn every_tool_prints_its_usage() {
    for (exe, usage) in [
        (env!("CARGO_BIN_EXE_vesper-audit"), "vesper audit"),
        (env!("CARGO_BIN_EXE_vesper-bench"), "vesper bench"),
        (env!("CARGO_BIN_EXE_vesper-grammar"), "vesper grammar"),
        (env!("CARGO_BIN_EXE_vesper-json"), "vesper json"),
        (env!("CARGO_BIN_EXE_vesper-publish"), "vesper publish"),
        (env!("CARGO_BIN_EXE_vesper-resolve"), "vesper resolve"),
        (env!("CARGO_BIN_EXE_vesper-sbom"), "vesper sbom"),
        (env!("CARGO_BIN_EXE_vesper-tee"), "vesper-tee measure"),
        (env!("CARGO_BIN_EXE_vesper-attest"), "attest"),
        (env!("CARGO_BIN_EXE_vesper-cas"), "cas"),
        (env!("CARGO_BIN_EXE_vesper-fuzz"), "fuzz"),
        (env!("CARGO_BIN_EXE_vesper-migrate"), "migrate"),
        (env!("CARGO_BIN_EXE_vesper-refute"), "refute"),
        (env!("CARGO_BIN_EXE_vesper-replay"), "replay"),
        (env!("CARGO_BIN_EXE_vesper-semver"), "semver"),
        (env!("CARGO_BIN_EXE_vesper-simd"), "simd"),
        (env!("CARGO_BIN_EXE_vesper-top"), "top"),
        (env!("CARGO_BIN_EXE_vesper-traceparent"), "traceparent"),
    ] {
        let out = run(exe, &["--help"]);
        assert!(out.status.success(), "{exe}: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains(usage), "{exe}: {text}");
    }
}

#[test]
fn errors_use_the_compiler_format() {
    let out = run(env!("CARGO_BIN_EXE_vesper-tee"), &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "vesper: usage: vesper-tee measure <executable>\n"
    );
}

#[test]
fn tee_measures_a_file() {
    let dir = std::env::temp_dir().join(format!("vesper-tee-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("artifact");
    std::fs::write(&file, b"vesper").unwrap();
    let out = run(
        env!("CARGO_BIN_EXE_vesper-tee"),
        &["measure", file.to_str().unwrap()],
    );
    assert!(out.status.success(), "{out:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("artifact.tee.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(report["schema"], "vesper.tee-measurement/1");
    assert_eq!(report["artifact_bytes"], 6);
    assert_eq!(report["blake3"], blake3::hash(b"vesper").to_hex().as_str());
}
