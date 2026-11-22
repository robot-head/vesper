//! Test discovery of `vesper test` behind a swap point (self-hosting SH5
//! lane 14, #1337): the `@case` rows a source declares, and the tests of a
//! parsed module with their attributes and Anvil harness plans.
//!
//! The Rust implementation (moved here unchanged from `src/test_runner.rs`)
//! is the default [`TestDiscoveryTable`]. The hybrid Stage 2 build installs
//! `@vesper/test-runner` (packages/vesper-test-runner) in its place; the
//! runner calls [`case_rows`] and [`discover`] whichever table is installed.
//! tests/self_host_test_runner.rs runs both on the repository's corpus.

use crate::ast::{ExprKind, Function, Item, Module, Span};
use crate::lexer::{self, Token, TokenKind as K};
use crate::test_attrs::{self, AttrValue, FunctionTestAttrs};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use vesper_test::{
    BudgetGate, ExpectErrorSpec, FaultProfile, HarnessPlan, TestAttr, TestCaseAttr, TestParam,
    TestSignature, classify,
};

/// One `// @case name(arguments)` row: the test it names and the wrapper
/// function appended to the source that calls the target with its
/// arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseRow {
    /// `target::label`.
    pub name: String,
    /// The span of the row's comment.
    pub span: Span,
    /// The wrapper function's name (`__vesper_test_case_<n>`).
    pub entry: String,
    /// The wrapper's span in the expanded source.
    pub generated: Span,
    /// The item index of the target function.
    pub function_index: usize,
}

/// The `@case` rows of a source and, when there are any, the source with
/// every wrapper appended.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CaseRows {
    pub rows: Vec<CaseRow>,
    pub expanded: Option<String>,
}

/// One discovered test: its function, name, span, harness plan and the raw
/// `@test_case` pins (which the runner evaluates and fingerprints).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovered {
    /// The item index of the test function.
    pub index: usize,
    pub name: String,
    pub span: Span,
    pub plan: HarnessPlan,
    pub raw_cases: Vec<test_attrs::TestCaseAttr>,
}

/// The signature of [`discover`].
pub type DiscoverFn = fn(&str, &[Item], &[CaseRow]) -> Result<Vec<Discovered>, String>;

/// The swappable discovery functions.
#[derive(Clone, Copy)]
pub struct TestDiscoveryTable {
    /// [`case_rows`].
    pub case_rows: fn(&str, &Module) -> Result<CaseRows, String>,
    /// [`discover`].
    pub discover: DiscoverFn,
}

/// The Rust discovery, the default [`TestDiscoveryTable`].
pub const RUST_TEST_DISCOVERY: TestDiscoveryTable = TestDiscoveryTable {
    case_rows: rust_case_rows,
    discover: rust_discover,
};

static TABLE: std::sync::OnceLock<TestDiscoveryTable> = std::sync::OnceLock::new();

/// Replace [`RUST_TEST_DISCOVERY`] for the rest of the process. Only the
/// first call takes effect; a later one returns its table back.
pub fn install_test_discovery(table: TestDiscoveryTable) -> Result<(), TestDiscoveryTable> {
    TABLE.set(table)
}

fn table() -> &'static TestDiscoveryTable {
    TABLE.get().unwrap_or(&RUST_TEST_DISCOVERY)
}

/// The `@case` rows of `source`, whose parse is `module`, with the wrapper
/// of each appended to the expanded source; `Err` is the runner's message
/// for an invalid row.
pub fn case_rows(source: &str, module: &Module) -> Result<CaseRows, String> {
    (table().case_rows)(source, module)
}

/// The tests of `items` (the parse of the expanded source) in source order.
/// `source` is the original source and `cases` its [`case_rows`].
pub fn discover(
    source: &str,
    items: &[Item],
    cases: &[CaseRow],
) -> Result<Vec<Discovered>, String> {
    (table().discover)(source, items, cases)
}

/// The 1-based line and column (in chars) of byte `offset`.
pub fn location(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let prefix = &source[..source.floor_char_boundary(offset)];
    (
        prefix.bytes().filter(|b| *b == b'\n').count() + 1,
        prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
    )
}

/// Whether a line comment is a `// @test` marker.
pub fn is_test_marker(comment_text: &str) -> bool {
    let trimmed = comment_text.trim_start_matches('/').trim_start();
    trimmed == "@test"
        || trimmed.starts_with("@test ")
        || trimmed.starts_with("@test:")
        || trimmed.starts_with("@test(")
}

/// The row of a `// @case name(arguments)` comment.
pub fn case_marker(comment: &str) -> Option<&str> {
    let rest = comment
        .trim_start_matches('/')
        .trim_start()
        .strip_prefix("@case")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

/// The line comments directly before `fn_start`, nearest first.
pub fn preceding_comments(tokens: &[Token], fn_start: usize) -> impl Iterator<Item = &Token> {
    let end = tokens.partition_point(|t| t.span.end <= fn_start);
    tokens[..end]
        .iter()
        .rev()
        .take_while(|t| matches!(t.kind, K::Whitespace | K::Comment))
        .filter(|t| t.kind == K::Comment)
}

/// [`case_rows`] by the Rust implementation, whatever table is installed.
pub fn rust_case_rows(source: &str, module: &Module) -> Result<CaseRows, String> {
    let (tokens, _) = lexer::lex(source);
    let mut names: BTreeSet<&str> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    names.extend(
        module
            .uses
            .iter()
            .flat_map(|import| import.names.iter().map(|name| name.local())),
    );
    let mut rows_out = Vec::new();
    let mut expanded: Option<String> = None;
    let mut next_entry = 0;
    for (function_index, item) in module.items.iter().enumerate() {
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
            rows_out.push(CaseRow {
                name: format!("{}::{label}", f.name),
                span,
                entry,
                generated: start..expanded.len(),
                function_index,
            });
        }
    }
    Ok(CaseRows {
        rows: rows_out,
        expanded,
    })
}

/// Whether a function's body is the single direct call a `@case` wrapper
/// must parse as.
pub fn is_case_wrapper_body(f: &Function, target: &str) -> bool {
    matches!(
        f.body.as_ref().map(|body| &body.kind),
        Some(ExprKind::Block { statements, tail: Some(tail) })
            if statements.is_empty()
            && matches!(&tail.kind, ExprKind::Call { callee, .. }
                if matches!(&callee.kind, ExprKind::Name(name) if name == target))
    )
}

/// `@test(...)` and `@test_case(...)` of a function as an Anvil signature,
/// with the raw pins.
pub fn syntax_attr_to_anvil(
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
                    message: t.message.clone(),
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

/// An attribute value as the source spelling a `@test_case` pin reports.
pub fn attr_value_to_string(v: &AttrValue) -> String {
    match v {
        AttrValue::Int(n) => n.to_string(),
        AttrValue::Float(f) => f.clone(),
        AttrValue::Bool(b) => b.to_string(),
        AttrValue::String(s) => format!("{s:?}"),
        AttrValue::Ident(id) => id.clone(),
        AttrValue::Raw(raw) => raw.clone(),
    }
}

/// [`discover`] by the Rust implementation, whatever table is installed.
pub fn rust_discover(
    source: &str,
    items: &[Item],
    cases: &[CaseRow],
) -> Result<Vec<Discovered>, String> {
    let (tokens, _) = lexer::lex(source);
    let parsed_attrs = test_attrs::extract_test_attrs(source);
    let attr_map: BTreeMap<&str, &FunctionTestAttrs> = parsed_attrs
        .iter()
        .map(|a| (a.fn_name.as_str(), a))
        .collect();

    let case_entries: BTreeMap<&str, &CaseRow> = cases
        .iter()
        .map(|case| (case.entry.as_str(), case))
        .collect();
    let case_targets: BTreeSet<&str> = cases
        .iter()
        .filter_map(|case| match &items[case.function_index] {
            Item::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    let mut tests = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Item::Function(f) = item else {
            continue;
        };
        let has_real_attr = attr_map
            .get(f.name.as_str())
            .is_some_and(|a| a.test.is_some() || !a.test_cases.is_empty());
        let case = case_entries.get(f.name.as_str());
        let legacy = !case_targets.contains(f.name.as_str())
            && (f.name.starts_with("test_")
                || preceding_comments(&tokens, f.span.start)
                    .any(|comment| is_test_marker(&source[comment.span.clone()])));
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
        tests.push(Discovered {
            index,
            name: case.map_or_else(|| f.name.clone(), |case| case.name.clone()),
            span: case.map_or_else(|| f.span.clone(), |case| case.span.clone()),
            plan,
            raw_cases,
        });
    }
    tests.sort_by_key(|test| test.span.start);
    Ok(tests)
}
