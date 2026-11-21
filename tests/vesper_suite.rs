//! The Vesper test suite (Rust-exit wave SH6 lane 19, #1414): every `.ves`
//! file under tests/vesper/ passes `vesper test --engines all`, and each
//! converted family still has the cases its `converted.json` records.
//!
//! This is the only Rust in the suite: it runs the CLI so that `cargo test`
//! and `bazel test` run the Vesper tests on every CI platform. The cases
//! themselves are Vesper sources (tests/vesper/README.md); once the Rust
//! compiler retires, CI runs the same command directly.

use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf, process::Command};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The command CI runs: the same as tests/vesper/README.md's.
fn run_suite() -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .current_dir(root())
        .args([
            "test",
            "tests/vesper",
            "--engines",
            "all",
            "--no-cache",
            "--fuel",
            "200000000",
            "--json",
        ])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid json ({e})\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let failures: Vec<String> = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|file| {
            let name = file["file"].as_str().unwrap_or("?").to_string();
            let mut failed: Vec<String> = file["tests"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| t["passed"] != true)
                .map(|t| format!("{name}: {} ({})", t["name"], t["message"]))
                .collect();
            if let Some(error) = file["error"].as_str() {
                failed.push(format!("{name}: {error}"));
            }
            failed
        })
        .collect();
    assert!(
        out.status.success() && report["passed"] == true && failures.is_empty(),
        "the Vesper suite failed:\n{}\nstderr: {}",
        failures.join("\n"),
        String::from_utf8_lossy(&out.stderr)
    );
    report
}

#[test]
fn every_vesper_test_passes_on_every_engine() {
    let report = run_suite();
    // Each converted family keeps the cases the converter wrote.
    let mut per_family: BTreeMap<String, usize> = BTreeMap::new();
    for file in report["files"].as_array().unwrap() {
        let path = file["file"].as_str().unwrap().replace('\\', "/");
        let family = path
            .trim_start_matches("tests/vesper/")
            .split('/')
            .next()
            .unwrap()
            .to_string();
        *per_family.entry(family).or_default() += file["tests"].as_array().unwrap().len();
    }
    let mut families = 0;
    for entry in std::fs::read_dir(root().join("tests/vesper")).unwrap() {
        let dir = entry.unwrap().path();
        let manifest = dir.join("converted.json");
        if !manifest.is_file() {
            continue;
        }
        families += 1;
        let data: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        let family = dir.file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(
            per_family.get(&family).copied().unwrap_or(0),
            data["cases"].as_u64().unwrap() as usize,
            "{family}: the converter wrote {} cases (converted.json); the suite ran a different number",
            data["cases"]
        );
        // The Rust file a family came from either still exists (both suites
        // run until the flip prunes it) or names no test kept in Rust.
        let from = root().join(data["from"].as_str().unwrap());
        assert!(
            from.is_file() || data["kept_in_rust"].as_array().unwrap().is_empty(),
            "{family}: {} was deleted while converted.json still lists tests kept in Rust",
            data["from"]
        );
    }
    assert!(families > 0, "no converted family under tests/vesper");
}
