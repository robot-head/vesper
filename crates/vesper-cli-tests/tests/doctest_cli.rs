mod support;
use serde_json::Value;
fn invoke(source: &str) -> std::process::Output {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "vesper-doctest-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("docs.ves");
    std::fs::write(&file, source).unwrap();
    let output = support::vesper()
        .args(["test", "--doc"])
        .arg(file)
        .arg("--json")
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    output
}
#[test]
fn help_and_argument_errors_follow_the_requested_output_contract() {
    let help = support::vesper().args(["test", "--help"]).output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("test --doc [FILE|DIR]"));
    for args in [
        vec!["test", "--json"],
        vec!["test", "--unknown", "--json"],
        vec!["test", "--doc", "a.ves", "b.ves", "--json"],
    ] {
        let out = support::vesper().args(args).output().unwrap();
        assert!(!out.status.success());
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["passed"], false);
        assert!(value["error"].is_string());
    }
}
#[test]
fn cli_runs_multiple_entries_once_with_shared_helper_and_exact_locations() {
    let source = "//! ```vesper,entry=first\n//! fn first() -> i64 { helper(41) }\n//! ```\n/// ```vesper,entry=second\n/// fn second() -> i64 { helper(8) }\n/// ```\nfn helper(x: i64) -> i64 { wrapping_add(x, 1) }\n";
    let out = invoke(source);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let r = &report["report"];
    assert_eq!(r["batch"]["module_count"], 1);
    assert_eq!(r["batch"]["compilation_count"], 1);
    assert_eq!(r["batch"]["defined_functions"], 3);
    assert_eq!(r["batch"]["runs"], 4);
    assert_eq!(r["examples"][0]["result"], "42");
    assert_eq!(r["examples"][1]["result"], "9");
    assert_eq!(r["examples"][0]["line"], 1);
    assert_eq!(r["examples"][1]["line"], 4);
}
#[test]
fn linear_leak_exact_code_passes_wrong_code_and_base_errors_fail() {
    let code = "/// ```vesper,compile_fail(E0302)\n/// fn bad() { let tx = Tx {}; }\n/// ```\nlinear struct Tx {}\n";
    let out = invoke(code);
    assert!(
        out.status.success(),
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        report["report"]["examples"][0]["diagnostics"][0]["code"],
        "E0302"
    );
    assert_eq!(report["report"]["examples"][0]["diagnostics"][0]["line"], 2);
    for source in [
        code.replace("E0302", "E0301"),
        format!("{code}fn broken() -> i64 {{ unknown() }}"),
        code.replace("fn bad() { let tx = Tx {}; }", "fn bad( { ?? }"),
    ] {
        assert!(!invoke(&source).status.success());
    }
}
