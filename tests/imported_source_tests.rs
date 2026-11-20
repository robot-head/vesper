use serde_json::Value;
use std::process::Command;

fn run_suites(suites: &[(&str, u64)]) {
    for &(path, count) in suites {
        let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .args(["test", path, "--no-cache", "--json"])
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
            panic!(
                "{path}: {err}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert!(output.status.success(), "{path}: {report}");
        assert_eq!(report["total"].as_u64(), Some(count), "{path}: {report}");
    }
}

#[test]
fn standard_library_packages() {
    run_suites(&[
        ("packages/vesper-std/collections", 65),
        ("packages/vesper-std/fmt", 90),
        ("packages/vesper-std/graph", 79),
        ("packages/vesper-std/hash", 45),
        ("packages/vesper-std/sort", 81),
        ("packages/vesper-std/tests", 3),
    ]);
}

#[test]
fn front_end() {
    run_suites(&[("packages/vesper-front/src/lib.ves", 36)]);
}

#[test]
fn ast_lowering() {
    run_suites(&[("packages/vesper-ast/src/lib.ves", 15)]);
}

#[test]
fn ported_crate_packages() {
    run_suites(&[
        ("crates/vesper-semver/vesper", 77),
        ("crates/vesper-prism-h3/vesper", 22),
    ]);
}
