//! Port wave P0 lane 1 (F0): multi-file packages, path dependencies and
//! `use` imports. Fixtures live in tests/fixtures/packages/.
mod support;
use std::process::Command;

const APP: &str = "tests/fixtures/packages/workspace/app";
/// `total(7, 9 + 10) + 50 + 0 + 7 + count_twice()` (4).
const EXPECTED: i64 = 780;

fn cli() -> Command {
    support::vesper()
}

#[test]
fn cli_check_run_and_jit_accept_a_package_directory() {
    let o = cli().args(["check", APP]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    for command in ["run", "jit"] {
        let o = cli().args([command, APP]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim(),
            EXPECTED.to_string()
        );
    }
}

#[test]
#[cfg_attr(
    target_os = "windows",
    ignore = "vesper build uses a Unix cc linker; MSVC executable linking is not implemented"
)]
fn cli_build_links_a_package_executable() {
    let exe = std::env::temp_dir().join(format!("vesper-package-{}", std::process::id()));
    let o = cli()
        .arg("build")
        .arg(APP)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let path = exe;
    let status = Command::new(&path).status().unwrap();
    // Exit statuses are truncated to a byte outside Windows.
    assert_eq!(
        status.code().map(|c| c % 256),
        Some((EXPECTED % 256) as i32)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_variant_spelled_like_a_type_leaves_the_type_alone() {
    // `@accept/kinds` has a variant `Kind::String`; a module that sees no
    // `Kind` still names the `String` type. 1 * 10 + "four".len().
    let o = cli()
        .args([
            "run",
            "tests/fixtures/packages/accept/variant_named_like_type",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "14");
}

#[test]
fn optional_dependencies_are_not_loaded() {
    let o = cli()
        .args(["run", "tests/fixtures/packages/accept/optional_dependency"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "7");
}

#[test]
fn semantic_errors_point_at_the_right_file_with_qualified_names() {
    let dir = std::env::temp_dir().join(format!("vesper-package-error-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/deep")).unwrap();
    std::fs::write(
        dir.join("Loom.toml"),
        "[package]\nname = \"@demo/errors\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.ves"),
        "use self::deep::thing::value;\n\nfn main() -> i64 {\n    value()\n}\n",
    )
    .unwrap();
    // A type error in a nested module: `true` is not an i64.
    std::fs::write(
        dir.join("src/deep/thing.ves"),
        "pub fn value() -> i64 {\n    true\n}\n",
    )
    .unwrap();
    let o = cli().arg("check").arg("--json").arg(&dir).output().unwrap();
    assert!(!o.status.success());
    let stdout = String::from_utf8_lossy(&o.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    let first = &value[0];
    assert!(
        first["file"]
            .as_str()
            .unwrap()
            .ends_with("src/deep/thing.ves"),
        "{stdout}"
    );
    let span = first["span"]["start"].as_u64().unwrap() as usize;
    let text = std::fs::read_to_string(dir.join("src/deep/thing.ves")).unwrap();
    assert!(span <= text.len(), "{stdout}");
    assert!(
        !first["message"]
            .as_str()
            .unwrap()
            .contains("demo__errors__"),
        "{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
