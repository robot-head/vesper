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
            "[package]\nname = \"@test/unit\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
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
fn package_tests_resolve_imports_for_cases_and_anvil_modes() {
    let temp = Temp::new(&[
        (
            "app/Loom.toml",
            "[package]\nname = \"@test/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n[dependencies]\n\"@test/dep\" = { version = \"^0.1\", path = \"../dep\" }\n",
        ),
        (
            "dep/Loom.toml",
            "[package]\nname = \"@test/dep\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        ),
        (
            "dep/src/lib.ves",
            "pub fn boundary() -> i64 { 5 } test { fn test_bad_type() -> bool { 42 } }",
        ),
        (
            "app/src/helper.ves",
            "use @test/dep::boundary; pub fn below(value: i64) -> bool { value < boundary() } pub linear struct Tx { id: i64 }",
        ),
        (
            "app/src/other.ves",
            r#"test {
    use self::helper::below;
    // @case ordinary(2, true)
    fn test_below(value: i64, expected: bool) -> bool { below(value) == expected }
}
"#,
        ),
        (
            "app/src/suite.ves",
            r#"test {
    use @test/dep::boundary as __vesper_test_case_0;
    use self::helper::{below, Tx};
    // @case boundary(__vesper_test_case_0(), false)
    // @case lower(4, true)
    fn test_below(value: i64, expected: bool) -> bool { below(value) == expected }

    @test(cases = 3)
    @test_case(value = 5)
    fn property(value: i64) -> bool { below(value) == (value < __vesper_test_case_0()) }

    @test(sim_schedules = 1)
    fn simulation() -> bool { !below(__vesper_test_case_0()) }

    @test(expect_error = E0302, primary = "tx", related = "linear binding introduced here")
    fn negative() -> i64 { let tx = Tx { id: 1 }; 0 }
}
"#,
        ),
    ]);
    let run = |args: &[&str]| {
        let out = vesper(&temp.0, args);
        let report = parse_json(&out);
        assert!(out.status.success(), "{report}");
        report
    };
    let complete = run(&["test", "app", "--no-cache", "--json"]);
    assert_eq!(complete["total"], 6);
    assert_eq!(complete["files"][0]["file"], "src/other.ves");
    assert_eq!(complete["files"][1]["file"], "src/suite.ves");
    let tests = complete["files"][1]["tests"].as_array().unwrap();
    assert_eq!(tests[0]["name"], "test_below::boundary");
    assert_eq!(tests[0]["line"], 4);
    assert_eq!(tests[0]["column"], 5);
    assert_eq!(tests[2]["anvil_mode"], "Property");
    assert_eq!(tests[3]["anvil_mode"], "SimPulse");
    assert_eq!(tests[4]["anvil_mode"], "ExpectError");

    let single = run(&["test", "app/src/other.ves", "--json"]);
    assert_eq!(single["total"], 1);
    assert_eq!(
        single["files"][0]["tests"][0]["name"],
        "test_below::ordinary"
    );

    let mutation = run(&[
        "test",
        "app",
        "--mutate",
        "--filter",
        "test_below",
        "--json",
    ]);
    assert_eq!(mutation["killed"], 2);
    assert_eq!(mutation["survived"], 0);
    for mutant in mutation["mutants"].as_array().unwrap() {
        assert_eq!(mutant["file"], "src/helper.ves");
    }
    let mutation = run(&["test", "app", "--mutate", "--filter", "property", "--json"]);
    for mutant in mutation["mutants"].as_array().unwrap() {
        assert_eq!(mutant["killed_by"], "property");
    }
}

#[test]
fn package_tests_call_vsys_from_a_path_dependency() {
    let temp = Temp::new(&[
        (
            "app/Loom.toml",
            "[package]\nname = \"@test/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[StdEnv]\"]\n[dependencies]\n\"@test/host\" = { version = \"^0.1\", path = \"../host\" }\n",
        ),
        (
            "host/Loom.toml",
            "[package]\nname = \"@test/host\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[StdEnv]\"]\nvsys = [\"env.vsys\"]\n",
        ),
        (
            "host/env.vsys",
            r#"sys library StdEnv (
    target = "@TARGET@", abi = "@ABI@",
    link = "static", name = "vesper_std_host",
    artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000"
) {
    fn vstd_env_arg_count() -> ForeignVal[u64];
}"#,
        ),
        (
            "host/src/lib.ves",
            "pub fn arg_count() -> u64 eff [Ffi[StdEnv]] { StdEnv::vstd_env_arg_count() }",
        ),
        (
            "app/src/suite.ves",
            r#"use @test/host::arg_count;

fn enough_arguments(minimum: u64) -> bool eff [Ffi[StdEnv]] {
    arg_count() >= minimum
}

test {
    linear struct Tx { id: u64 }

    // @case present(1, true)
    // @case absent(1000, false)
    fn test_host_count(minimum: u64, expected: bool) -> bool eff [Ffi[StdEnv], Fail[ArithOverflow]] {
        enough_arguments(minimum) == expected && arg_count() + 1 > 1
    }

    @test(expect_error = E0302, primary = "tx", related = "linear binding introduced here")
    fn rejects_leaked_transaction() -> u64 eff [Ffi[StdEnv]] {
        let tx = Tx { id: 1 };
        arg_count()
    }
}
"#,
        ),
    ]);
    for path in ["app", "app/src/suite.ves"] {
        let out = vesper(&temp.0, &["test", path, "--json"]);
        let report = parse_json(&out);
        assert!(out.status.success(), "{report}");
        assert_eq!(report["total"], 3);
        assert_eq!(report["files"][0]["file"], "src/suite.ves");
        let cases = report["files"][0]["tests"].as_array().unwrap();
        assert_eq!(cases[0]["name"], "test_host_count::present");
        assert_eq!(cases[1]["name"], "test_host_count::absent");
        assert_eq!(cases[0]["engine"], "evaluator");
        assert_eq!(cases[1]["engine"], "evaluator");
        assert_eq!(cases[2]["engine"], "checker");
    }

    let out = vesper(&temp.0, &["test", "app", "--fuel", "1", "--json"]);
    let report = parse_json(&out);
    assert!(!out.status.success(), "{report}");
    assert_eq!(report["failed"], 2);
    assert!(
        report["files"][0]["tests"][0]["message"]
            .as_str()
            .unwrap()
            .contains("fuel exhausted"),
        "{report}"
    );

    let out = vesper(&temp.0, &["test", "app", "--mutate", "--json"]);
    let report = parse_json(&out);
    let mutants = report["mutants"].as_array().unwrap();
    assert!(
        mutants.iter().any(|mutant| {
            mutant["status"] == "killed"
                && mutant["killed_by"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("test_host_count::"))
        }),
        "{report}"
    );

    let descriptor = temp.path("host/env.vsys");
    let source = fs::read_to_string(&descriptor).unwrap();
    fs::write(
        &descriptor,
        source.replace("vesper_std_host", "unavailable_host"),
    )
    .unwrap();
    let out = vesper(&temp.0, &["test", "app", "--json"]);
    let report = parse_json(&out);
    assert!(!out.status.success(), "{report}");
    assert!(
        report["error"].as_str().unwrap().contains("does not host"),
        "{report}"
    );
}

#[test]
fn bad_imported_case_arguments_are_reported_at_the_row() {
    let temp = Temp::new(&[
        (
            "Loom.toml",
            "[package]\nname = \"@test/arguments\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        ),
        ("src/arguments.ves", "pub fn arg() -> bool { true }"),
        (
            "src/suite.ves",
            "use self::arguments::arg;\n// @case wrong_type(arg())\nfn test_value(value: i64) -> bool { value == 1 }\n",
        ),
    ]);
    let out = vesper(&temp.0, &["test", "src/suite.ves", "--json"]);
    let report = parse_json(&out);
    assert!(!out.status.success(), "{report}");
    assert_eq!(report["files"][0]["file"], "src/suite.ves");
    let error = report["files"][0]["error"].as_str().unwrap();
    assert!(
        error.starts_with("2:1: test_value::wrong_type: 1 semantic error"),
        "{error}"
    );
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

// @case positive(5, 3, 8)
// @case zero(0, 3, 0)
// @case negative(-2, 3, 0)
fn check_add_positive(a: i64, b: i64, expected: i64) -> bool eff [Fail[ArithOverflow]] {
    add_positive(a, b) == expected
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
    for mutant in strong_json["mutants"].as_array().unwrap() {
        assert_eq!(mutant["function"], "add_positive");
        if mutant["status"] == "killed" {
            assert!(
                mutant["killed_by"]
                    .as_str()
                    .unwrap()
                    .starts_with("check_add_positive::")
            );
        }
    }

    let single = vesper(
        &temp.0,
        &[
            "test",
            "strong.ves",
            "--mutate",
            "--mutant-offset",
            "1",
            "--max-mutants",
            "1",
            "--json",
        ],
    );
    assert!(single.status.success());
    let single = parse_json(&single);
    assert_eq!(single["total_candidates"], strong_json["total_candidates"]);
    assert_eq!(single["offset"], 1);
    assert_eq!(single["tested"], 1);
    assert_eq!(single["capped"], true);
    assert_eq!(single["mutants"][0], strong_json["mutants"][1]);

    let weak_out = vesper(&temp.0, &["test", "weak.ves", "--mutate", "--json"]);
    assert!(!weak_out.status.success());
    let weak_json = parse_json(&weak_out);
    assert_eq!(weak_json["mode"], "mutate");
    assert_eq!(weak_json["passed"], false);
    assert!(weak_json["survived"].as_u64().unwrap() >= 1);
}

#[test]
fn mutation_gate_replays_tests_against_a_dependency() {
    let temp = Temp::new(&[
        (
            "app/Loom.toml",
            "[package]\nname = \"@test/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n[dependencies]\n\"@test/dep\" = { version = \"^0.1\", path = \"../dep\" }\n",
        ),
        (
            "dep/Loom.toml",
            "[package]\nname = \"@test/dep\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        ),
        ("dep/src/lib.ves", "pub fn flag() -> bool { true }"),
        (
            "app/src/lib.ves",
            "test { use @test/dep::flag; fn test_flag() -> bool { flag() } }",
        ),
    ]);
    let out = vesper(
        &temp.0,
        &[
            "test",
            "app",
            "--mutate",
            "--mutate-dependency",
            "dep/src/lib.ves",
            "--no-opt",
            "--json",
        ],
    );
    let report = parse_json(&out);
    assert!(out.status.success(), "{report}");
    assert_eq!(report["tested"], 1);
    assert_eq!(report["mutants"][0]["killed_by"], "test_flag");
    assert_eq!(
        fs::read_to_string(temp.path("dep/src/lib.ves")).unwrap(),
        "pub fn flag() -> bool { true }"
    );
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

#[test]
fn parameterized_cases_report_separate_outcomes_and_can_be_filtered() {
    let source = r#"
fn __vesper_test_case_0(a: i64, b: i64) -> i64 {
    wrapping_add(a, b)
}

// @case negative(-3, -3)
// @case nested(__vesper_test_case_0(2, 3), 5)
// @case wrong(2, 3)
fn compare(actual: i64, expected: i64) -> bool {
    actual == expected
}

// @test with an ordinary regression description
fn ordinary() -> bool { true }
"#;
    let temp = Temp::new(&[("cases.ves", source)]);
    let out = vesper(&temp.0, &["test", "cases.ves", "--json"]);
    assert!(!out.status.success());
    let report = parse_json(&out);
    assert_eq!(report["total"], 4, "{report}");
    assert_eq!(report["failed"], 1);
    let cases = report["files"][0]["tests"].as_array().unwrap();
    assert_eq!(cases[0]["name"], "compare::negative");
    assert_eq!(cases[0]["engine"], "jit");
    assert_eq!(cases[0]["passed"], true);
    assert_eq!(cases[1]["name"], "compare::nested");
    assert_eq!(cases[1]["passed"], true);
    assert_eq!(cases[2]["name"], "compare::wrong");
    assert_eq!(cases[2]["line"], 8);
    assert_eq!(cases[2]["column"], 1);
    assert_eq!(cases[2]["passed"], false);
    assert_eq!(cases[3]["name"], "ordinary");

    let out = vesper(
        &temp.0,
        &["test", "cases.ves", "--filter", "compare::nested", "--json"],
    );
    assert!(out.status.success());
    assert_eq!(parse_json(&out)["total"], 1);
}

#[test]
fn invalid_parameterized_rows_report_the_case_location() {
    for (rows, expected) in [
        (
            "// @case missing_parentheses",
            "1:1: expected @case name(arguments)",
        ),
        (
            "// @case repeat(1)\n// @case repeat(2)",
            "2:1: duplicate test case 'test_value::repeat'",
        ),
        (
            "// @case bad_syntax(,)",
            "1:1: test_value::bad_syntax: 1 syntax error",
        ),
        (
            "// @case wrong_arity()",
            "1:1: test_value::wrong_arity: 1 semantic error",
        ),
        (
            "// @case wrong_type(true)",
            "1:1: test_value::wrong_type: 1 semantic error",
        ),
    ] {
        let source = format!("{rows}\nfn test_value(x: i64) -> bool {{ x == 1 }}\n");
        let temp = Temp::new(&[("invalid.ves", &source)]);
        let out = vesper(&temp.0, &["test", "invalid.ves", "--json"]);
        assert!(!out.status.success());
        let report = parse_json(&out);
        let error = report["files"][0]["error"].as_str().unwrap();
        assert!(error.starts_with(expected), "{error}");
    }
}

fn check_source_unit_tests(suites: &[(&str, usize)]) {
    let cwd = std::env::current_dir().unwrap();
    for &(file, count) in suites {
        let out = vesper(&cwd, &["test", file, "--json", "--no-cache"]);
        let report = parse_json(&out);
        assert!(out.status.success(), "{file}: {report}");
        assert_eq!(report["total"], count, "{file}: {report}");
    }
}

#[test]
fn standard_library_core_and_byte_source_unit_tests_pass() {
    check_source_unit_tests(&[
        ("packages/vesper-std/bytes/src/ct.ves", 17),
        ("packages/vesper-std/bytes/src/lib.ves", 53),
        ("packages/vesper-std/bytes/src/views.ves", 24),
        ("packages/vesper-std/core/src/lib.ves", 23),
        ("packages/vesper-std/core/src/option.ves", 27),
        ("packages/vesper-std/core/src/result.ves", 13),
        ("packages/vesper-std/core/src/saturating.ves", 130),
        ("packages/vesper-std/fmt/src/lib.ves", 62),
    ]);
}

#[test]
fn standard_library_collection_source_unit_tests_pass() {
    check_source_unit_tests(&[
        ("packages/vesper-std/arena/src/lib.ves", 23),
        ("packages/vesper-std/collections/src/flat_table.ves", 15),
        ("packages/vesper-std/collections/src/radix_seq.ves", 16),
        ("packages/vesper-std/graph/src/util.ves", 9),
        ("packages/vesper-std/hash/src/fast.ves", 23),
        ("packages/vesper-std/intern/src/lib.ves", 36),
        ("packages/vesper-std/sort/src/unstable.ves", 28),
    ]);
}

#[test]
fn frontend_source_unit_tests_pass() {
    check_source_unit_tests(&[
        ("packages/vesper-ast/src/text.ves", 63),
        ("packages/vesper-front/src/util.ves", 36),
    ]);
}

#[test]
fn ported_crate_source_unit_tests_pass() {
    check_source_unit_tests(&[
        ("crates/vesper-crucible/vesper/src/lib.ves", 239),
        ("crates/vesper-lean-syntax/vesper/src/lib.ves", 174),
        ("crates/vesper-simpulse/vesper/src/lib.ves", 25),
    ]);
}

#[test]
fn test_blocks_are_type_checked_only_when_enabled() {
    let source = r#"
fn main() -> i64 { 0 }

test {
    fn test_bad_type() -> bool { 42 }
}
"#;
    let temp = Temp::new(&[("suite.ves", source)]);
    let check = vesper(&temp.0, &["check", "suite.ves"]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    if cfg!(unix) {
        let build = vesper(&temp.0, &["build", "suite.ves", "-o", "suite"]);
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
    }

    let out = vesper(&temp.0, &["test", "suite.ves", "--json"]);
    assert!(!out.status.success());
    let report = parse_json(&out);
    let error = report["files"][0]["error"].as_str().unwrap();
    assert!(error.contains("expected bool"), "{error}");
}

#[test]
fn test_blocks_keep_private_helpers_and_cases_in_the_module() {
    let source = r#"
fn offset(value: i64) -> i64 { if value >= 0 { 5 } else { 0 } }

test {
    fn same(actual: i64, expected: i64) -> bool { actual == expected }

    // @case negative(-2, 0)
    // @case boundary(0, 5)
    fn test_offset(value: i64, expected: i64) -> bool {
        same(offset(value), expected)
    }
}
"#;
    let temp = Temp::new(&[("suite.ves", source)]);
    let out = vesper(&temp.0, &["test", "suite.ves", "--mutate", "--json"]);
    let report = parse_json(&out);
    assert!(out.status.success(), "{report}");
    let mutants = report["mutants"].as_array().unwrap();
    assert!(!mutants.is_empty());
    for mutant in mutants {
        assert_eq!(mutant["function"], "offset", "{mutant}");
    }
}

#[test]
fn package_test_mode_only_enables_blocks_in_the_root_package() {
    let temp = Temp::new(&[
        (
            "app/Loom.toml",
            "[package]\nname = \"@test/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n[dependencies]\n\"@test/helper\" = { version = \"^0.1\", path = \"../helper\" }\n",
        ),
        (
            "app/src/main.ves",
            r#"
fn main() -> i64 { 0 }

test {
    use @test/helper::value;
    fn run_suite() -> i64 { value() }
}
"#,
        ),
        (
            "helper/Loom.toml",
            "[package]\nname = \"@test/helper\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        ),
        (
            "helper/src/lib.ves",
            "pub fn value() -> i64 { 7 } test { fn test_bad_type() -> bool { 42 } }",
        ),
    ]);
    let ordinary = vesper(&temp.0, &["run", "app", "--entry", "run_suite"]);
    assert!(!ordinary.status.success());
    let test = vesper(
        &temp.0,
        &["run", "app", "--include-tests", "--entry", "run_suite"],
    );
    assert!(
        test.status.success(),
        "{}",
        String::from_utf8_lossy(&test.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&test.stdout).trim(), "7");
}
