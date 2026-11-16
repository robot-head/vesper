use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new(files: &[(&str, &str)]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-test-runner-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        for (rel, content) in files {
            let path = dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        Self(dir)
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn parse_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid json ({e})\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

const SUITE_SOURCE: &str = r#"
copy enum Outcome {
    Ok(i64),
    Err(i64),
}

fn clamp_nonneg(x: i64, upper: i64) -> i64 {
    if x < 0 {
        0
    } else if x > upper {
        upper
    } else {
        x
    }
}

fn is_within_limit(x: i64, limit: i64) -> bool {
    x >= 0 && x <= limit
}

fn test_clamp_scalar_jit() -> bool {
    clamp_nonneg(-5, 10) == 0 && clamp_nonneg(25, 10) == 10 && clamp_nonneg(4, 10) == 4
}

fn test_limit_bounds() -> bool {
    is_within_limit(0, 5)
        && is_within_limit(5, 5)
        && !is_within_limit(-1, 5)
        && !is_within_limit(6, 5)
}

// @test regression: synthetic regression case
fn regression_outcome_ok() -> Outcome {
    Outcome::Ok(clamp_nonneg(7, 10))
}

fn test_unit_pass() {
    let _v = clamp_nonneg(3, 10);
}
"#;

#[test]
fn unit_tests_discover_bool_unit_and_at_test_markers_across_jit_and_evaluator() {
    let temp = Temp::new(&[("suite.ves", SUITE_SOURCE)]);
    let file = temp.path("suite.ves");
    let out = vesper(&temp.0, &["test", file.to_str().unwrap(), "--json"]);
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let report = parse_json(&out);
    assert_eq!(report["mode"], "unit");
    assert_eq!(report["passed"], true);
    assert_eq!(report["total"], 4);
    assert_eq!(report["failed"], 0);
    let tests = report["files"][0]["tests"].as_array().unwrap();
    assert_eq!(tests[0]["name"], "test_clamp_scalar_jit");
    assert_eq!(tests[0]["engine"], "jit");
    assert_eq!(tests[1]["name"], "test_limit_bounds");
    assert_eq!(tests[1]["engine"], "jit");
    assert_eq!(tests[2]["name"], "regression_outcome_ok");
    assert_eq!(tests[2]["engine"], "evaluator");
    assert_eq!(tests[3]["name"], "test_unit_pass");

    // Plain text output.
    let text = vesper(&temp.0, &["test", "suite.ves"]);
    assert!(text.status.success());
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("PASS test_clamp_scalar_jit"), "{stdout}");
    assert!(stdout.contains("4 passed, 0 failed"), "{stdout}");
}

#[test]
fn unit_test_failures_and_filters_report_structured_outcomes() {
    let source = r#"
copy enum Outcome {
    Ok(i64),
    Err(i64),
}

fn test_false_fails() -> bool {
    1 == 2
}

fn test_err_fails() -> Outcome {
    Outcome::Err(99)
}

fn test_passing() -> bool {
    true
}
"#;
    let temp = Temp::new(&[("fails.ves", source)]);
    let out = vesper(&temp.0, &["test", "fails.ves", "--json"]);
    assert!(!out.status.success());
    let report = parse_json(&out);
    assert_eq!(report["passed"], false);
    assert_eq!(report["total"], 3);
    assert_eq!(report["failed"], 2);
    let tests = report["files"][0]["tests"].as_array().unwrap();
    assert_eq!(tests[0]["passed"], false);
    assert_eq!(tests[1]["passed"], false);
    assert_eq!(tests[2]["passed"], true);

    // Filter selects only the passing test.
    let filtered = vesper(
        &temp.0,
        &["test", "fails.ves", "--filter", "passing", "--json"],
    );
    assert!(filtered.status.success());
    assert_eq!(parse_json(&filtered)["total"], 1);

    // Filter matching zero tests fails explicitly.
    let empty_filter = vesper(
        &temp.0,
        &["test", "fails.ves", "--filter", "nonexistent", "--json"],
    );
    assert!(!empty_filter.status.success());
    assert_eq!(parse_json(&empty_filter)["passed"], false);
}

#[test]
fn directory_discovery_uses_loom_manifest_and_skips_non_test_helpers() {
    let temp = Temp::new(&[
        (
            "Loom.toml",
            "[package]\nname = \"@test/unit\"\nversion = \"0.1.0\"\n",
        ),
        (
            "src/helpers.ves",
            "fn helper(x: i64) -> i64 { wrapping_add(x, 1) }\n",
        ),
        ("src/suite.ves", SUITE_SOURCE),
    ]);
    let out = vesper(&temp.0, &["test", temp.0.to_str().unwrap(), "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let report = parse_json(&out);
    assert_eq!(report["passed"], true);
    assert_eq!(report["files"].as_array().unwrap().len(), 1);
    assert_eq!(report["files"][0]["file"], "src/suite.ves");
}

#[test]
fn mutation_gate_kills_strong_suite_and_fails_on_surviving_mutant() {
    let strong = r#"
fn add_positive(a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] {
    if a > 0 {
        a + b
    } else {
        0
    }
}

fn test_add_positive() -> bool eff [Fail[ArithOverflow]] {
    add_positive(5, 3) == 8 && add_positive(0, 3) == 0 && add_positive(-2, 3) == 0
}
"#;
    let weak = r#"
fn add_positive(a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] {
    if a > 0 {
        a + b
    } else {
        0
    }
}

fn test_add_positive_weak() -> bool eff [Fail[ArithOverflow]] {
    add_positive(5, 3) == 8
}
"#;
    let temp = Temp::new(&[("strong.ves", strong), ("weak.ves", weak)]);

    let strong_out = vesper(&temp.0, &["test", "strong.ves", "--mutate", "--json"]);
    assert!(
        strong_out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&strong_out.stdout),
        String::from_utf8_lossy(&strong_out.stderr)
    );
    let strong_json = parse_json(&strong_out);
    assert_eq!(strong_json["mode"], "mutate");
    assert_eq!(strong_json["passed"], true);
    assert_eq!(strong_json["survived"], 0);
    assert!(strong_json["killed"].as_u64().unwrap() >= 3);

    let weak_out = vesper(&temp.0, &["test", "weak.ves", "--mutate", "--json"]);
    assert!(!weak_out.status.success());
    let weak_json = parse_json(&weak_out);
    assert_eq!(weak_json["mode"], "mutate");
    assert_eq!(weak_json["passed"], false);
    assert!(weak_json["survived"].as_u64().unwrap() >= 1);
}

#[test]
fn mutation_gate_validates_baseline_with_same_fuel_and_engine() {
    let source = r#"
fn unused_helper() -> bool {
    true
}

fn test_ok() -> bool {
    true
}
"#;
    let temp = Temp::new(&[("fuel.ves", source)]);
    let out = vesper(
        &temp.0,
        &["test", "fuel.ves", "--mutate", "--fuel", "1", "--json"],
    );
    assert!(
        !out.status.success(),
        "baseline fuel exhaustion must fail before mutant classification: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let report = parse_json(&out);
    assert_eq!(report["mode"], "mutate");
    assert_eq!(report["passed"], false);
    assert_eq!(
        report["error"],
        "baseline unit tests failed before mutation testing"
    );
    assert!(report.get("mutants").is_none());
}

#[test]
fn mutation_gate_collects_and_tests_trait_impl_and_model_methods() {
    let strong = r#"
copy struct Pair { a: i64, b: i64 }

trait Check {
    fn valid(in self) -> bool;
}

impl Check for Pair {
    fn valid(in self) -> bool {
        self.a > 0
    }
}

model Strict: Check for Pair {
    fn valid(in self) -> bool {
        self.a > 0 && self.b > 0
    }
}

fn test_canonical_and_model() -> bool {
    let p1 = Pair { a: 1, b: 0 };
    let p2 = Pair { a: 0, b: 1 };
    let p3 = Pair { a: 1, b: 1 };
    let canon_ok = p1.valid() && !p2.valid();
    use model Strict;
    canon_ok && p3.valid() && !p1.valid() && !p2.valid()
}
"#;
    let weak = r#"
copy struct Pair { a: i64, b: i64 }

trait Check {
    fn valid(in self) -> bool;
}

impl Check for Pair {
    fn valid(in self) -> bool {
        self.a > 0
    }
}

model Strict: Check for Pair {
    fn valid(in self) -> bool {
        self.a > 0 && self.b > 0
    }
}

fn test_only_positive() -> bool {
    let p = Pair { a: 1, b: 1 };
    let canon = p.valid();
    use model Strict;
    canon && p.valid()
}
"#;
    let temp = Temp::new(&[("strong.ves", strong), ("weak.ves", weak)]);

    let strong_out = vesper(&temp.0, &["test", "strong.ves", "--mutate", "--json"]);
    assert!(
        strong_out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&strong_out.stdout),
        String::from_utf8_lossy(&strong_out.stderr)
    );
    let strong_json = parse_json(&strong_out);
    assert_eq!(strong_json["mode"], "mutate");
    assert_eq!(strong_json["passed"], true);
    assert_eq!(strong_json["survived"], 0);
    assert!(strong_json["total_candidates"].as_u64().unwrap() >= 6);
    assert!(strong_json["killed"].as_u64().unwrap() >= 6);

    let weak_out = vesper(&temp.0, &["test", "weak.ves", "--mutate", "--json"]);
    assert!(!weak_out.status.success());
    let weak_json = parse_json(&weak_out);
    assert_eq!(weak_json["mode"], "mutate");
    assert_eq!(weak_json["passed"], false);
    assert!(weak_json["survived"].as_u64().unwrap() >= 1);
}
