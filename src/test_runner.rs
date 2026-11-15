use serde::Serialize;
use std::{fs::OpenOptions, io::Read, path::Path};
use vesper::{
    ast::{Expr, ExprKind, Function, Item, Literal, Span, Statement},
    check::{self, World},
    lexer::{self, TokenKind as K},
    native::batch::{BatchLimits, NativeBatch},
    parser,
    runtime::{Runtime, Value},
    tooling::doc::sources::{SourceLimits, discover},
};

const MAX_SOURCE_BYTES: usize = 512 * 1024;
const DEFAULT_FUEL: u64 = 1_000_000;
const DEFAULT_MAX_MUTANTS: usize = 256;
const HARD_MAX_MUTANTS: usize = 2048;

pub const USAGE: &str = "usage: vesper test --doc [FILE|DIR] [--json]\n       vesper test <FILE|DIR> [--mutate] [--filter <PAT>] [--max-mutants <N>] [--fuel <N>] [--json]";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestEngine {
    Jit,
    Evaluator,
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
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--mutate" if !mutate => mutate = true,
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

fn discover_tests(source: &str, items: &[Item]) -> Result<Vec<(String, Span)>, String> {
    let (tokens, _) = lexer::lex(source);
    let mut tests = Vec::new();
    for item in items {
        let Item::Function(f) = item else {
            continue;
        };
        let marked = has_preceding_test_comment(source, &tokens, f.span.start);
        let named = f.name.starts_with("test_");
        if !marked && !named {
            continue;
        }
        if !f.generics.is_empty() {
            return Err(format!("test function '{}' cannot be generic", f.name));
        }
        if !f.params.is_empty() {
            return Err(format!(
                "test function '{}' must take zero arguments",
                f.name
            ));
        }
        if f.body.is_none() {
            return Err(format!("test function '{}' must have a body", f.name));
        }
        tests.push((f.name.clone(), f.span.clone()));
    }
    Ok(tests)
}

fn has_preceding_test_comment(
    source: &str,
    tokens: &[vesper::lexer::Token],
    fn_start: usize,
) -> bool {
    let mut idx = tokens.partition_point(|t| t.span.end <= fn_start);
    while idx > 0 {
        idx -= 1;
        let tok = &tokens[idx];
        match tok.kind {
            K::Whitespace => continue,
            K::Comment => {
                if is_test_marker(&source[tok.span.clone()]) {
                    return true;
                }
            }
            _ => break,
        }
    }
    false
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

fn run_tests_on_world(
    source: &str,
    world: &World,
    selected: &[(String, Span)],
    fuel: u64,
    prefer_jit: bool,
) -> (bool, Vec<UnitTestCaseReport>) {
    let mut fallback: std::collections::BTreeMap<&str, String> = Default::default();
    let mut batch = None;
    if prefer_jit && !selected.is_empty() {
        let names: Vec<&str> = selected.iter().map(|(n, _)| n.as_str()).collect();
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
        for (name, _) in selected {
            fallback.insert(name.as_str(), "mutation gate uses evaluator".into());
        }
    }

    let mut evaluator = None;
    let mut cases = Vec::with_capacity(selected.len());
    let mut all_passed = true;
    for (name, span) in selected {
        let (line, column) = location(source, span.start);
        let (passed, result, message, engine, fallback_reason) =
            if let Some(reason) = fallback.get(name.as_str()) {
                let runtime = evaluator.get_or_insert_with(|| Runtime::new(world));
                runtime.fuel = fuel;
                let (passed, result, message) = evaluate_outcome(runtime.run(name, vec![]));
                (
                    passed,
                    result,
                    message,
                    TestEngine::Evaluator,
                    prefer_jit.then(|| reason.clone()),
                )
            } else {
                let b = batch.as_mut().expect("jit batch available");
                let (passed, result, message) = evaluate_outcome(b.run(name));
                (passed, result, message, TestEngine::Jit, None)
            };
        if !passed {
            all_passed = false;
        }
        cases.push(UnitTestCaseReport {
            name: name.clone(),
            line,
            column,
            passed,
            result,
            message,
            engine,
            fallback_reason,
        });
    }
    (all_passed, cases)
}

fn parse_module(source: &str) -> Result<parser::Parse, String> {
    let parsed = parser::parse(source);
    if let Some(d) = parsed.diagnostics.first() {
        return Err(format!(
            "{} syntax error(s); first: {} {}",
            parsed.diagnostics.len(),
            d.code,
            d.message
        ));
    }
    Ok(parsed)
}

fn check_parsed(parsed: &parser::Parse) -> Result<check::Checked, String> {
    let checked = check::check(&parsed.module);
    if let Some(d) = checked.diagnostics.first() {
        return Err(format!(
            "{} semantic error(s); first: {} {}",
            checked.diagnostics.len(),
            d.code,
            d.message
        ));
    }
    Ok(checked)
}

fn check_module(source: &str) -> Result<(parser::Parse, check::Checked), String> {
    let parsed = parse_module(source)?;
    let checked = check_parsed(&parsed)?;
    Ok((parsed, checked))
}

struct FileUnitTests {
    display: String,
    source: String,
    report: UnitTestFileReport,
    selected: Vec<(String, Span)>,
    parsed_items: Vec<Item>,
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

fn run_unit_suite(opts: &Options) -> Result<(bool, Vec<FileUnitTests>), String> {
    let files = collect_files(Path::new(&opts.path))?;
    let mut results = Vec::new();
    let mut total_discovered = 0usize;
    let mut total_selected = 0usize;
    let mut all_passed = true;

    for (display, source) in files {
        let parsed = match parse_module(&source) {
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
                    parsed_items: vec![],
                });
                continue;
            }
        };
        let discovered = match discover_tests(&source, &parsed.module.items) {
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
                    parsed_items: vec![],
                });
                continue;
            }
        };
        total_discovered += discovered.len();
        let selected: Vec<(String, Span)> = discovered
            .into_iter()
            .filter(|(name, _)| {
                opts.filter
                    .as_ref()
                    .is_none_or(|pat| name.contains(pat.as_str()))
            })
            .collect();
        total_selected += selected.len();
        if selected.is_empty() {
            continue;
        }
        let checked = match check_parsed(&parsed) {
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
                    parsed_items: vec![],
                });
                continue;
            }
        };
        let (passed, cases) =
            run_tests_on_world(&source, &checked.world, &selected, opts.fuel, true);
        if !passed {
            all_passed = false;
        }
        results.push(FileUnitTests {
            display: display.clone(),
            source,
            report: UnitTestFileReport {
                file: display,
                passed,
                tests: cases,
                error: None,
            },
            selected,
            parsed_items: parsed.module.items,
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
    let test_names: std::collections::BTreeSet<&str> =
        unit.selected.iter().map(|(n, _)| n.as_str()).collect();
    let (tokens, _) = lexer::lex(&unit.source);
    let mut candidates = Vec::new();
    for item in &unit.parsed_items {
        match item {
            Item::Function(f) => {
                if !test_names.contains(f.name.as_str())
                    && !f.name.starts_with("test_")
                    && !has_preceding_test_comment(&unit.source, &tokens, f.span.start)
                {
                    collect_mutants_in_fn(&unit.display, &unit.source, &tokens, f, &mut candidates);
                }
            }
            Item::Impl { methods, .. } => {
                for m in methods {
                    collect_mutants_in_fn(&unit.display, &unit.source, &tokens, m, &mut candidates);
                }
            }
            Item::Adt(_) | Item::Effect(_) | Item::Trait(_) | Item::Conformance(_) => {}
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
            "no mutation sites found outside test functions in {}",
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

        let report = match check_module(&mutated) {
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
            Ok((_, checked)) => {
                let (passed, cases) =
                    run_tests_on_world(&mutated, &checked.world, &unit.selected, opts.fuel, false);
                if passed {
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
                        detail: Some("all unit tests still passed".into()),
                    }
                } else {
                    killed += 1;
                    let killer = cases.into_iter().find(|c| !c.passed);
                    MutantReport {
                        index: idx,
                        file: cand.file.clone(),
                        function: cand.function.clone(),
                        line,
                        column,
                        original: cand.original.clone(),
                        replacement: cand.replacement.clone(),
                        status: MutantStatus::Killed,
                        killed_by: killer.as_ref().map(|k| k.name.clone()),
                        detail: killer.map(|k| k.message),
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
