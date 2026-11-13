use serde_json::Value;
use std::process::Command;
use vesper::doctest::{self, DocTestLimits};
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
    let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
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
    let help = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["test", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("test --doc [FILE|DIR]"));
    for args in [
        vec!["test", "--json"],
        vec!["test", "--unknown", "--json"],
        vec!["test", "--doc", "a.ves", "b.ves", "--json"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .args(args)
            .output()
            .unwrap();
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
#[test]
fn body_unit_fences_execute_and_unsupported_execution_is_visible_failure() {
    let good = "/// ```vesper\n/// let x = answer();\n/// ```\nfn answer() -> i64 { 42 }\n";
    assert!(doctest::run(good, DocTestLimits::default()).unwrap().passed);
    for code in [
        "/// ```vesper,entry=main\n/// fn main() -> i64 { main() }\n/// ```",
        "/// ```vesper,entry=main\n/// fn main() -> i64 { host() }\n/// ```\nfn host() -> i64;",
        "/// ```vesper,no_run\n/// ()\n/// ```",
    ] {
        let out = invoke(code);
        assert!(!out.status.success());
        let json: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(json["passed"], false);
    }
}
#[test]
fn only_actual_doc_comment_fences_are_extracted_and_nonvesper_blocks_ignore() {
    let source = "// ```vesper\n// garbage\n// ```\n/// ```text\n/// ```vesper\n/// ```\n/// ~~~vesper\n/// ()\n/// ~~~\nfn base() {}";
    let report = doctest::run(source, DocTestLimits::default()).unwrap();
    assert!(report.passed);
    assert_eq!(report.examples.len(), 1);
    assert!(doctest::run("fn base() {}", DocTestLimits::default()).is_err());
    assert!(doctest::run("/// ```vesper\n/// ()", DocTestLimits::default()).is_err());
}
#[test]
fn limits_precede_recursive_parse_and_never_satisfy_compile_fail() {
    let defaults = DocTestLimits::default();
    for source in [
        format!("fn x() {{ {}1{}; }}", "(".repeat(40), ")".repeat(40)),
        format!("fn x() -> i64 {{ {}1 }}", "1 + ".repeat(200)),
    ] {
        assert!(doctest::preflight(&source, defaults).is_err());
    }
    let source = "/// ```vesper\n/// ()\n/// ```";
    for limits in [
        DocTestLimits {
            max_source_bytes: 1,
            ..defaults
        },
        DocTestLimits {
            max_tokens: 1,
            ..defaults
        },
        DocTestLimits {
            max_snippet_bytes: 1,
            ..defaults
        },
        DocTestLimits {
            max_total_check_bytes: 1,
            ..defaults
        },
    ] {
        assert!(doctest::run(source, limits).is_err());
    }
}

#[test]
fn compact_nominal_dag_expansion_and_recursive_types_fail_before_checker() {
    let mut source = "copy struct A0 { a: i64, b: i64 }\n".to_string();
    for n in 1..20 {
        source.push_str(&format!(
            "copy struct A{n} {{ a: A{}, b: A{} }}\n",
            n - 1,
            n - 1
        ));
    }
    source.push_str("/// ```vesper\n/// ()\n/// ```\nfn marker() {}\n");
    let error = doctest::run(&source, DocTestLimits::default())
        .err()
        .unwrap();
    assert!(error.contains("nominal expansion"), "{error}");
    let source = "copy enum List { End, Next(List) }\n/// ```vesper\n/// ()\n/// ```";
    assert!(
        doctest::run(source, DocTestLimits::default())
            .err()
            .unwrap()
            .contains("recursive nominal")
    );
}

#[test]
fn matching_base_code_or_mixed_snippet_codes_cannot_satisfy_compile_fail() {
    let base = "/// ```vesper,compile_fail(E0302)\n/// fn bad() { let tx = Tx {}; }\n/// ```\nlinear struct Tx {}\nfn unrelated() { let leaked = Tx {}; }";
    let report = doctest::run(base, DocTestLimits::default()).unwrap();
    assert!(!report.passed);
    assert!(report.base_diagnostics.iter().any(|d| d.code == "E0302"));
    let mixed = "/// ```vesper,compile_fail(E0302)\n/// fn bad() { let tx = Tx {}; unknown(); }\n/// ```\nlinear struct Tx {}";
    assert!(
        !doctest::run(mixed, DocTestLimits::default())
            .unwrap()
            .passed
    );
}

#[test]
fn cli_file_read_cap_reports_limit_as_structured_failure() {
    let out = invoke(&" ".repeat(DocTestLimits::default().max_source_bytes + 1));
    assert!(!out.status.success());
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["passed"], false);
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("source byte limit")
    );
}

#[test]
fn positive_entry_must_originate_in_its_own_snippet() {
    for fence in ["vesper", "vesper,entry=main"] {
        let source = format!(
            "/// ```{fence}\n/// fn unused() -> i64 {{ 9 }}\n/// ```\nfn main() -> i64 {{ 7 }}"
        );
        let report = doctest::run(&source, DocTestLimits::default()).unwrap();
        assert!(!report.passed);
        assert!(report.batch.is_none());
        assert!(
            report.examples[0]
                .message
                .contains("defined by this snippet")
        );
    }
}

#[test]
fn wrapper_names_and_contextual_declarations_cannot_create_expected_errors() {
    for declaration in ["trait T {}", "pub trait T {}"] {
        let source = format!("/// ```vesper,compile_fail(E0002)\n/// {declaration}\n/// ```");
        assert!(
            doctest::run(&source, DocTestLimits::default())
                .map(|r| !r.passed)
                .unwrap_or(true)
        );
    }
    let source = "/// ```vesper,compile_fail(E0100)\n/// ()\n/// ```\nfn __vesper_doctest_0() {}";
    assert!(
        !doctest::run(source, DocTestLimits::default())
            .unwrap()
            .passed
    );
}

#[test]
fn parsed_guard_rejects_inferred_composite_growth_without_invoking_checker() {
    let limits = DocTestLimits::default();
    let mut tuple = "fn main() { let x = 0;".to_string();
    for _ in 0..25 {
        tuple.push_str("let x = (x, x);");
    }
    tuple.push('}');
    let generic =
        "fn dup[T](x: T) -> (T,T) { (x,x) } fn main() { let x = 0; let x = dup(dup(dup(x))); }";
    let constructors = "fn main() { let x = 0; let x = if true { Ok(x) } else { Err(x) }; }";
    for source in [&tuple, generic, constructors] {
        doctest::preflight(source, limits).unwrap();
        let parsed = vesper::parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        // source_check_guard never calls check::check: the 25-level inferred
        // tuple therefore cannot enter the exponential checker in this test.
        let error = doctest::source_check_guard(&parsed.module, limits).unwrap_err();
        assert!(error.contains("inferred composite growth"), "{error}");
    }
    let integrated = "/// ```vesper\n/// let x = (0,0);\n/// ```\nfn main() {}";
    assert!(
        doctest::run(integrated, limits)
            .err()
            .unwrap()
            .contains("inferred composite growth")
    );
}

#[test]
fn completed_subtree_and_completed_statement_spines_fail_before_parsing() {
    for semicolon in [false, true] {
        let mut expr = "0".to_string();
        for _ in 0..28 {
            expr = if semicolon {
                format!("{{{expr};0}}{}", "+1".repeat(60))
            } else {
                format!("{{{expr}}}{}", "+1".repeat(60))
            };
        }
        let source = format!("fn main() -> i64 {{ {expr} }}");
        assert!(doctest::preflight(&source, DocTestLimits::default()).is_err());
    }
}

#[test]
fn flat_nongeneric_linear_cleanup_still_reaches_actual_checker() {
    let source = "linear struct Tx {} fn main() { let tx = Tx {}; let Tx {} = tx; }";
    let limits = DocTestLimits::default();
    doctest::preflight(source, limits).unwrap();
    let parsed = vesper::parser::parse(source);
    assert!(parsed.diagnostics.is_empty());
    doctest::source_check_guard(&parsed.module, limits).unwrap();
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}
