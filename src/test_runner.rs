use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    fs::OpenOptions,
    io::Read,
    path::Path,
};
use vesper::{
    anvil_hash::{FunctionGraph, VerdictCache},
    anvil_sim::{
        format_value_literal, run_property_test, run_simpulse_test_on_world, write_vchrono_capsule,
    },
    ast::{Diagnostic, Expr, ExprKind, Function, Item, Literal, Module, Span, Statement, Type},
    check::{self, World},
    lexer::{self, Token, TokenKind as K},
    native::batch::{BatchLimits, NativeBatch},
    parser,
    runtime::{Runtime, Value},
    test_attrs::{self, AttrValue, FunctionTestAttrs},
    tooling::doc::sources::{SourceLimits, discover},
};
use vesper_test::{
    BudgetGate, ExpectErrorOutcome, ExpectErrorSpec, FaultProfile, HarnessPlan, ObligationLedger,
    ObservedDiagnostic, TestAttr, TestCaseAttr, TestMode, TestParam, TestSignature, VerdictKey,
    VerdictRecord, VerdictStatus, classify, is_cacheable, verify_expect_error,
};

const MAX_SOURCE_BYTES: usize = 512 * 1024;
const DEFAULT_FUEL: u64 = 1_000_000;
const DEFAULT_MAX_MUTANTS: usize = 256;
const HARD_MAX_MUTANTS: usize = 2048;

pub const USAGE: &str = "usage: vesper test --doc [FILE|DIR] [--json]\n       vesper test <FILE|DIR> [--mutate] [--filter <PAT>] [--max-mutants <N>] [--fuel <N>] [--no-cache] [--json]";

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
    filter: Option<String>,
    max_mutants: usize,
    fuel: u64,
    no_cache: bool,
    json: bool,
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
    let mut filter = None;
    let mut max_mutants = None;
    let mut fuel = DEFAULT_FUEL;
    let mut no_cache = false;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--mutate" if !mutate => mutate = true,
            "--no-cache" if !no_cache => no_cache = true,
            "--json" if !json => json = true,
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
    let path = path.ok_or_else(|| USAGE.to_string())?;
    Ok(Options {
        path,
        mutate,
        filter,
        max_mutants: max_mutants.unwrap_or(DEFAULT_MAX_MUTANTS),
        fuel,
        no_cache,
        json,
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

fn location(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let prefix = &source[..source.floor_char_boundary(offset)];
    (
        prefix.bytes().filter(|b| *b == b'\n').count() + 1,
        prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
    )
}

fn is_test_marker(comment_text: &str) -> bool {
    let trimmed = comment_text.trim_start_matches('/').trim_start();
    trimmed == "@test"
        || trimmed.starts_with("@test ")
        || trimmed.starts_with("@test:")
        || trimmed.starts_with("@test(")
}

fn case_marker(comment: &str) -> Option<&str> {
    let rest = comment
        .trim_start_matches('/')
        .trim_start()
        .strip_prefix("@case")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

fn preceding_comments(tokens: &[Token], fn_start: usize) -> impl Iterator<Item = &Token> {
    let end = tokens.partition_point(|t| t.span.end <= fn_start);
    tokens[..end]
        .iter()
        .rev()
        .take_while(|t| matches!(t.kind, K::Whitespace | K::Comment))
        .filter(|t| t.kind == K::Comment)
}

struct CaseWrapper {
    entry: String,
    generated: Span,
    function_index: usize,
}

struct TestCase {
    name: String,
    span: Span,
    wrapper: CaseWrapper,
}

impl TestCase {
    fn error(&self, source: &str, message: &str) -> String {
        let (line, column) = location(source, self.span.start);
        format!("{line}:{column}: {}: {message}", self.name)
    }
}

fn prepare_tests(source: &str) -> Result<(parser::Parse, Vec<TestCase>), String> {
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
    let (tokens, _) = lexer::lex(source);
    let names: BTreeSet<&str> = parsed
        .module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    let mut tests = Vec::new();
    let mut expanded = None;
    let mut next_entry = 0;
    for (function_index, item) in parsed.module.items.iter().enumerate() {
        let Item::Function(f) = item else {
            continue;
        };
        let rows: Vec<_> = preceding_comments(&tokens, f.span.start)
            .filter_map(|comment| {
                case_marker(&source[comment.span.clone()]).map(|row| (row, comment.span.clone()))
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        if !f.generics.is_empty() {
            return Err(format!("test function '{}' cannot be generic", f.name));
        }
        if f.body.is_none() {
            return Err(format!("test function '{}' must have a body", f.name));
        }

        let mut labels = BTreeSet::new();
        for (row, span) in rows.into_iter().rev() {
            let (label, args) = row.split_once('(').unwrap_or_default();
            let label = label.trim_end();
            let mut bytes = label.bytes();
            if !bytes
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || !args.ends_with(')')
            {
                let (line, column) = location(source, span.start);
                return Err(format!(
                    "{line}:{column}: expected @case name(arguments) for '{}'",
                    f.name
                ));
            }
            if !labels.insert(label) {
                let (line, column) = location(source, span.start);
                return Err(format!(
                    "{line}:{column}: duplicate test case '{}::{label}'",
                    f.name
                ));
            }
            let entry = loop {
                let name = format!("__vesper_test_case_{next_entry}");
                next_entry += 1;
                if !names.contains(name.as_str()) {
                    break name;
                }
            };
            let expanded = expanded.get_or_insert_with(|| source.to_owned());
            let start = expanded.len() + 1;
            write!(expanded, "\nfn {entry}() {{ {}({args} }}", f.name).unwrap();
            tests.push(TestCase {
                name: format!("{}::{label}", f.name),
                span,
                wrapper: CaseWrapper {
                    entry,
                    generated: start..expanded.len(),
                    function_index,
                },
            });
        }
    }
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
        let valid_body = matches!(
            f.body.as_ref().map(|body| &body.kind),
            Some(ExprKind::Block { statements, tail: Some(tail) })
                if statements.is_empty()
                && matches!(&tail.kind, ExprKind::Call { callee, .. }
                    if matches!(&callee.kind, ExprKind::Name(name) if name == &target.name))
        );
        if f.span != wrapper.generated || f.name != wrapper.entry || !valid_body {
            return Err(test.error(source, "expected a single argument list"));
        }
        f.result = target.result.clone();
        f.effects = target.effects.clone();
    }
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

fn syntax_attr_to_anvil(
    func: &Function,
    parsed: Option<&FunctionTestAttrs>,
) -> (TestSignature, Vec<test_attrs::TestCaseAttr>) {
    let mut attr = TestAttr::default();
    let mut cases = Vec::new();
    let mut raw_cases = Vec::new();
    if let Some(pa) = parsed {
        if let Some(t) = &pa.test {
            if let Some(code) = &t.expect_error {
                attr.expect_error = Some(ExpectErrorSpec {
                    code: code.clone(),
                    primary: t.primary.clone().or_else(|| t.expected_span.clone()),
                    related: t.related.clone(),
                });
            }
            attr.cases = t.cases.map(|n| n as u32);
            attr.sim_schedules = t.sim_schedules.map(|n| n as u32);
            if let Some(fp) = &t.fault_profile {
                attr.fault_profile = Some(match fp.as_str() {
                    "TornWalSector" => FaultProfile::TornWalSector,
                    "NetPartition" => FaultProfile::NetPartition,
                    "ClockSkew" => FaultProfile::ClockSkew,
                    other => FaultProfile::Custom(other.to_string()),
                });
            }
            let gate = BudgetGate {
                max_stack: t.max_stack,
                max_cycles: t.max_cycles,
                zero_alloc: t.zero_alloc.unwrap_or(false),
                ..Default::default()
            };
            if gate.max_stack.is_some() || gate.max_cycles.is_some() || gate.zero_alloc {
                attr.budget = gate;
            }
        }
        raw_cases = pa.test_cases.clone();
        for tc in &pa.test_cases {
            let mut vals = Vec::new();
            for arg in &tc.args {
                let s = match &arg.name {
                    None => attr_value_to_string(&arg.value),
                    Some(name) => format!("{name} = {}", attr_value_to_string(&arg.value)),
                };
                vals.push(s);
            }
            cases.push(TestCaseAttr {
                args: vals,
                label: None,
            });
        }
    }

    let params: Vec<TestParam> = func
        .params
        .iter()
        .map(|p| {
            let ty_str = format!("{}", p.ty);
            let is_capability = matches!(ty_str.as_str(), "SimStorageCtl" | "SimNetCtl")
                || ty_str.starts_with("Cap[");
            TestParam {
                name: p.name.clone(),
                ty: ty_str,
                is_capability,
            }
        })
        .collect();
    let effects: Vec<String> = func
        .effects
        .instances
        .iter()
        .map(|e| e.name.clone())
        .collect();

    let sig = TestSignature {
        name: func.name.clone(),
        params,
        effects,
        attr,
        cases,
    };
    (sig, raw_cases)
}

fn attr_value_to_string(v: &AttrValue) -> String {
    match v {
        AttrValue::Int(n) => n.to_string(),
        AttrValue::Float(f) => f.clone(),
        AttrValue::Bool(b) => b.to_string(),
        AttrValue::String(s) => format!("{s:?}"),
        AttrValue::Ident(id) => id.clone(),
        AttrValue::Raw(raw) => raw.clone(),
    }
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
    let (tokens, _) = lexer::lex(source);
    let parsed_attrs = test_attrs::extract_test_attrs(source);
    let attr_map: BTreeMap<&str, &FunctionTestAttrs> = parsed_attrs
        .iter()
        .map(|a| (a.fn_name.as_str(), a))
        .collect();

    let case_entries: BTreeMap<&str, &TestCase> = cases
        .iter()
        .map(|case| (case.wrapper.entry.as_str(), case))
        .collect();
    let case_targets: BTreeSet<&str> = cases
        .iter()
        .filter_map(|case| match &items[case.wrapper.function_index] {
            Item::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    let mut tests = Vec::new();
    for item in items {
        let Item::Function(f) = item else {
            continue;
        };
        let has_real_attr = attr_map
            .get(f.name.as_str())
            .is_some_and(|a| a.test.is_some() || !a.test_cases.is_empty());
        let case = case_entries.get(f.name.as_str());
        let legacy = !case_targets.contains(f.name.as_str())
            && (f.name.starts_with("test_")
                || has_preceding_test_comment(source, &tokens, f.span.start));
        if !has_real_attr && !legacy && case.is_none() {
            continue;
        }
        if !f.generics.is_empty() {
            return Err(format!("test function '{}' cannot be generic", f.name));
        }
        if f.body.is_none() {
            return Err(format!("test function '{}' must have a body", f.name));
        }
        let (mut sig, raw_cases) = syntax_attr_to_anvil(f, attr_map.get(f.name.as_str()).copied());
        if case.is_some() {
            // Each named row is an explicit unit invocation. Effect rows
            // on the wrapper still participate in ordinary type checking.
            sig.effects.clear();
        }
        let plan = classify(&sig);
        if !f.params.is_empty() && !has_real_attr {
            return Err(format!(
                "test function '{}' must take zero arguments",
                f.name
            ));
        }
        tests.push(DiscoveredTest {
            name: case.map_or_else(|| f.name.clone(), |case| case.name.clone()),
            span: case.map_or_else(|| f.span.clone(), |case| case.span.clone()),
            plan,
            raw_cases,
            func: (**f).clone(),
        });
    }
    tests.sort_by_key(|test| test.span.start);
    Ok(tests)
}

fn has_preceding_test_comment(
    source: &str,
    tokens: &[vesper::lexer::Token],
    fn_start: usize,
) -> bool {
    preceding_comments(tokens, fn_start)
        .any(|comment| is_test_marker(&source[comment.span.clone()]))
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

fn run_tests_on_world(
    source: &str,
    world: &World,
    selected: &[UnitEntry],
    fuel: u64,
    prefer_jit: bool,
) -> (bool, Vec<UnitTestCaseReport>) {
    let mut fallback: BTreeMap<&str, String> = Default::default();
    let mut batch = None;
    if prefer_jit && !selected.is_empty() {
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
    } else {
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
                if let Some(reason) = fallback.get(entry) {
                    let runtime = evaluator.get_or_insert_with(|| Runtime::new(world));
                    runtime.fuel = fuel;
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
    }
    (all_passed, cases)
}

fn without_negative_tests(mut module: Module, negative: &BTreeSet<String>) -> Module {
    module.items.retain(|item| match item {
        Item::Function(f) => !negative.contains(&f.name),
        _ => true,
    });
    module
}

fn checked_for_mutation(
    source: &str,
    negative: &BTreeSet<String>,
) -> Result<check::Checked, String> {
    let (parsed, cases) = prepare_tests(source)?;
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

/// A non-unit test the mutation gate replays against each mutant.
#[derive(Clone)]
enum ReplayTest {
    Property {
        name: String,
        cases: usize,
        explicit: Vec<Vec<Value>>,
    },
    SimPulse {
        name: String,
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
                cases,
                explicit,
            } => {
                let outcome = run_property_test(world, name, *cases, explicit, fuel);
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
                schedules,
                profile,
                seed,
            } => {
                let outcome = run_simpulse_test_on_world(world, name, *schedules, profile, *seed);
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

fn host_target_isa() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn run_unit_suite(opts: &Options) -> Result<(bool, Vec<FileUnitTests>), String> {
    let files = collect_files(Path::new(&opts.path))?;
    let mut results = Vec::new();
    let mut total_discovered = 0usize;
    let mut total_selected = 0usize;
    let mut all_passed = true;
    let verdict_cache = if !opts.no_cache && !opts.mutate {
        VerdictCache::open_default().ok()
    } else {
        None
    };

    for (display, source) in files {
        let (parsed, cases) = match prepare_tests(&source) {
            Ok(ok) => ok,
            Err(error) => {
                all_passed = false;
                results.push(FileUnitTests {
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
                });
                continue;
            }
        };
        let discovered = match discover_tests(&source, &parsed.module.items, &cases) {
            Ok(list) => list,
            Err(error) => {
                all_passed = false;
                results.push(FileUnitTests {
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
                });
                continue;
            }
        };
        total_discovered += discovered.len();
        // Negative tests are left out of the positive module whether or not
        // the filter selects them: their bodies are meant not to check.
        let expect_error_names: BTreeSet<String> = discovered
            .iter()
            .filter(|t| t.plan.primary_mode == TestMode::ExpectError)
            .map(|t| t.func.name.clone())
            .collect();
        let all_test_names: BTreeSet<String> =
            discovered.iter().map(|t| t.func.name.clone()).collect();
        let selected_tests: Vec<DiscoveredTest> = discovered
            .into_iter()
            .filter(|t| {
                opts.filter
                    .as_ref()
                    .is_none_or(|pat| t.name.contains(pat.as_str()))
            })
            .collect();
        total_selected += selected_tests.len();
        if selected_tests.is_empty() {
            continue;
        }

        // Filter out ExpectError functions before checking the positive module
        // so intentional substructural failures never poison sibling tests.
        let positive_module = without_negative_tests(parsed.module.clone(), &expect_error_names);
        let checked = match check::check(&positive_module) {
            c if c.diagnostics.is_empty() => c,
            c => {
                let d = &c.diagnostics[0];
                let error = test_diagnostic(&source, &cases, d, "semantic", c.diagnostics.len());
                all_passed = false;
                results.push(FileUnitTests {
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
                });
                continue;
            }
        };

        let fn_graph = FunctionGraph::from_module(&parsed.module);
        let mut case_reports: BTreeMap<String, UnitTestCaseReport> = BTreeMap::new();
        let mut unit_batch_selected = Vec::new();
        let mut unit_plans: BTreeMap<String, HarnessPlan> = BTreeMap::new();
        let mut replay: Vec<ReplayTest> = Vec::new();

        for test in &selected_tests {
            let (line, column) = location(&source, test.span.start);
            let fn_hash = fn_graph
                .transitive_fn_blake3(&test.func.name)
                .unwrap_or_default();
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
                && let Some(cache) = &verdict_cache
                && let Some(bytes) = cache.get(&cache_key.cache_key_hex())
                && let Ok(record) = VerdictRecord::from_json(&String::from_utf8_lossy(&bytes))
            {
                let passed = record.status == VerdictStatus::Pass;
                if !passed {
                    all_passed = false;
                }
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
                    let diags = checked.world.check_isolated_item(&test.func);
                    let observed: Vec<ObservedDiagnostic> = diags
                        .iter()
                        .map(|d| ObservedDiagnostic {
                            code: Some(d.code.clone()),
                            message: d.message.clone(),
                            primary_text: source
                                .get(d.span.start..d.span.end)
                                .unwrap_or("")
                                .to_string(),
                            related_texts: d
                                .related
                                .iter()
                                .map(|r| {
                                    let snippet =
                                        source.get(r.span.start..r.span.end).unwrap_or("");
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
                    if !passed {
                        all_passed = false;
                    }
                    if let Some(cache) = &verdict_cache
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
                            all_passed = false;
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
                    let outcome = run_property_test(
                        &checked.world,
                        &test.name,
                        cases_n,
                        &explicit,
                        opts.fuel,
                    );
                    let passed = outcome.passed;
                    if !passed {
                        all_passed = false;
                    } else {
                        replay.push(ReplayTest::Property {
                            name: test.name.clone(),
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
                    if let Some(cache) = &verdict_cache
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
                        &checked.world,
                        &test.name,
                        schedules,
                        profile,
                        test.plan.seed,
                    );
                    let passed = outcome.passed;
                    if !passed {
                        all_passed = false;
                    } else {
                        replay.push(ReplayTest::SimPulse {
                            name: test.name.clone(),
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
            let (unit_passed, unit_cases) = run_tests_on_world(
                &source,
                &checked.world,
                &unit_batch_selected,
                opts.fuel,
                !opts.mutate,
            );
            if !unit_passed {
                all_passed = false;
            }
            for mut rep in unit_cases {
                if let Some(plan) = unit_plans.get(&rep.name) {
                    rep.anvil_mode = Some(plan.primary_mode);
                    // The harness cannot measure stack, cycles or allocations
                    // yet, so a gated test is never reported as passing on the
                    // strength of its ordinary unit run.
                    if plan.primary_mode == TestMode::BudgetGate && rep.passed {
                        rep.passed = false;
                        all_passed = false;
                        rep.message = format!(
                            "budget gate not enforced: this harness cannot yet check {} (the unit run passed)",
                            budget_gate_names(plan)
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

        results.push(FileUnitTests {
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
        });
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

fn collect_file_mutants(unit: &FileUnitTests) -> Vec<MutantCandidate> {
    // Mutants are replayed against the file's unit and budget-gate tests
    // (`unit.selected`) and its passing property and SimPulse tests
    // (`unit.replay`). A file with none of those would report every
    // well-typed mutant as surviving without running a test, so it gets no
    // mutants.
    if unit.selected.is_empty() && unit.replay.is_empty() {
        return Vec::new();
    }
    let test_names: std::collections::BTreeSet<&str> =
        unit.test_names.iter().map(String::as_str).collect();
    let (tokens, _) = lexer::lex(&unit.source);
    let mut candidates = Vec::new();
    for item in &parser::parse(&unit.source).module.items {
        match item {
            Item::Function(f) => {
                if !test_names.contains(f.name.as_str())
                    && !f.name.starts_with("test_")
                    && !preceding_comments(&tokens, f.span.start).any(|comment| {
                        let text = &unit.source[comment.span.clone()];
                        is_test_marker(text) || case_marker(text).is_some()
                    })
                {
                    collect_mutants_in_fn(&unit.display, &unit.source, &tokens, f, &mut candidates);
                }
            }
            Item::Impl { methods, .. } => {
                for m in methods {
                    collect_mutants_in_fn(&unit.display, &unit.source, &tokens, m, &mut candidates);
                }
            }
            Item::Conformance(c) => {
                for m in &c.methods {
                    collect_mutants_in_fn(&unit.display, &unit.source, &tokens, m, &mut candidates);
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

    let mut all_candidates = Vec::new();
    let mut file_indices = Vec::new();
    for (file_idx, file) in files.iter().enumerate() {
        let mut cands = collect_file_mutants(file);
        for c in cands.drain(..) {
            file_indices.push(file_idx);
            all_candidates.push(c);
        }
    }
    let total_candidates = all_candidates.len();
    if total_candidates == 0 {
        return Err(format!(
            "no mutation sites found outside test functions in {} (mutants are replayed against unit, budget-gate, property and SimPulse tests)",
            opts.path
        ));
    }
    let capped = total_candidates > opts.max_mutants;
    let limit = total_candidates.min(opts.max_mutants);

    let mut reports = Vec::with_capacity(limit);
    let mut killed = 0usize;
    let mut survived = 0usize;
    let mut ill_typed = 0usize;

    for idx in 0..limit {
        let cand = &all_candidates[idx];
        let unit = &files[file_indices[idx]];
        let (line, column) = location(&unit.source, cand.start);
        let mut mutated = String::with_capacity(
            unit.source.len() - (cand.end - cand.start) + cand.replacement.len(),
        );
        mutated.push_str(&unit.source[..cand.start]);
        mutated.push_str(&cand.replacement);
        mutated.push_str(&unit.source[cand.end..]);

        let report = match checked_for_mutation(&mutated, &unit.expect_error_names) {
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
                let (unit_passed, cases) =
                    run_tests_on_world(&mutated, &checked.world, &unit.selected, opts.fuel, false);
                let mut killer = cases
                    .into_iter()
                    .find(|c| !c.passed)
                    .map(|k| (k.name, k.message));
                if unit_passed {
                    killer = unit
                        .replay
                        .iter()
                        .find_map(|t| t.run(&checked.world, opts.fuel).err());
                }
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
