//! The text reports of `vesper test` and `vesper test --doc`, behind a swap
//! point (self-hosting SH5 lane 14, #1337): the doctest lines of one file,
//! the unit-test lines and summary, the failing baseline lines of the
//! mutation gate and the mutant lines and summary.
//!
//! The Rust implementation (moved here from src/doctest_commands.rs and
//! src/test_runner.rs) is the default [`TestReportTable`].
//! `@vesper/test-runner` (packages/vesper-test-runner/src/report.ves)
//! renders the same; tests/self_host_test_report.rs runs both and requires
//! equal text.

use std::fmt::Write;

/// A diagnostic placed in a file.
#[derive(Clone, Copy, Debug)]
pub struct Diag<'a> {
    pub line: usize,
    pub column: usize,
    pub code: &'a str,
    pub message: &'a str,
}

/// One doctest example's result.
#[derive(Clone, Copy, Debug)]
pub struct DocExample<'a> {
    pub index: usize,
    pub line: usize,
    pub column: usize,
    pub passed: bool,
    pub message: &'a str,
    /// `jit`, `evaluator` or `simpulse`; `None` when nothing ran.
    pub engine: Option<&'a str>,
    pub diagnostics: &'a [Diag<'a>],
}

/// One unit test's result.
#[derive(Clone, Copy, Debug)]
pub struct UnitCase<'a> {
    pub name: &'a str,
    pub line: usize,
    pub column: usize,
    pub passed: bool,
    pub message: &'a str,
    /// `jit`, `evaluator`, `checker`, `crucible`, `simpulse` or `cache`.
    pub engine: &'a str,
    pub pin: Option<&'a str>,
    pub capsule: Option<&'a str>,
}

/// One file's unit tests, or the error that stopped them.
#[derive(Clone, Copy, Debug)]
pub struct UnitFile<'a> {
    pub display: &'a str,
    pub error: Option<&'a str>,
    pub tests: &'a [UnitCase<'a>],
}

/// What happened to one mutant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutantOutcome<'a> {
    Killed(Option<&'a str>),
    Survived,
    IllTyped,
}

/// One mutant's result.
#[derive(Clone, Copy, Debug)]
pub struct Mutant<'a> {
    pub file: &'a str,
    pub line: usize,
    pub column: usize,
    pub function: &'a str,
    pub original: &'a str,
    pub replacement: &'a str,
    pub outcome: MutantOutcome<'a>,
}

/// The mutation gate's counts.
#[derive(Clone, Copy, Debug)]
pub struct MutantCounts {
    pub killed: usize,
    pub survived: usize,
    pub ill_typed: usize,
    pub tested: usize,
    pub total: usize,
}

/// The signature of [`doc_text`].
pub type DocTextFn = fn(&str, &[Diag<'_>], &[DocExample<'_>]) -> String;
/// The signature of [`mutants_text`].
pub type MutantsTextFn = fn(&[Mutant<'_>], MutantCounts) -> String;

/// The swappable functions.
#[derive(Clone, Copy)]
pub struct TestReportTable {
    /// [`doc_text`].
    pub doc_text: DocTextFn,
    /// [`unit_text`].
    pub unit_text: fn(&[UnitFile<'_>]) -> String,
    /// [`baseline_text`].
    pub baseline_text: fn(&[UnitFile<'_>]) -> String,
    /// [`mutants_text`].
    pub mutants_text: MutantsTextFn,
}

/// The Rust implementation, the default [`TestReportTable`].
pub const RUST_TEST_REPORT: TestReportTable = TestReportTable {
    doc_text: rust_doc_text,
    unit_text: rust_unit_text,
    baseline_text: rust_baseline_text,
    mutants_text: rust_mutants_text,
};

static TABLE: std::sync::OnceLock<TestReportTable> = std::sync::OnceLock::new();

/// Replace [`RUST_TEST_REPORT`] for the rest of the process. Only the first
/// call takes effect; a later one returns its table back.
pub fn install_test_report(table: TestReportTable) -> Result<(), TestReportTable> {
    TABLE.set(table)
}

fn table() -> &'static TestReportTable {
    TABLE.get().unwrap_or(&RUST_TEST_REPORT)
}

/// The lines `vesper test --doc` prints for one file: its base diagnostics,
/// then per example a PASS/FAIL line and the example's diagnostics.
pub fn doc_text(file: &str, base: &[Diag<'_>], examples: &[DocExample<'_>]) -> String {
    (table().doc_text)(file, base, examples)
}

/// The lines `vesper test` prints: per file its error or each test's line
/// (with its pin and capsule lines), then the summary line.
pub fn unit_text(files: &[UnitFile<'_>]) -> String {
    (table().unit_text)(files)
}

/// The lines the mutation gate prints when the baseline suite fails: each
/// file error and each failing test.
pub fn baseline_text(files: &[UnitFile<'_>]) -> String {
    (table().baseline_text)(files)
}

/// The lines the mutation gate prints: each killed or surviving mutant,
/// then the summary line.
pub fn mutants_text(mutants: &[Mutant<'_>], counts: MutantCounts) -> String {
    (table().mutants_text)(mutants, counts)
}

fn diag_line(out: &mut String, file: &str, d: &Diag<'_>) {
    let _ = writeln!(
        out,
        "{file}:{}:{}: {}: {}",
        d.line, d.column, d.code, d.message
    );
}

/// The Rust implementation of [`doc_text`].
pub fn rust_doc_text(file: &str, base: &[Diag<'_>], examples: &[DocExample<'_>]) -> String {
    let mut out = String::new();
    for d in base {
        diag_line(&mut out, file, d);
    }
    for example in examples {
        let engine = example
            .engine
            .map(|e| format!(" [{e}]"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{file}:{}:{}: {} doctest {}: {}{engine}",
            example.line,
            example.column,
            if example.passed { "PASS" } else { "FAIL" },
            example.index,
            example.message
        );
        for d in example.diagnostics {
            diag_line(&mut out, file, d);
        }
    }
    out
}

/// The Rust implementation of [`unit_text`].
pub fn rust_unit_text(files: &[UnitFile<'_>]) -> String {
    let mut out = String::new();
    let total: usize = files.iter().map(|f| f.tests.len()).sum();
    let failed: usize = files
        .iter()
        .map(|f| f.tests.iter().filter(|t| !t.passed).count())
        .sum();
    let file_errors = files.iter().filter(|f| f.error.is_some()).count();
    for f in files {
        if let Some(err) = f.error {
            let _ = writeln!(out, "{}: FAIL: {err}", f.display);
            continue;
        }
        for t in f.tests {
            let _ = writeln!(
                out,
                "{}:{}:{}: {} {} ({}) [{}]",
                f.display,
                t.line,
                t.column,
                if t.passed { "PASS" } else { "FAIL" },
                t.name,
                t.message,
                t.engine
            );
            if let Some(pin) = t.pin {
                let _ = writeln!(out, "  pin regression: {pin}");
            }
            if let Some(capsule) = t.capsule {
                let _ = writeln!(out, "  replay capsule: {capsule}");
            }
        }
    }
    let _ = writeln!(
        out,
        "tests: {} passed, {} failed ({} file error(s))",
        total.saturating_sub(failed),
        failed,
        file_errors
    );
    out
}

/// The Rust implementation of [`baseline_text`].
pub fn rust_baseline_text(files: &[UnitFile<'_>]) -> String {
    let mut out = String::new();
    for f in files {
        if let Some(error) = f.error {
            let _ = writeln!(out, "{}: FAIL: {error}", f.display);
        }
        for t in f.tests {
            if !t.passed {
                let _ = writeln!(
                    out,
                    "{}:{}:{}: FAIL {}: {}",
                    f.display, t.line, t.column, t.name, t.message
                );
            }
        }
    }
    out
}

/// The Rust implementation of [`mutants_text`].
pub fn rust_mutants_text(mutants: &[Mutant<'_>], counts: MutantCounts) -> String {
    let mut out = String::new();
    for m in mutants {
        match m.outcome {
            MutantOutcome::Killed(by) => {
                let _ = writeln!(
                    out,
                    "{}:{}:{}: KILLED {} ({} -> {}) by {}",
                    m.file,
                    m.line,
                    m.column,
                    m.function,
                    m.original,
                    m.replacement,
                    by.unwrap_or("test")
                );
            }
            MutantOutcome::Survived => {
                let _ = writeln!(
                    out,
                    "{}:{}:{}: SURVIVED {} ({} -> {})",
                    m.file, m.line, m.column, m.function, m.original, m.replacement
                );
            }
            MutantOutcome::IllTyped => {}
        }
    }
    let _ = writeln!(
        out,
        "mutation gate: {} killed, {} survived, {} ill-typed (of {}/{} candidate(s))",
        counts.killed, counts.survived, counts.ill_typed, counts.tested, counts.total
    );
    out
}
