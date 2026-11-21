use serde::Serialize;
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};
use vesper::{
    anvil_hash::{FunctionGraph, VerdictCache},
    anvil_sim::{
        format_value_literal, run_property_test, run_simpulse_test_on_world, write_vchrono_capsule,
    },
    ast::{Diagnostic, Expr, ExprKind, Function, Item, Literal, Module, Span, Statement, Type},
    check::{self, World},
    ffi_lowering::{self, EvalHost, FfiError, ForeignChecked, Imports},
    lexer::{self, TokenKind as K},
    native::batch::{BatchLimits, NativeBatch},
    packages::{self, Assembled, SourceMap},
    parser,
    runtime::{Runtime, Value},
    test_attrs::{self, AttrValue},
    test_discovery::{
        self, CaseRow, CaseRows, case_marker, is_test_marker, location, preceding_comments,
    },
    tooling::doc::sources::{SourceLimits, discover},
};
#[path = "test_engines.rs"]
mod test_engines;

use vesper_test::{
    ExpectErrorOutcome, FaultProfile, HarnessPlan, ObligationLedger, ObservedDiagnostic, TestMode,
    VerdictKey, VerdictRecord, VerdictStatus, is_cacheable, verify_expect_error,
};

const MAX_SOURCE_BYTES: usize = 512 * 1024;
const DEFAULT_FUEL: u64 = 1_000_000;
const DEFAULT_MAX_MUTANTS: usize = 256;
const HARD_MAX_MUTANTS: usize = 2048;

pub const USAGE: &str = "usage: vesper test --doc [FILE|DIR] [--json]\n       vesper test <FILE|DIR> [--mutate] [--mutate-dependency <FILE>] [--filter <PAT>] [--mutant-offset <N>] [--max-mutants <N>] [--no-opt] [--fuel <N>] [--engines all|unit] [--no-cache] [--json]";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestEngine {
    Jit,
    Evaluator,
    Checker,
    Crucible,
    SimPulse,
    Cache,
}

#[derive(Debug, Serialize)]
pub struct UnitTestCaseReport {
    pub name: String,
    pub line: usize,
    pub column: usize,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    pub message: String,
    pub engine: TestEngine,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anvil_mode: Option<TestMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_case_pin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vchrono_capsule: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UnitTestFileReport {
    pub file: String,
    pub passed: bool,
    pub tests: Vec<UnitTestCaseReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MutantReport {
    pub index: usize,
    pub file: String,
    pub function: String,
    pub line: usize,
    pub column: usize,
    pub original: String,
    pub replacement: String,
    pub status: MutantStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub killed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutantStatus {
    Killed,
    Survived,
    IllTyped,
}

struct Options {
    path: String,
    mutate: bool,
    mutate_dependency: Option<PathBuf>,
    optimize: bool,
    filter: Option<String>,
    max_mutants: usize,
    mutant_offset: usize,
    fuel: u64,
    no_cache: bool,
    json: bool,
    /// `--engines all`: every passing unit test must pass on every engine
    /// ([`test_engines::agree`]).
    engines_all: bool,
}

impl Options {
    fn selects(&self, test: &DiscoveredTest) -> bool {
        self.filter
            .as_ref()
            .is_none_or(|pat| test.name.contains(pat.as_str()))
    }
}

fn parse_u64(text: &str, flag: &str) -> Result<u64, String> {
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    if let Some(hex) = digits.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        digits.parse::<u64>()
    }
    .map_err(|_| format!("{flag} requires a positive integer"))
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut path = None;
    let mut mutate = false;
    let mut mutate_dependency = None;
    let mut optimize = true;
    let mut filter = None;
    let mut max_mutants = None;
    let mut mutant_offset = None;
    let mut fuel = DEFAULT_FUEL;
    let mut no_cache = false;
    let mut json = false;
    let mut engines = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--mutate" if !mutate => mutate = true,
            "--no-opt" if optimize => optimize = false,
            "--mutate-dependency" if mutate_dependency.is_none() => {
                let path = it.next().ok_or("--mutate-dependency requires a file")?;
                mutate_dependency = Some(PathBuf::from(path));
            }
            "--no-cache" if !no_cache => no_cache = true,
            "--json" if !json => json = true,
            "--engines" if engines.is_none() => {
                let value = it.next().ok_or("--engines requires `all` or `unit`")?;
                engines = Some(test_engines::parse(value)?);
            }
            "--filter" => {
                if filter.is_some() {
                    return Err("duplicate --filter option".into());
                }
                let pat = it.next().ok_or("--filter requires a pattern")?;
                filter = Some(pat.clone());
            }
            "--max-mutants" => {
                if max_mutants.is_some() {
                    return Err("duplicate --max-mutants option".into());
                }
                let val = it
                    .next()
                    .ok_or("--max-mutants requires a positive integer")?;
                let parsed = parse_u64(val, "--max-mutants")?;
                let n = usize::try_from(parsed).map_err(|_| "--max-mutants is too large")?;
                if n == 0 || n > HARD_MAX_MUTANTS {
                    return Err(format!(
                        "--max-mutants must be between 1 and {HARD_MAX_MUTANTS}"
                    ));
                }
                max_mutants = Some(n);
            }
            "--mutant-offset" => {
                if mutant_offset.is_some() {
                    return Err("duplicate --mutant-offset option".into());
                }
                let value = it.next().ok_or("--mutant-offset requires an integer")?;
                let n = parse_u64(value, "--mutant-offset")?;
                mutant_offset =
                    Some(usize::try_from(n).map_err(|_| "--mutant-offset is too large")?);
            }
            "--fuel" => {
                let val = it.next().ok_or("--fuel requires a positive integer")?;
                let n = parse_u64(val, "--fuel")?;
                if n == 0 {
                    return Err("--fuel must be positive".into());
                }
                fuel = n;
            }
            value if !value.starts_with('-') && path.is_none() => path = Some(value.to_string()),
            _ => return Err(USAGE.into()),
        }
    }
    if max_mutants.is_some() && !mutate {
        return Err("--max-mutants requires --mutate".into());
    }
    if mutant_offset.is_some() && !mutate {
        return Err("--mutant-offset requires --mutate".into());
    }
    if mutate_dependency.is_some() && !mutate {
        return Err("--mutate-dependency requires --mutate".into());
    }
    if !optimize && !mutate {
        return Err("--no-opt requires --mutate".into());
    }
    let engines_all = engines.unwrap_or(false);
    if engines_all && mutate {
        return Err("--engines all cannot be combined with --mutate".into());
    }
    let path = path.ok_or_else(|| USAGE.to_string())?;
    Ok(Options {
        path,
        mutate,
        mutate_dependency,
        optimize,
        filter,
        max_mutants: max_mutants.unwrap_or(DEFAULT_MAX_MUTANTS),
        mutant_offset: mutant_offset.unwrap_or(0),
        fuel,
        no_cache,
        json,
        engines_all,
    })
}

fn read_source(file: &Path) -> Result<String, String> {
    let name = file.display();
    if !std::fs::metadata(file)
        .map_err(|e| format!("{name}: {e}"))?
        .is_file()
    {
        return Err(format!("{name}: test input must be a regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let input = options.open(file).map_err(|e| format!("{name}: {e}"))?;
    if !input.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("{name}: test input must be a regular file"));
    }
    let mut bytes = Vec::new();
    input
        .take((MAX_SOURCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "{name}: exceeds {MAX_SOURCE_BYTES}-byte test source limit"
        ));
    }
    std::str::from_utf8(&bytes)
        .map(|s| s.to_string())
        .map_err(|e| format!("{name}: {e}"))
}

struct TestCase {
    name: String,
    span: Span,
    wrapper: CaseRow,
}

impl TestCase {
    fn error(&self, source: &str, message: &str) -> String {
        let (line, column) = location(source, self.span.start);
        format!("{line}:{column}: {}: {message}", self.name)
    }
}

fn prepare_tests(source: &mut String) -> Result<(parser::Parse, Vec<TestCase>), String> {
    let parsed = parser::parse_for_tests(source);
    if let Some(d) = parsed.diagnostics.first() {
        return Err(test_diagnostic(
            source,
            &[],
            d,
            "syntax",
            parsed.diagnostics.len(),
        ));
    }
    let CaseRows { rows, expanded } = test_discovery::case_rows(source, &parsed.module)?;
    let tests: Vec<TestCase> = rows
        .into_iter()
        .map(|row| TestCase {
            name: row.name.clone(),
            span: row.span.clone(),
            wrapper: row,
        })
        .collect();
    let Some(expanded) = expanded else {
        return Ok((parsed, tests));
    };

    // Reparse with the original module in scope so literals, constants and
    // argument expressions follow the same rules as ordinary calls.
    let mut lowered = parser::parse_for_tests(&expanded);
    if let Some(d) = lowered.diagnostics.first() {
        return Err(test_diagnostic(
            source,
            &tests,
            d,
            "syntax",
            lowered.diagnostics.len(),
        ));
    }
    let first_wrapper = parsed.module.items.len();
    for (test, index) in tests.iter().zip(first_wrapper..) {
        let wrapper = &test.wrapper;
        let Item::Function(target) = &parsed.module.items[wrapper.function_index] else {
            unreachable!("only functions can declare cases");
        };
        // A row must parse as one direct call, contained in its own wrapper.
        let Some(Item::Function(f)) = lowered.module.items.get_mut(index) else {
            return Err(test.error(source, "expected a single argument list"));
        };
        let valid_body = test_discovery::is_case_wrapper_body(f, &target.name);
        if f.span != wrapper.generated || f.name != wrapper.entry || !valid_body {
            return Err(test.error(source, "expected a single argument list"));
        }
        f.result = target.result.clone();
        f.effects = target.effects.clone();
    }
    *source = expanded;
    Ok((lowered, tests))
}

fn test_diagnostic(
    source: &str,
    tests: &[TestCase],
    diagnostic: &Diagnostic,
    kind: &str,
    count: usize,
) -> String {
    let message = format!(
        "{count} {kind} error(s); first: {} {}",
        diagnostic.code, diagnostic.message
    );
    if let Some(test) = tests.iter().find(|test| {
        let span = &test.wrapper.generated;
        span.start <= diagnostic.span.start && diagnostic.span.start <= span.end
    }) {
        return test.error(source, &message);
    }
    message
}

struct DiscoveredTest {
    name: String,
    span: Span,
    plan: HarnessPlan,
    raw_cases: Vec<test_attrs::TestCaseAttr>,
    func: Function,
}

/// The argument rows of a test's `@test_case(...)` pins, or a message naming
/// the first pin that does not fit the function's parameters.
fn eval_test_case_args(
    func: &Function,
    raw_cases: &[test_attrs::TestCaseAttr],
) -> Result<Vec<Vec<Value>>, String> {
    let mut out = Vec::new();
    for (pin, tc) in raw_cases.iter().enumerate() {
        let mut row: Vec<Option<Value>> = vec![None; func.params.len()];
        let mut ok = true;
        for (idx, arg) in tc.args.iter().enumerate() {
            let slot = match &arg.name {
                None => idx,
                Some(name) => {
                    if let Some(pos) = func.params.iter().position(|p| p.name == *name) {
                        pos
                    } else {
                        ok = false;
                        break;
                    }
                }
            };
            if slot >= row.len() {
                ok = false;
                break;
            }
            let parsed_val = match &arg.value {
                AttrValue::Int(n) => {
                    let unsigned_bits = match &func.params[slot].ty {
                        Type::Named(t, _) => match t.as_str() {
                            "u8" => Some(8),
                            "u16" => Some(16),
                            "u32" => Some(32),
                            "u64" | "usize" => Some(64),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(bits) = unsigned_bits {
                        Value::UInt(*n as u64, bits)
                    } else {
                        Value::Int(*n as i64)
                    }
                }
                AttrValue::Bool(b) => Value::Bool(*b),
                AttrValue::Float(f) => match f.parse::<f64>() {
                    Ok(v) => Value::Float(v),
                    Err(_) => {
                        ok = false;
                        break;
                    }
                },
                _ => {
                    ok = false;
                    break;
                }
            };
            row[slot] = Some(parsed_val);
        }
        if !ok || row.iter().any(|v| v.is_none()) {
            let names: Vec<&str> = func.params.iter().map(|p| p.name.as_str()).collect();
            return Err(format!(
                "invalid @test_case #{} for '{}': it must give one supported literal for each parameter ({}), by position or by name",
                pin + 1,
                func.name,
                names.join(", ")
            ));
        }
        out.push(row.into_iter().map(|v| v.unwrap()).collect());
    }
    Ok(out)
}

/// The function hash together with everything else that decides a verdict:
/// the harness plan (mode, case count, gates, `expect_error`), the explicit
/// `@test_case` rows and the fuel limit. A change to any of them is a new
/// cache key, so an edited test never reuses an earlier verdict.
/// The gates a budget-gated plan asks for, for its "not enforced" report.
fn budget_gate_names(plan: &HarnessPlan) -> String {
    let mut names = Vec::new();
    let gate = &plan.budget;
    if gate.max_stack.is_some() {
        names.push("max_stack");
    }
    if gate.max_cycles.is_some() {
        names.push("max_cycles");
    }
    if gate.zero_alloc {
        names.push("zero_alloc");
    }
    if names.is_empty() {
        "its budget".into()
    } else {
        names.join(", ")
    }
}

fn test_fingerprint(fn_hash: &str, test: &DiscoveredTest, fuel: u64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"vesper-anvil-test-fingerprint-v1\0");
    hasher.update(fn_hash.as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:?}", test.plan).as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:?}", test.raw_cases).as_bytes());
    hasher.update(b"\0");
    hasher.update(&fuel.to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

fn discover_tests(
    source: &str,
    items: &[Item],
    cases: &[TestCase],
) -> Result<Vec<DiscoveredTest>, String> {
    let rows: Vec<CaseRow> = cases.iter().map(|case| case.wrapper.clone()).collect();
    let found = test_discovery::discover(source, items, &rows)?;
    Ok(found
        .into_iter()
        .map(|test| {
            let Item::Function(f) = &items[test.index] else {
                unreachable!("only functions are tests");
            };
            DiscoveredTest {
                name: test.name,
                span: test.span,
                plan: test.plan,
                raw_cases: test.raw_cases,
                func: (**f).clone(),
            }
        })
        .collect())
}

fn evaluate_outcome(result: Result<Value, String>) -> (bool, Option<String>, String) {
    match result {
        Ok(Value::Bool(true)) => (true, Some("true".into()), "ok".into()),
        Ok(Value::Bool(false)) => (
            false,
            Some("false".into()),
            "assertion failed: test returned false".into(),
        ),
        Ok(Value::Data {
            ref constructor, ..
        }) if constructor == "Err" => {
            let rendered = result.as_ref().expect("ok").to_string();
            (
                false,
                Some(rendered.clone()),
                format!("test returned {rendered}"),
            )
        }
        Ok(val) => {
            let rendered = val.to_string();
            (true, Some(rendered), "ok".into())
        }
        Err(err) => (false, None, err),
    }
}

struct UnitEntry {
    name: String,
    entry: String,
    span: Span,
}

impl From<&DiscoveredTest> for UnitEntry {
    fn from(test: &DiscoveredTest) -> Self {
        Self {
            name: test.name.clone(),
            entry: test.func.name.clone(),
            span: test.span.clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UnitExecution {
    Standard,
    MutationBaseline,
    Mutant,
}

fn run_tests_on_world(
    source: &str,
    world: &World,
    selected: &[UnitEntry],
    fuel: u64,
    execution: UnitExecution,
    foreign: Option<(&ForeignChecked, &EvalHost)>,
    optimize: bool,
) -> (bool, Vec<UnitTestCaseReport>) {
    let prefer_jit = execution == UnitExecution::Standard;
    let mut fallback: BTreeMap<&str, String> = Default::default();
    let mut batch = None;
    if foreign.is_none() && prefer_jit && !selected.is_empty() {
        let names: Vec<&str> = selected.iter().map(|test| test.entry.as_str()).collect();
        match NativeBatch::compile(world, &names, BatchLimits::default()) {
            Ok(b) => batch = Some(b),
            Err(_) => {
                let mut covered = Vec::new();
                for name in &names {
                    match NativeBatch::compile(world, &[name], BatchLimits::default()) {
                        Ok(_) => covered.push(*name),
                        Err(reason) => {
                            fallback.insert(name, reason);
                        }
                    }
                }
                if !covered.is_empty() {
                    match NativeBatch::compile(world, &covered, BatchLimits::default()) {
                        Ok(b) => batch = Some(b),
                        Err(reason) => {
                            for name in covered {
                                fallback.insert(name, format!("shared module: {reason}"));
                            }
                        }
                    }
                }
            }
        }
    } else if foreign.is_none() {
        for test in selected {
            fallback.insert(test.entry.as_str(), "mutation gate uses evaluator".into());
        }
    }

    let ledger = ObligationLedger::new();
    let mut evaluator = None;
    let mut cases = Vec::with_capacity(selected.len());
    let mut all_passed = true;
    for test in selected {
        let entry = test.entry.as_str();
        let (line, column) = location(source, test.span.start);
        let ((mut passed, result, mut message, engine, fallback_reason), ledger_report) = ledger
            .audit_frame(|| {
                if let Some((checked, host)) = foreign {
                    let (passed, result, message) = evaluate_outcome(
                        checked
                            .evaluate_with_options(entry, Some(fuel), optimize, |call| {
                                host.call(call)
                            })
                            .map_err(|error| error.to_string()),
                    );
                    (passed, result, message, TestEngine::Evaluator, None)
                } else if let Some(reason) = fallback.get(entry) {
                    let runtime = evaluator.get_or_insert_with(|| Runtime::new(world));
                    runtime.fuel = fuel;
                    runtime.optimize = optimize;
                    let (passed, result, message) = evaluate_outcome(runtime.run(entry, vec![]));
                    (
                        passed,
                        result,
                        message,
                        TestEngine::Evaluator,
                        prefer_jit.then(|| reason.clone()),
                    )
                } else {
                    let b = batch.as_mut().expect("jit batch available");
                    let (passed, result, message) = evaluate_outcome(b.run(entry));
                    (passed, result, message, TestEngine::Jit, None)
                }
            });
        if ledger_report.has_leaks() {
            passed = false;
            message = format!(
                "obligation leak detected ({} leaked obligation(s))",
                ledger_report.total_leaks()
            );
        }
        if !passed {
            all_passed = false;
        }
        cases.push(UnitTestCaseReport {
            name: test.name.clone(),
            line,
            column,
            passed,
            result,
            message,
            engine,
            fallback_reason,
            anvil_mode: None,
            cached: None,
            test_case_pin: None,
            vchrono_capsule: None,
        });
        if !passed && execution == UnitExecution::Mutant {
            break;
        }
    }
    (all_passed, cases)
}

/// `--engines all` on one passing unit test: fails the report when another
/// engine disagrees (tests with foreign imports run on the evaluator only
/// and are left as they are).
fn engines_agree(
    rep: &mut UnitTestCaseReport,
    selected: &[UnitEntry],
    world: &World,
    foreign: Option<(&ForeignChecked, &EvalHost)>,
    source: &str,
    fuel: u64,
) {
    if foreign.is_some() {
        return;
    }
    let Some(entry) = selected.iter().find(|test| test.name == rep.name) else {
        return;
    };
    match test_engines::agree(world, &entry.entry, source, fuel) {
        Ok(()) => rep.message = format!("{} on every engine", rep.message),
        Err(message) => {
            rep.passed = false;
            rep.message = format!("--engines all: {message}");
        }
    }
}

fn without_negative_tests(mut module: Module, negative: &BTreeSet<String>) -> Module {
    module.items.retain(|item| match item {
        Item::Function(f) => !negative.contains(&f.name),
        _ => true,
    });
    module
}

fn checked_module(module: &Module, negative: BTreeSet<String>) -> Cow<'_, Module> {
    // Intentional checker failures never poison other tests in the module.
    if negative.is_empty() {
        Cow::Borrowed(module)
    } else {
        Cow::Owned(without_negative_tests(module.clone(), &negative))
    }
}

fn checked_for_mutation(
    source: &str,
    negative: &BTreeSet<String>,
) -> Result<check::Checked, String> {
    let (parsed, cases) = prepare_tests(&mut source.to_owned())?;
    let world = check::check(&without_negative_tests(parsed.module, negative));
    if let Some(diagnostic) = world.diagnostics.first() {
        return Err(test_diagnostic(
            source,
            &cases,
            diagnostic,
            "semantic",
            world.diagnostics.len(),
        ));
    }
    Ok(world)
}

struct FileUnitTests {
    display: String,
    source: String,
    report: UnitTestFileReport,
    selected: Vec<UnitEntry>,
    /// Property and SimPulse tests that passed on the baseline, replayed
    /// against every well-typed mutant after the unit tests.
    replay: Vec<ReplayTest>,
    /// Every test function of the file, whatever its mode or the filter:
    /// mutation testing never mutates these.
    test_names: BTreeSet<String>,
    expect_error_names: BTreeSet<String>,
}

impl FileUnitTests {
    fn failed(display: String, source: String, error: String) -> Self {
        Self {
            display: display.clone(),
            source,
            report: UnitTestFileReport {
                file: display,
                passed: false,
                tests: vec![],
                error: Some(error),
            },
            selected: vec![],
            replay: vec![],
            test_names: BTreeSet::new(),
            expect_error_names: BTreeSet::new(),
        }
    }
}

/// A non-unit test the mutation gate replays against each mutant.
#[derive(Clone)]
enum ReplayTest {
    Property {
        name: String,
        entry: String,
        cases: usize,
        explicit: Vec<Vec<Value>>,
    },
    SimPulse {
        name: String,
        entry: String,
        schedules: usize,
        profile: String,
        seed: u64,
    },
}

impl ReplayTest {
    /// Runs the test on a mutant's world; `Err` names the test and why it failed.
    fn run(&self, world: &World, fuel: u64) -> Result<(), (String, String)> {
        match self {
            ReplayTest::Property {
                name,
                entry,
                cases,
                explicit,
            } => {
                let outcome = run_property_test(world, entry, *cases, explicit, fuel);
                if outcome.passed {
                    Ok(())
                } else {
                    Err((
                        name.clone(),
                        outcome
                            .failure_message
                            .unwrap_or_else(|| "property failed".into()),
                    ))
                }
            }
            ReplayTest::SimPulse {
                name,
                entry,
                schedules,
                profile,
                seed,
            } => {
                let outcome = run_simpulse_test_on_world(world, entry, *schedules, profile, *seed);
                if outcome.passed {
                    Ok(())
                } else {
                    Err((name.clone(), "SimPulse schedule failed".into()))
                }
            }
        }
    }
}

fn collect_files(target: &Path) -> Result<Vec<(String, String)>, String> {
    if std::fs::metadata(target).is_ok_and(|m| m.is_dir()) {
        let package = discover(target, SourceLimits::default())?;
        let mut out = Vec::with_capacity(package.files.len());
        for path in &package.files {
            out.push((package.display(path), read_source(path)?));
        }
        Ok(out)
    } else {
        Ok(vec![(target.display().to_string(), read_source(target)?)])
    }
}

struct PackageTarget {
    root: PathBuf,
    file: Option<PathBuf>,
}

impl PackageTarget {
    fn discover(target: &Path) -> Result<Option<Self>, String> {
        let directory = target.is_dir();
        let start = if directory {
            target
        } else {
            target
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
        };
        let start = start
            .canonicalize()
            .map_err(|e| format!("{}: {e}", start.display()))?;
        let Some(root) = start
            .ancestors()
            .take(SourceLimits::default().max_ancestors)
            .find(|dir| packages::is_package(dir))
        else {
            return Ok(None);
        };
        let src = root.join("src");
        if !src.is_dir() || (start != root && !start.starts_with(&src)) {
            return Ok(None);
        }
        let file = if directory {
            None
        } else if start.starts_with(&src) && target.extension().is_some_and(|ext| ext == "ves") {
            target.file_name().map(|name| start.join(name))
        } else {
            return Ok(None);
        };
        Ok(Some(Self {
            root: root.to_owned(),
            file,
        }))
    }
}

struct TestSource {
    display: String,
    source: String,
    id: Option<usize>,
    cases: Vec<TestCase>,
    tests: Vec<DiscoveredTest>,
}

impl TestSource {
    fn prepare(display: String, source: &mut String) -> Result<(parser::Parse, Self), String> {
        let original = source.clone();
        let (parsed, cases) = prepare_tests(source)?;
        let tests = discover_tests(&original, &parsed.module.items, &cases)?;
        Ok((
            parsed,
            Self {
                display,
                source: original,
                id: None,
                cases,
                tests,
            },
        ))
    }
}

struct TestProgram {
    module: Module,
    files: Vec<TestSource>,
    sources: Option<SourceMap>,
    root_file_count: usize,
    imports: Option<Imports>,
}

enum TestWorld {
    Ordinary(World),
    Foreign(ForeignChecked),
}

impl TestWorld {
    fn world(&self) -> &World {
        match self {
            Self::Ordinary(world) => world,
            Self::Foreign(checked) => checked.world(),
        }
    }

    fn foreign<'a>(
        &'a self,
        host: Option<&'a EvalHost>,
    ) -> Option<(&'a ForeignChecked, &'a EvalHost)> {
        match self {
            Self::Ordinary(_) => None,
            Self::Foreign(checked) => host.map(|host| (checked, host)),
        }
    }
}

fn bridge_diagnostics(mut error: FfiError) -> Vec<Diagnostic> {
    if let Some(span) = error.span {
        error.diagnostics.push(Diagnostic::new(
            "bridge",
            format!("{:?}: {}", error.kind, error.message),
            span,
        ));
    }
    error.diagnostics
}

fn test_host(imports: &Imports) -> Result<EvalHost, String> {
    crate::vsys_commands::eval_host(imports, "test")
}

impl TestProgram {
    fn standalone(display: String, source: String) -> Result<Self, Box<FileUnitTests>> {
        let mut expanded = source.clone();
        match TestSource::prepare(display.clone(), &mut expanded) {
            Ok((parsed, file)) => Ok(Self {
                module: parsed.module,
                files: vec![file],
                sources: None,
                root_file_count: 1,
                imports: None,
            }),
            Err(error) => Err(Box::new(FileUnitTests::failed(display, source, error))),
        }
    }

    fn negative_names(&self) -> BTreeSet<String> {
        self.files
            .iter()
            .flat_map(|file| &file.tests)
            .filter(|test| test.plan.primary_mode == TestMode::ExpectError)
            .map(|test| test.func.name.clone())
            .collect()
    }

    fn check(&self, module: &Module) -> Result<TestWorld, Box<FileUnitTests>> {
        if let Some(imports) = &self.imports {
            return ffi_lowering::check_module(module, imports)
                .map(TestWorld::Foreign)
                .map_err(|error| {
                    let message = error.to_string();
                    let diagnostics = bridge_diagnostics(error);
                    Box::new(if diagnostics.is_empty() {
                        FileUnitTests::failed(
                            self.files
                                .first()
                                .map_or("<package>", |f| &f.display)
                                .into(),
                            String::new(),
                            message,
                        )
                    } else {
                        self.semantic_error(&diagnostics)
                    })
                });
        }
        let checked = check::check(module);
        if checked.diagnostics.is_empty() {
            Ok(TestWorld::Ordinary(checked.world))
        } else {
            Err(Box::new(self.semantic_error(&checked.diagnostics)))
        }
    }

    fn package(target: &PackageTarget, replacement: Option<(&Path, &str)>) -> Result<Self, String> {
        let mut files = Vec::new();
        let mut found = target.file.is_none();
        let mut parse_root = |path: &Path| {
            let mut source = match replacement {
                Some((replaced, text)) if path == replaced => text.to_owned(),
                _ => read_source(path)?,
            };
            if target.file.as_deref().is_some_and(|file| file != path) {
                files.push(None);
                let parsed = parser::parse(&source);
                return Ok((source, parsed));
            }
            found = true;
            let display = path
                .strip_prefix(&target.root)
                .unwrap_or(path)
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let (parsed, tested) = TestSource::prepare(display, &mut source)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            files.push(Some(tested));
            Ok((source, parsed))
        };
        let assembled = match replacement {
            Some(replacement) => packages::load_with_dependency_replacement(
                &target.root,
                &mut parse_root,
                replacement,
            )?,
            None => packages::load_with_root_parser(&target.root, &mut parse_root)?,
        };
        if let Some((path, d)) = assembled.manifest_errors.first() {
            return Err(format!("{path}: {} {}", d.code, d.message));
        }
        let imports = (!assembled.vsys.is_empty())
            .then(|| assembled.imports(None))
            .transpose()?;
        let Assembled {
            module,
            sources,
            diagnostics,
            root_package_files,
            ..
        } = assembled;
        if let Some(d) = diagnostics.first() {
            let diagnostic = &d.diagnostic;
            if let Some(index) = d.file {
                let file = &sources.files[index];
                let tests = root_package_files
                    .iter()
                    .position(|&id| id == index)
                    .and_then(|i| files[i].as_ref());
                return Err(format!(
                    "{}: {}",
                    file.display,
                    test_diagnostic(
                        &file.text,
                        tests.map_or(&[], |f| f.cases.as_slice()),
                        diagnostic,
                        "package",
                        diagnostics.len()
                    )
                ));
            }
            return Err(format!("{} {}", diagnostic.code, diagnostic.message));
        }
        if !found {
            return Err(format!(
                "{} is not a source module in {}",
                target.file.as_ref().unwrap().display(),
                target.root.display()
            ));
        }
        let functions: BTreeMap<_, _> = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(f) => Some((f.span.start, f.as_ref())),
                _ => None,
            })
            .collect();
        let mut prepared = Vec::new();
        let root_file_count = root_package_files.len();
        for (file, id) in files.into_iter().zip(root_package_files) {
            let Some(mut file) = file else {
                continue;
            };
            file.id = Some(id);
            let base = sources.files[id].base;
            for test in &mut file.tests {
                let offset = base + test.func.span.start;
                let function = functions.get(&offset).ok_or_else(|| {
                    format!("{}: could not resolve test '{}'", file.display, test.name)
                })?;
                test.func = (**function).clone();
            }
            prepared.push(file);
        }
        Ok(Self {
            module,
            files: prepared,
            sources: Some(sources),
            root_file_count,
            imports,
        })
    }

    fn semantic_error(&self, diagnostics: &[Diagnostic]) -> FileUnitTests {
        let first = &diagnostics[0];
        let localized = self.sources.as_ref().map(|sources| sources.localize(first));
        let id = localized.as_ref().and_then(|d| d.file);
        let file = id.and_then(|i| self.sources.as_ref()?.files.get(i));
        let source = match id {
            Some(id) => self.files.iter().find(|f| f.id == Some(id)),
            None => self.files.first(),
        };
        let text = source.map_or_else(|| file.map_or("", |f| f.text.as_str()), |f| &f.source);
        let error = test_diagnostic(
            text,
            source.map_or(&[], |f| f.cases.as_slice()),
            localized.as_ref().map_or(first, |d| &d.diagnostic),
            "semantic",
            diagnostics.len(),
        );
        let display = source.map_or_else(
            || file.map_or("<package>", |f| f.display.as_str()),
            |f| f.display.as_str(),
        );
        FileUnitTests::failed(display.to_owned(), text.to_owned(), error)
    }
}

fn host_target_isa() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn run_unit_suite(opts: &Options) -> Result<(bool, Vec<FileUnitTests>), String> {
    let target = Path::new(&opts.path);
    let package = PackageTarget::discover(target)?
        .map(|package| TestProgram::package(&package, None))
        .transpose()?;
    let standalone = if package.is_none() {
        collect_files(target)?
    } else {
        Vec::new()
    };
    let programs = package.into_iter().map(Ok).chain(
        standalone
            .into_iter()
            .map(|(display, source)| TestProgram::standalone(display, source)),
    );
    let mut results = Vec::new();
    let mut total_discovered = 0usize;
    let mut total_selected = 0usize;
    let mut all_passed = true;
    let verdict_cache = if !opts.no_cache && !opts.mutate {
        VerdictCache::open_default().ok()
    } else {
        None
    };

    for program in programs {
        let program = match program {
            Ok(program) => program,
            Err(file) => {
                all_passed = false;
                results.push(*file);
                continue;
            }
        };
        total_discovered += program
            .files
            .iter()
            .map(|file| file.tests.len())
            .sum::<usize>();
        let selected_count = program
            .files
            .iter()
            .flat_map(|file| &file.tests)
            .filter(|test| opts.selects(test))
            .count();
        total_selected += selected_count;
        if selected_count == 0 {
            continue;
        }

        let module = checked_module(&program.module, program.negative_names());
        let checked = match program.check(&module) {
            Ok(checked) => checked,
            Err(file) => {
                all_passed = false;
                results.push(*file);
                continue;
            }
        };
        let host = program.imports.as_ref().map(test_host).transpose()?;
        let fn_graph = FunctionGraph::from_module(&program.module);
        let combined = program.sources.as_ref().map(SourceMap::combined);
        let package_hash = combined.as_ref().map(|text| blake3::hash(text.as_bytes()));
        let execution = TestExecution {
            opts,
            world: checked.world(),
            foreign: checked.foreign(host.as_ref()),
            bridge_module: program.imports.as_ref().map(|imports| (&*module, imports)),
            fn_graph: &fn_graph,
            combined: combined.as_deref(),
            package_hash,
            // Host I/O is external to the source fingerprint and can change between runs.
            verdict_cache: verdict_cache.as_ref().filter(|_| program.imports.is_none()),
        };
        for file in program.files {
            if let Some(result) = execution.run_file(file) {
                all_passed &= result.report.passed;
                results.push(result);
            }
        }
    }

    if total_discovered == 0 && all_passed {
        return Err(format!("no Vesper unit tests found in {}", opts.path));
    }
    if total_selected == 0 && opts.filter.is_some() && all_passed {
        return Err(format!(
            "no Vesper unit tests matched filter '{}'",
            opts.filter.as_deref().unwrap_or_default()
        ));
    }
    Ok((all_passed, results))
}

struct TestExecution<'a> {
    opts: &'a Options,
    world: &'a World,
    foreign: Option<(&'a ForeignChecked, &'a EvalHost)>,
    bridge_module: Option<(&'a Module, &'a Imports)>,
    fn_graph: &'a FunctionGraph,
    combined: Option<&'a str>,
    package_hash: Option<blake3::Hash>,
    verdict_cache: Option<&'a VerdictCache>,
}

impl TestExecution<'_> {
    fn run_file(&self, file: TestSource) -> Option<FileUnitTests> {
        let Self {
            opts,
            world,
            foreign,
            bridge_module,
            fn_graph,
            combined,
            package_hash,
            verdict_cache,
        } = *self;
        let TestSource {
            display,
            source,
            tests,
            ..
        } = file;
        let expect_error_names = tests
            .iter()
            .filter(|t| t.plan.primary_mode == TestMode::ExpectError)
            .map(|t| t.func.name.clone())
            .collect();
        let all_test_names = tests.iter().map(|t| t.name.clone()).collect();
        let selected_tests: Vec<_> = tests
            .into_iter()
            .filter(|test| opts.selects(test))
            .collect();
        if selected_tests.is_empty() {
            return None;
        }
        let diagnostic_source = combined.unwrap_or(&source);

        let mut case_reports: BTreeMap<String, UnitTestCaseReport> = BTreeMap::new();
        let mut unit_batch_selected = Vec::new();
        let mut unit_plans: BTreeMap<String, HarnessPlan> = BTreeMap::new();
        let mut replay: Vec<ReplayTest> = Vec::new();

        for test in &selected_tests {
            let (line, column) = location(&source, test.span.start);
            let mut fn_hash = fn_graph
                .transitive_fn_blake3(&test.func.name)
                .unwrap_or_default();
            if let Some(hash) = package_hash {
                fn_hash.push_str(&hash.to_hex());
            }
            let cache_key = VerdictKey {
                fn_blake3: test_fingerprint(&fn_hash, test, opts.fuel),
                sim_seed: test.plan.seed,
                target_isa: host_target_isa(),
                harness_version: vesper_test::HARNESS_VERSION,
            };

            // Check CAS verdict cache for deterministic non-unit Anvil modes
            // (unit mode preserves exact JIT/Evaluator engine reporting for existing CLI callers).
            let use_cache = verdict_cache.is_some()
                && is_cacheable(&test.plan)
                && !matches!(test.plan.primary_mode, TestMode::Unit);
            if use_cache
                && let Some(cache) = verdict_cache
                && let Some(bytes) = cache.get(&cache_key.cache_key_hex())
                && let Ok(record) = VerdictRecord::from_json(&String::from_utf8_lossy(&bytes))
            {
                let passed = record.status == VerdictStatus::Pass;

                case_reports.insert(
                    test.name.clone(),
                    UnitTestCaseReport {
                        name: test.name.clone(),
                        line,
                        column,
                        passed,
                        result: None,
                        message: format!("cached: {}", record.message.as_deref().unwrap_or("ok")),
                        engine: TestEngine::Cache,
                        fallback_reason: None,
                        anvil_mode: Some(test.plan.primary_mode),
                        cached: Some(true),
                        test_case_pin: None,
                        vchrono_capsule: record.capsule_path,
                    },
                );
                continue;
            }

            match test.plan.primary_mode {
                TestMode::Unit | TestMode::BudgetGate => {
                    unit_batch_selected.push(UnitEntry::from(test));
                    unit_plans.insert(test.name.clone(), test.plan.clone());
                }
                TestMode::ExpectError => {
                    let diags = if let Some((module, imports)) = bridge_module {
                        let mut module = module.clone();
                        module
                            .items
                            .push(Item::Function(Box::new(test.func.clone())));
                        ffi_lowering::check_module(&module, imports)
                            .err()
                            .map_or_else(Vec::new, bridge_diagnostics)
                    } else {
                        world.check_isolated_item(&test.func)
                    };
                    let observed: Vec<ObservedDiagnostic> = diags
                        .iter()
                        .map(|d| ObservedDiagnostic {
                            code: Some(d.code.clone()),
                            message: d.message.clone(),
                            primary_text: diagnostic_source
                                .get(d.span.start..d.span.end)
                                .unwrap_or("")
                                .to_string(),
                            related_texts: d
                                .related
                                .iter()
                                .map(|r| {
                                    let snippet = diagnostic_source
                                        .get(r.span.start..r.span.end)
                                        .unwrap_or("");
                                    format!("{} {}", r.message, snippet)
                                })
                                .collect(),
                        })
                        .collect();
                    let spec = test
                        .plan
                        .expect_error
                        .as_ref()
                        .expect("ExpectError mode has spec");
                    let outcome = verify_expect_error(spec, &observed);
                    let (passed, message) = match outcome {
                        ExpectErrorOutcome::Matched { .. } => {
                            (true, format!("matched {}", spec.code))
                        }
                        other => (false, format!("{other:?}")),
                    };

                    if let Some(cache) = verdict_cache
                        && is_cacheable(&test.plan)
                    {
                        let rec = VerdictRecord {
                            key: cache_key,
                            test_name: test.name.clone(),
                            mode: TestMode::ExpectError,
                            status: if passed {
                                VerdictStatus::Pass
                            } else {
                                VerdictStatus::Fail
                            },
                            message: Some(message.clone()),
                            duration_us: 0,
                            capsule_path: None,
                        };
                        if let Ok(json) = rec.to_json() {
                            let _ = cache.put(&rec.cache_key_hex(), json.as_bytes());
                        }
                    }
                    case_reports.insert(
                        test.name.clone(),
                        UnitTestCaseReport {
                            name: test.name.clone(),
                            line,
                            column,
                            passed,
                            result: None,
                            message,
                            engine: TestEngine::Checker,
                            fallback_reason: None,
                            anvil_mode: Some(TestMode::ExpectError),
                            cached: Some(false),
                            test_case_pin: None,
                            vchrono_capsule: None,
                        },
                    );
                }
                TestMode::Property => {
                    let explicit = match eval_test_case_args(&test.func, &test.raw_cases) {
                        Ok(rows) => rows,
                        Err(message) => {
                            case_reports.insert(
                                test.name.clone(),
                                UnitTestCaseReport {
                                    name: test.name.clone(),
                                    line,
                                    column,
                                    passed: false,
                                    result: None,
                                    message,
                                    engine: TestEngine::Crucible,
                                    fallback_reason: None,
                                    anvil_mode: Some(TestMode::Property),
                                    cached: Some(false),
                                    test_case_pin: None,
                                    vchrono_capsule: None,
                                },
                            );
                            continue;
                        }
                    };
                    let cases_n = if test.plan.property_cases > 0 {
                        test.plan.property_cases as usize
                    } else {
                        vesper_test::DEFAULT_PROPERTY_CASES as usize
                    };
                    let outcome =
                        run_property_test(world, &test.func.name, cases_n, &explicit, opts.fuel);
                    let passed = outcome.passed;
                    if passed {
                        replay.push(ReplayTest::Property {
                            name: test.name.clone(),
                            entry: test.func.name.clone(),
                            cases: cases_n,
                            explicit: explicit.clone(),
                        });
                    }
                    let mut capsule = None;
                    let mut message = if passed {
                        format!("ok ({} case(s))", outcome.cases_run)
                    } else {
                        outcome
                            .failure_message
                            .clone()
                            .unwrap_or_else(|| "property failed".into())
                    };
                    if !passed {
                        let out_dir = Path::new("target").join("anvil");
                        let tape = outcome.failing_tape.as_deref().unwrap_or(&[]);
                        if let Ok(path) = write_vchrono_capsule(
                            &out_dir,
                            &test.name,
                            outcome.seed,
                            tape,
                            &message,
                        ) {
                            capsule = Some(path.display().to_string());
                        }
                        if let Some(ce) = &outcome.counterexample {
                            let rendered: Vec<String> =
                                ce.iter().map(format_value_literal).collect();
                            message =
                                format!("{message}; counterexample=({})", rendered.join(", "));
                        }
                    }
                    if let Some(cache) = verdict_cache
                        && is_cacheable(&test.plan)
                    {
                        let rec = VerdictRecord {
                            key: cache_key,
                            test_name: test.name.clone(),
                            mode: TestMode::Property,
                            status: if passed {
                                VerdictStatus::Pass
                            } else {
                                VerdictStatus::Fail
                            },
                            message: Some(message.clone()),
                            duration_us: 0,
                            capsule_path: capsule.clone(),
                        };
                        if let Ok(json) = rec.to_json() {
                            let _ = cache.put(&rec.cache_key_hex(), json.as_bytes());
                        }
                    }
                    case_reports.insert(
                        test.name.clone(),
                        UnitTestCaseReport {
                            name: test.name.clone(),
                            line,
                            column,
                            passed,
                            result: None,
                            message,
                            engine: TestEngine::Crucible,
                            fallback_reason: None,
                            anvil_mode: Some(TestMode::Property),
                            cached: Some(false),
                            test_case_pin: outcome.test_case_pin,
                            vchrono_capsule: capsule,
                        },
                    );
                }
                TestMode::SimPulse => {
                    let schedules = if test.plan.sim_schedules > 0 {
                        test.plan.sim_schedules as usize
                    } else {
                        vesper_test::DEFAULT_SIM_SCHEDULES as usize
                    };
                    let profile = match &test.plan.fault_profile {
                        Some(FaultProfile::TornWalSector) => "TornWalSector",
                        Some(FaultProfile::NetPartition) => "NetPartition",
                        Some(FaultProfile::ClockSkew) => "ClockSkew",
                        Some(FaultProfile::Custom(s)) => s.as_str(),
                        None => "None",
                    };
                    let outcome = run_simpulse_test_on_world(
                        world,
                        &test.func.name,
                        schedules,
                        profile,
                        test.plan.seed,
                    );
                    let passed = outcome.passed;
                    if passed {
                        replay.push(ReplayTest::SimPulse {
                            name: test.name.clone(),
                            entry: test.func.name.clone(),
                            schedules,
                            profile: profile.to_string(),
                            seed: test.plan.seed,
                        });
                    }
                    let message = if passed {
                        format!(
                            "ok ({} schedule(s), {} DPOR exec(s))",
                            outcome.schedules_run, outcome.dpor_executions
                        )
                    } else {
                        outcome
                            .failure_message
                            .clone()
                            .unwrap_or_else(|| "SimPulse schedule failed".into())
                    };
                    let mut capsule = None;
                    if !passed {
                        let out_dir = Path::new("target").join("anvil");
                        let fail_seed = outcome.failing_seed.unwrap_or(test.plan.seed);
                        if let Ok(path) =
                            write_vchrono_capsule(&out_dir, &test.name, fail_seed, &[], &message)
                        {
                            capsule = Some(path.display().to_string());
                        }
                    }
                    case_reports.insert(
                        test.name.clone(),
                        UnitTestCaseReport {
                            name: test.name.clone(),
                            line,
                            column,
                            passed,
                            result: None,
                            message,
                            engine: TestEngine::SimPulse,
                            fallback_reason: None,
                            anvil_mode: Some(TestMode::SimPulse),
                            cached: Some(false),
                            test_case_pin: None,
                            vchrono_capsule: capsule,
                        },
                    );
                }
            }
        }

        if !unit_batch_selected.is_empty() {
            let (_, unit_cases) = run_tests_on_world(
                &source,
                world,
                &unit_batch_selected,
                opts.fuel,
                if opts.mutate {
                    UnitExecution::MutationBaseline
                } else {
                    UnitExecution::Standard
                },
                foreign,
                opts.optimize,
            );
            for mut rep in unit_cases {
                if let Some(plan) = unit_plans.get(&rep.name) {
                    rep.anvil_mode = Some(plan.primary_mode);
                    // The harness cannot measure stack, cycles or allocations
                    // yet, so a gated test is never reported as passing on the
                    // strength of its ordinary unit run.
                    if plan.primary_mode == TestMode::BudgetGate && rep.passed {
                        rep.passed = false;
                        rep.message = format!(
                            "budget gate not enforced: this harness cannot yet check {} (the unit run passed)",
                            budget_gate_names(plan)
                        );
                    }
                    if opts.engines_all && plan.primary_mode == TestMode::Unit && rep.passed {
                        engines_agree(
                            &mut rep,
                            &unit_batch_selected,
                            world,
                            foreign,
                            &source,
                            opts.fuel,
                        );
                    }
                }
                case_reports.insert(rep.name.clone(), rep);
            }
        }

        // Preserve source declaration order for deterministic reports.
        let mut ordered_cases = Vec::with_capacity(selected_tests.len());
        let mut file_passed = true;
        for test in &selected_tests {
            if let Some(rep) = case_reports.remove(&test.name) {
                if !rep.passed {
                    file_passed = false;
                }
                ordered_cases.push(rep);
            }
        }
        let unit_selected: Vec<UnitEntry> = selected_tests
            .iter()
            .filter(|t| matches!(t.plan.primary_mode, TestMode::Unit | TestMode::BudgetGate))
            .map(UnitEntry::from)
            .collect();

        Some(FileUnitTests {
            display: display.clone(),
            source,
            report: UnitTestFileReport {
                file: display,
                passed: file_passed,
                tests: ordered_cases,
                error: None,
            },
            selected: unit_selected,
            replay,
            test_names: all_test_names,
            expect_error_names,
        })
    }
}

#[derive(Clone)]
struct MutantCandidate {
    file: String,
    function: String,
    start: usize,
    end: usize,
    original: String,
    replacement: String,
}

fn find_operator_span(
    source: &str,
    tokens: &[vesper::lexer::Token],
    range_start: usize,
    range_end: usize,
    op: &str,
) -> Option<(usize, usize)> {
    let start_idx = tokens.partition_point(|t| t.span.start < range_start);
    for tok in &tokens[start_idx..] {
        if tok.span.end > range_end {
            break;
        }
        if matches!(tok.kind, K::Whitespace | K::Comment | K::BlockComment) {
            continue;
        }
        if &source[tok.span.clone()] == op {
            return Some((tok.span.start, tok.span.end));
        }
    }
    None
}

fn collect_mutants_in_fn(
    file: &str,
    source: &str,
    tokens: &[vesper::lexer::Token],
    func: &Function,
    out: &mut Vec<MutantCandidate>,
) {
    let Some(body) = &func.body else {
        return;
    };
    let mut stack: Vec<&Expr> = vec![body];
    let mut visited = 0usize;
    while let Some(expr) = stack.pop() {
        visited += 1;
        if visited > 16_384 {
            break;
        }
        match &expr.kind {
            ExprKind::Literal(Literal::Bool(val)) => {
                if let Some(slice) = source.get(expr.span.start..expr.span.end) {
                    let trimmed = slice.trim();
                    if (trimmed == "true" && *val) || (trimmed == "false" && !*val) {
                        let lead = slice.len() - slice.trim_start().len();
                        let start = expr.span.start + lead;
                        let end = start + trimmed.len();
                        out.push(MutantCandidate {
                            file: file.to_string(),
                            function: func.name.clone(),
                            start,
                            end,
                            original: trimmed.to_string(),
                            replacement: if *val { "false" } else { "true" }.into(),
                        });
                    }
                }
            }
            ExprKind::Unary(op, inner) => {
                if op == "!"
                    && let Some((start, end)) =
                        find_operator_span(source, tokens, expr.span.start, inner.span.start, "!")
                {
                    out.push(MutantCandidate {
                        file: file.to_string(),
                        function: func.name.clone(),
                        start,
                        end,
                        original: "!".into(),
                        replacement: " ".into(),
                    });
                }
                stack.push(inner);
            }
            ExprKind::Binary(op, lhs, rhs) => {
                let replacements: &[&str] = match op.as_str() {
                    "+" => &["-"],
                    "-" => &["+"],
                    "*" => &["+"],
                    "==" => &["!="],
                    "!=" => &["=="],
                    "<" => &["<=", ">="],
                    "<=" => &["<", ">"],
                    ">" => &[">=", "<="],
                    ">=" => &[">", "<"],
                    "&&" => &["||"],
                    "||" => &["&&"],
                    _ => &[],
                };
                if !replacements.is_empty()
                    && let Some((start, end)) =
                        find_operator_span(source, tokens, lhs.span.end, rhs.span.start, op)
                {
                    for rep in replacements {
                        out.push(MutantCandidate {
                            file: file.to_string(),
                            function: func.name.clone(),
                            start,
                            end,
                            original: op.clone(),
                            replacement: (*rep).to_string(),
                        });
                    }
                }
                stack.push(rhs);
                stack.push(lhs);
            }
            ExprKind::Assign(op, lhs, rhs) => {
                let rep = match op.as_str() {
                    "+=" => Some("-="),
                    "-=" => Some("+="),
                    _ => None,
                };
                if let Some(rep) = rep
                    && let Some((start, end)) =
                        find_operator_span(source, tokens, lhs.span.end, rhs.span.start, op)
                {
                    out.push(MutantCandidate {
                        file: file.to_string(),
                        function: func.name.clone(),
                        start,
                        end,
                        original: op.clone(),
                        replacement: rep.to_string(),
                    });
                }
                stack.push(rhs);
                stack.push(lhs);
            }
            ExprKind::Tuple(es) => {
                for e in es.iter().rev() {
                    stack.push(e);
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, e) in fields.iter().rev() {
                    stack.push(e);
                }
            }
            ExprKind::Block { statements, tail } => {
                if let Some(t) = tail {
                    stack.push(t);
                }
                for st in statements.iter().rev() {
                    match st {
                        Statement::Let { value, .. } => stack.push(value),
                        Statement::Expr(e) => stack.push(e),
                    }
                }
            }
            ExprKind::Call { callee, args, .. } => {
                for a in args.iter().rev() {
                    stack.push(&a.expr);
                }
                stack.push(callee);
            }
            ExprKind::Field(inner, _) | ExprKind::Try(inner) => stack.push(inner),
            ExprKind::Index(a, b) => {
                stack.push(b);
                stack.push(a);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                if let Some(e) = otherwise {
                    stack.push(e);
                }
                stack.push(then);
                stack.push(condition);
            }
            ExprKind::Match { value, arms } => {
                for (_, e) in arms.iter().rev() {
                    stack.push(e);
                }
                stack.push(value);
            }
            ExprKind::Closure { body, .. } => stack.push(body),
            k @ (ExprKind::Loop(_)
            | ExprKind::For { .. }
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Return(_)) => {
                let mut children = k.control_children().unwrap_or_default();
                children.reverse();
                stack.extend(children);
            }
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                for c in ops.iter().rev() {
                    stack.push(&c.body);
                }
                stack.push(&ret.body);
                stack.push(body);
                if let Some((_, e)) = state {
                    stack.push(e);
                }
            }
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
        }
    }
}

fn collect_source_mutants(
    display: &str,
    source: &str,
    test_names: &BTreeSet<&str>,
) -> Vec<MutantCandidate> {
    let (tokens, _) = lexer::lex(source);
    let mut candidates = Vec::new();
    for item in &parser::parse(source).module.items {
        match item {
            Item::Function(f) => {
                if !test_names.contains(f.name.as_str())
                    && !f.name.starts_with("test_")
                    && !preceding_comments(&tokens, f.span.start).any(|comment| {
                        let text = &source[comment.span.clone()];
                        is_test_marker(text) || case_marker(text).is_some()
                    })
                {
                    collect_mutants_in_fn(display, source, &tokens, f, &mut candidates);
                }
            }
            Item::Impl { methods, .. } => {
                for m in methods {
                    collect_mutants_in_fn(display, source, &tokens, m, &mut candidates);
                }
            }
            Item::Conformance(c) => {
                for m in &c.methods {
                    collect_mutants_in_fn(display, source, &tokens, m, &mut candidates);
                }
            }
            Item::Adt(_) | Item::Effect(_) | Item::Trait(_) => {}
        }
    }
    candidates.sort_by_key(|c| (c.start, c.end, c.replacement.clone()));
    candidates
}

fn run_mutation_gate(opts: &Options) -> Result<(), String> {
    let (baseline_passed, files) = run_unit_suite(opts)?;
    if !baseline_passed {
        let summary = serde_json::json!({
            "mode": "mutate",
            "passed": false,
            "error": "baseline unit tests failed before mutation testing",
            "files": files.iter().map(|f| &f.report).collect::<Vec<_>>(),
        });
        if opts.json {
            println!("{summary}");
        } else {
            for f in &files {
                if let Some(error) = &f.report.error {
                    println!("{}: FAIL: {error}", f.display);
                }
                for t in &f.report.tests {
                    if !t.passed {
                        println!(
                            "{}:{}:{}: FAIL {}: {}",
                            f.display, t.line, t.column, t.name, t.message
                        );
                    }
                }
            }
        }
        return Err("baseline unit tests failed before mutation testing".into());
    }

    let package = PackageTarget::discover(Path::new(&opts.path))?;
    let package_program = package
        .as_ref()
        .map(|target| TestProgram::package(target, None))
        .transpose()?;
    let host = package_program
        .as_ref()
        .and_then(|program| program.imports.as_ref())
        .map(test_host)
        .transpose()?;
    let mut all_candidates = Vec::new();
    let dependency_source = if let Some(path) = &opts.mutate_dependency {
        let program = package_program
            .as_ref()
            .ok_or("--mutate-dependency requires a Loom package")?;
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Some(
            program
                .sources
                .as_ref()
                .expect("package source map")
                .files
                .iter()
                .skip(program.root_file_count)
                .find(|file| {
                    Path::new(&file.display)
                        .canonicalize()
                        .is_ok_and(|p| p == canonical)
                })
                .ok_or_else(|| {
                    format!(
                        "{} is not a source file in the package dependencies",
                        path.display()
                    )
                })?,
        )
    } else {
        None
    };
    if let Some(file) = dependency_source {
        if files
            .iter()
            .any(|f| !f.selected.is_empty() || !f.replay.is_empty())
        {
            all_candidates.extend(
                collect_source_mutants(&file.display, &file.text, &BTreeSet::new())
                    .into_iter()
                    .map(|candidate| (0, candidate)),
            );
        }
    } else if let Some(program) = &package_program {
        if files
            .iter()
            .any(|f| !f.selected.is_empty() || !f.replay.is_empty())
        {
            for (index, file) in program.files.iter().enumerate() {
                let names = file.tests.iter().map(|test| test.name.as_str()).collect();
                all_candidates.extend(
                    collect_source_mutants(&file.display, &file.source, &names)
                        .into_iter()
                        .map(|candidate| (index, candidate)),
                );
            }
        }
    } else {
        for (index, file) in files.iter().enumerate() {
            if file.selected.is_empty() && file.replay.is_empty() {
                continue;
            }
            let names = file.test_names.iter().map(String::as_str).collect();
            all_candidates.extend(
                collect_source_mutants(&file.display, &file.source, &names)
                    .into_iter()
                    .map(|candidate| (index, candidate)),
            );
        }
    }
    let total_candidates = all_candidates.len();
    if total_candidates == 0 {
        return Err(format!(
            "no mutation sites found outside test functions in {} (mutants are replayed against unit, budget-gate, property and SimPulse tests)",
            opts.path
        ));
    }
    let start = opts.mutant_offset;
    if start >= total_candidates {
        return Err(format!(
            "--mutant-offset {start} is outside the {total_candidates} mutation candidate(s)"
        ));
    }
    let limit = (total_candidates - start).min(opts.max_mutants);
    let end = start + limit;
    let capped = end < total_candidates;

    let mut reports = Vec::with_capacity(limit);
    let mut killed = 0usize;
    let mut survived = 0usize;
    let mut ill_typed = 0usize;

    for (idx, (file_index, cand)) in all_candidates.iter().enumerate().skip(start).take(limit) {
        let file_index = *file_index;
        let source = match (dependency_source, &package_program) {
            (Some(file), _) => &file.text,
            (None, Some(program)) => &program.files[file_index].source,
            (None, None) => &files[file_index].source,
        };
        let (line, column) = location(source, cand.start);
        let mut mutated =
            String::with_capacity(source.len() - (cand.end - cand.start) + cand.replacement.len());
        mutated.push_str(&source[..cand.start]);
        mutated.push_str(&cand.replacement);
        mutated.push_str(&source[cand.end..]);

        let checked = if let Some(target) = &package {
            let path = target.root.join(&cand.file);
            TestProgram::package(target, Some((&path, &mutated))).and_then(|program| {
                program
                    .check(&checked_module(&program.module, program.negative_names()))
                    .map_err(|file| file.report.error.unwrap())
            })
        } else {
            checked_for_mutation(&mutated, &files[file_index].expect_error_names)
                .map(|checked| TestWorld::Ordinary(checked.world))
        };

        let report = match checked {
            Err(err) => {
                ill_typed += 1;
                MutantReport {
                    index: idx,
                    file: cand.file.clone(),
                    function: cand.function.clone(),
                    line,
                    column,
                    original: cand.original.clone(),
                    replacement: cand.replacement.clone(),
                    status: MutantStatus::IllTyped,
                    killed_by: None,
                    detail: Some(err),
                }
            }
            Ok(checked) => {
                let suites = if package.is_some() {
                    files.as_slice()
                } else {
                    std::slice::from_ref(&files[file_index])
                };
                let killer = suites.iter().find_map(|unit| {
                    let (_, cases) = run_tests_on_world(
                        &unit.source,
                        checked.world(),
                        &unit.selected,
                        opts.fuel,
                        UnitExecution::Mutant,
                        checked.foreign(host.as_ref()),
                        opts.optimize,
                    );
                    cases
                        .into_iter()
                        .find(|case| !case.passed)
                        .map(|case| (case.name, case.message))
                        .or_else(|| {
                            unit.replay
                                .iter()
                                .find_map(|test| test.run(checked.world(), opts.fuel).err())
                        })
                });
                if killer.is_none() {
                    survived += 1;
                    MutantReport {
                        index: idx,
                        file: cand.file.clone(),
                        function: cand.function.clone(),
                        line,
                        column,
                        original: cand.original.clone(),
                        replacement: cand.replacement.clone(),
                        status: MutantStatus::Survived,
                        killed_by: None,
                        detail: Some("all replayed tests still passed".into()),
                    }
                } else {
                    killed += 1;
                    MutantReport {
                        index: idx,
                        file: cand.file.clone(),
                        function: cand.function.clone(),
                        line,
                        column,
                        original: cand.original.clone(),
                        replacement: cand.replacement.clone(),
                        status: MutantStatus::Killed,
                        killed_by: killer.as_ref().map(|k| k.0.clone()),
                        detail: killer.map(|k| k.1),
                    }
                }
            }
        };
        reports.push(report);
    }

    let runnable_mutants = killed + survived;
    if runnable_mutants == 0 {
        return Err(format!(
            "all {ill_typed} mutant(s) were ill-typed; no runnable mutant to test"
        ));
    }
    let gate_passed = survived == 0;

    if opts.json {
        println!(
            "{}",
            serde_json::json!({
                "mode": "mutate",
                "passed": gate_passed,
                "total_candidates": total_candidates,
                "offset": start,
                "tested": limit,
                "capped": capped,
                "killed": killed,
                "survived": survived,
                "ill_typed": ill_typed,
                "mutants": reports,
            })
        );
    } else {
        for m in &reports {
            match m.status {
                MutantStatus::Killed => println!(
                    "{}:{}:{}: KILLED {} ({} -> {}) by {}",
                    m.file,
                    m.line,
                    m.column,
                    m.function,
                    m.original,
                    m.replacement,
                    m.killed_by.as_deref().unwrap_or("test")
                ),
                MutantStatus::Survived => println!(
                    "{}:{}:{}: SURVIVED {} ({} -> {})",
                    m.file, m.line, m.column, m.function, m.original, m.replacement
                ),
                MutantStatus::IllTyped => {}
            }
        }
        println!(
            "mutation gate: {killed} killed, {survived} survived, {ill_typed} ill-typed (of {limit}/{total_candidates} candidate(s))"
        );
    }

    if gate_passed {
        Ok(())
    } else {
        Err(format!(
            "mutation gate failed: {survived} mutant(s) survived"
        ))
    }
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let requested_json = args.iter().any(|arg| arg == "--json");
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(error) => {
            if requested_json {
                println!("{}", serde_json::json!({"passed":false,"error":error}));
            }
            return Err(error);
        }
    };
    if opts.mutate {
        return match run_mutation_gate(&opts) {
            Ok(()) => Ok(()),
            Err(error) => {
                if opts.json
                    && error != "mutation gate failed: "
                    && !error.starts_with("mutation gate failed:")
                    && !error.starts_with("baseline unit tests failed")
                {
                    println!(
                        "{}",
                        serde_json::json!({"mode":"mutate","passed":false,"error":error})
                    );
                }
                Err(error)
            }
        };
    }
    let (all_passed, files) = match run_unit_suite(&opts) {
        Ok(res) => res,
        Err(error) => {
            if opts.json {
                println!(
                    "{}",
                    serde_json::json!({"mode":"unit","passed":false,"error":error})
                );
            }
            return Err(error);
        }
    };
    let total_tests: usize = files.iter().map(|f| f.report.tests.len()).sum();
    let failed_tests: usize = files
        .iter()
        .map(|f| f.report.tests.iter().filter(|t| !t.passed).count())
        .sum();
    let file_errors: usize = files.iter().filter(|f| f.report.error.is_some()).count();

    if opts.json {
        let reports: Vec<&UnitTestFileReport> = files.iter().map(|f| &f.report).collect();
        println!(
            "{}",
            serde_json::json!({
                "mode": "unit",
                "passed": all_passed,
                "total": total_tests,
                "failed": failed_tests,
                "files": reports,
            })
        );
    } else {
        for f in &files {
            if let Some(err) = &f.report.error {
                println!("{}: FAIL: {err}", f.display);
                continue;
            }
            for t in &f.report.tests {
                let engine = match t.engine {
                    TestEngine::Jit => "jit",
                    TestEngine::Evaluator => "evaluator",
                    TestEngine::Checker => "checker",
                    TestEngine::Crucible => "crucible",
                    TestEngine::SimPulse => "simpulse",
                    TestEngine::Cache => "cache",
                };
                println!(
                    "{}:{}:{}: {} {} ({}) [{}]",
                    f.display,
                    t.line,
                    t.column,
                    if t.passed { "PASS" } else { "FAIL" },
                    t.name,
                    t.message,
                    engine
                );
                if let Some(pin) = &t.test_case_pin {
                    println!("  pin regression: {pin}");
                }
                if let Some(capsule) = &t.vchrono_capsule {
                    println!("  replay capsule: {capsule}");
                }
            }
        }
        println!(
            "tests: {} passed, {} failed ({} file error(s))",
            total_tests.saturating_sub(failed_tests),
            failed_tests,
            file_errors
        );
    }

    if all_passed {
        Ok(())
    } else {
        Err(format!(
            "{failed_tests} test(s) failed ({file_errors} file error(s))"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutant_replay_stops_after_the_first_failure() {
        let source = "fn first() -> bool { false } fn second() -> bool { true }";
        let checked = check::check(&parser::parse(source).module);
        assert!(checked.diagnostics.is_empty());
        let selected = ["first", "second"].map(|name| UnitEntry {
            name: name.into(),
            entry: name.into(),
            span: 0..0,
        });
        for (execution, expected_count) in [
            (UnitExecution::MutationBaseline, 2),
            (UnitExecution::Mutant, 1),
        ] {
            let (passed, reports) = run_tests_on_world(
                source,
                &checked.world,
                &selected,
                10_000,
                execution,
                None,
                true,
            );
            assert!(!passed);
            assert_eq!(reports.len(), expected_count);
            assert_eq!(reports[0].name, "first");
        }
    }
}
