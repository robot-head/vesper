//! Checked finite source kernels measured through the owned native batch.
//! Compilation/setup are outside windows; Rust dispatch/result overhead remains.
use serde_json::{Value as Json, json};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};
use vesper::{
    ast::{ExprKind, Item, Literal, Module, PatternKind, Statement, Type},
    lexer::{self, TokenKind as K},
    native::batch::{BatchLimits, BatchStats, NativeBatch},
    runtime::Value,
};
use vesper_bench::{
    Analysis, BenchCase, Decision, Invocation, PairedSample, RunConfig, analyze_pairs, run_abba,
};

const MAX_SOURCE_BYTES: u64 = 256 * 1024;
const MAX_CALLS: usize = 1_000_000;
const HELP: &str = r#"Usage: vesper bench FILE [options]

Compile checked finite scalar source kernels before paired A/B/B/A measurement.
The default baseline is a separately compiled copy of FILE.

  --baseline FILE          Local baseline source (no git/CAS resolution)
  --entry NAME             Candidate no-argument entry (default: main)
  --baseline-entry NAME    Baseline entry (default: candidate entry)
  --samples N              Measured epochs, 1..64 (default: 12)
  --warmup N               Report-excluded warmup epochs, 0..64 (default: 1)
  --iterations N           Calls per measured window, 1..65536 (default: 128)
  --max-regression-bp N     Apply the exact paired-median 95% confidence gate
  --json                   Machine-readable report, including failures
  -h, --help               Show help

Inputs: bounded regular UTF-8 files containing ordinary nongeneric pure scalar
functions. Recursive/indirect/aggregate/imported/fallible kernels are unsupported.
Measurements: std::time::Instant nanoseconds, including Rust/JIT dispatch, loop
and result-storage overhead. No PMU, allocation, SIMD, layout-randomization or
causal-profile evidence is inferred. The elapsed budget is checked between
windows, not a hard timeout. Exact large ratio bounds are decimal JSON strings.
Without an explicit threshold this command measures; it does not enforce a gate.
"#;

struct Options {
    path: PathBuf,
    baseline: PathBuf,
    entry: String,
    baseline_entry: String,
    config: RunConfig,
    threshold: Option<i64>,
}
fn argument(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}
fn count(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<usize, String> {
    argument(args, flag)?
        .parse()
        .map_err(|_| format!("{flag} requires a nonnegative integer"))
}
fn options(args: Vec<String>) -> Result<Option<Options>, String> {
    let mut args = args.into_iter();
    let mut path = None;
    let mut baseline = None;
    let mut entry = String::from("main");
    let mut baseline_entry = None;
    let mut config = RunConfig::default();
    let mut threshold = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--json" => {}
            "--baseline" => baseline = Some(PathBuf::from(argument(&mut args, "--baseline")?)),
            "--entry" => entry = argument(&mut args, "--entry")?,
            "--baseline-entry" => {
                baseline_entry = Some(argument(&mut args, "--baseline-entry")?);
            }
            "--samples" => config.epochs = count(&mut args, "--samples")?,
            "--warmup" => config.warmup_epochs = count(&mut args, "--warmup")?,
            "--iterations" => config.operations_per_window = count(&mut args, "--iterations")?,
            "--max-regression-bp" => {
                let value = argument(&mut args, "--max-regression-bp")?
                    .parse::<i64>()
                    .map_err(|_| "--max-regression-bp requires an integer")?;
                if value < -9999 {
                    return Err("--max-regression-bp must be at least -9999".into());
                }
                threshold = Some(value);
            }
            _ if arg.starts_with('-') => return Err(format!("unknown bench option '{arg}'")),
            _ => {
                if path.replace(PathBuf::from(arg)).is_some() {
                    return Err(
                        "bench accepts one candidate file; use --baseline for comparison".into(),
                    );
                }
            }
        }
    }
    let path = path.ok_or("bench requires a source file; see bench --help")?;
    if config.epochs == 0
        || config.epochs > 64
        || config.warmup_epochs > 64
        || config.operations_per_window == 0
        || config.operations_per_window > 65_536
    {
        return Err("bench samples/warmup/iterations exceed the documented limits".into());
    }
    let calls = config
        .epochs
        .checked_add(config.warmup_epochs)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| n.checked_mul(config.operations_per_window))
        .ok_or("bench call-count overflow")?;
    if calls > MAX_CALLS {
        return Err("bench exceeds the one-million total-call limit".into());
    }
    config.max_total_calls = MAX_CALLS;
    Ok(Some(Options {
        baseline: baseline.unwrap_or_else(|| path.clone()),
        path,
        baseline_entry: baseline_entry.unwrap_or_else(|| entry.clone()),
        entry,
        config,
        threshold,
    }))
}
fn read_source(path: &Path) -> Result<String, String> {
    let regular = || {
        format!(
            "{}: expected a regular source file without final symlinks",
            path.display()
        )
    };
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err(regular());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err(regular());
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err("bench source exceeds 256 KiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err("bench source exceeds 256 KiB".into());
    }
    String::from_utf8(bytes).map_err(|_| "bench source must be UTF-8".into())
}
fn source_guard(source: &str) -> Result<(), String> {
    let (tokens, errors) = lexer::lex(source);
    if !errors.is_empty() {
        return Err("bench source has lexical diagnostics".into());
    }
    if tokens.len() > 16_384 {
        return Err("bench source token limit".into());
    }
    let (mut depth, mut functions, mut conditionals, mut segment) =
        (0usize, 0usize, 0usize, 0usize);
    for token in tokens {
        if token.kind.trivia() || token.kind == K::Eof {
            continue;
        }
        match token.kind {
            K::LBrace | K::LBracket | K::LParen => {
                depth += 1;
                if depth > 32 {
                    return Err("bench source nesting limit".into());
                }
            }
            K::RBrace | K::RBracket | K::RParen => depth = depth.saturating_sub(1),
            _ => {}
        }
        functions += usize::from(token.kind == K::Fn);
        conditionals += usize::from(token.kind == K::If);
        if functions > 128 || conditionals > 64 {
            return Err("bench source function/conditional limit".into());
        }
        if matches!(token.kind, K::Semi | K::LBrace | K::RBrace) {
            segment = 0;
        } else {
            segment += 1;
            if segment > 256 {
                return Err("bench source expression/signature limit".into());
            }
        }
    }
    Ok(())
}
fn primitive(ty: &Type) -> bool {
    *ty == Type::unit()
        || matches!(
            ty,
            Type::Named(name, args)
                if args.is_empty()
                    && matches!(name.as_str(), "i64" | "u8" | "u32" | "u64" | "f64" | "bool" | "Unit")
        )
}
fn scalar_source(module: &Module) -> Result<(), String> {
    if !module.model_uses.is_empty() || !module.trait_bounds.is_empty() {
        return Err("bench source models/trait constraints are unsupported".into());
    }
    let mut bodies = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for item in &module.items {
        let Item::Function(function) = item else {
            return Err(
                "bench currently supports ordinary scalar function declarations only".into(),
            );
        };
        if !function.generics.is_empty()
            || function.body.is_none()
            || !function.effects.instances.is_empty()
            || function.effects.tail.is_some()
            || !primitive(&function.result)
            || function.params.iter().any(|p| !primitive(&p.ty))
        {
            return Err("bench requires nongeneric pure primitive-scalar signatures".into());
        }
        names.insert(function.name.as_str());
        bodies.push((function.body.as_ref().expect("body checked"), 1usize));
    }
    // The initial kernel profile excludes inferred composite growth before the
    // unchanged checker. Byte/depth limits alone cannot bound repeated (x,x).
    const MAX_NODES: usize = 4096;
    let mut visited = 0usize;
    while let Some((expr, depth)) = bodies.pop() {
        visited += 1;
        if visited > MAX_NODES || depth > 32 {
            return Err("bench scalar AST volume/depth limit".into());
        }
        let mut add = |child| -> Result<(), String> {
            if visited + bodies.len() >= MAX_NODES {
                return Err("bench scalar AST volume limit".into());
            }
            bodies.push((child, depth + 1));
            Ok(())
        };
        match &expr.kind {
            ExprKind::Literal(Literal::String(_)) => {
                return Err("bench string/aggregate expressions are unsupported".into());
            }
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
            ExprKind::Tuple(values) if values.is_empty() => {}
            ExprKind::Unary(_, value) => add(value)?,
            ExprKind::Binary(_, left, right) | ExprKind::Assign(_, left, right) => {
                add(left)?;
                add(right)?;
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let ExprKind::Name(name) = &callee.kind else {
                    return Err("bench indirect/method calls are unsupported".into());
                };
                if !types.is_empty()
                    || (!names.contains(name.as_str())
                        && vesper::numeric::wrapping_intrinsic(name).is_none())
                {
                    return Err("bench generic/constructor/unknown calls are unsupported".into());
                }
                add(callee)?;
                for argument in args {
                    add(&argument.expr)?;
                }
            }
            ExprKind::Block { statements, tail } => {
                for statement in statements {
                    match statement {
                        Statement::Let { pat, ty, value } => {
                            if !matches!(pat.kind, PatternKind::Bind(_) | PatternKind::Wild)
                                || (*ty != Type::Infer && !primitive(ty))
                            {
                                return Err(
                                    "bench composite patterns/annotations are unsupported".into()
                                );
                            }
                            add(value)?;
                        }
                        Statement::Expr(value) => add(value)?,
                    }
                }
                if let Some(tail) = tail {
                    add(tail)?;
                }
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                add(condition)?;
                add(then)?;
                if let Some(otherwise) = otherwise {
                    add(otherwise)?;
                }
            }
            _ => return Err("bench composite/closure/handler expressions are unsupported".into()),
        }
    }
    Ok(())
}
struct SourceCase {
    runner: NativeBatch,
    entry: String,
    source_id: String,
}
impl SourceCase {
    fn compile(path: &Path, entry: &str) -> Result<Self, String> {
        let source = read_source(path)?;
        source_guard(&source)?;
        let parsed = vesper::parser::parse(&source);
        if let Some(error) = parsed.diagnostics.first() {
            return Err(format!(
                "{} at {:?}: {}",
                error.code, error.span, error.message
            ));
        }
        scalar_source(&parsed.module)?;
        let checked = vesper::check::check(&parsed.module);
        if let Some(error) = checked.diagnostics.first() {
            return Err(format!(
                "{} at {:?}: {}",
                error.code, error.span, error.message
            ));
        }
        let runner = NativeBatch::compile(&checked.world, &[entry], BatchLimits::default())?;
        Ok(Self {
            runner,
            entry: entry.to_owned(),
            source_id: blake3::hash(source.as_bytes()).to_hex().to_string(),
        })
    }
}
impl BenchCase for SourceCase {
    type Input = ();
    type Output = Value;
    type Error = String;
    fn setup(&mut self, _: Invocation) -> Result<(), String> {
        Ok(())
    }
    fn run(&mut self, _: ()) -> Result<Value, String> {
        self.runner.run(&self.entry)
    }
}
fn pair_json(pair: PairedSample) -> Json {
    let bounds = pair.regression_bp_bounds();
    json!({
        "baseline_total_ns": pair.baseline_total().to_string(),
        "candidate_total_ns": pair.candidate_total().to_string(),
        "operations_per_arm": pair.operations_per_arm(),
        "regression_basis_points": {"floor": bounds.floor.to_string(), "ceil": bounds.ceil.to_string()},
    })
}
fn stats_json(stats: BatchStats) -> Json {
    json!({
        "modules": stats.module_count, "compilations": stats.compilation_count,
        "defined_functions": stats.defined_functions, "entries": stats.entry_count,
        "runs": stats.runs, "max_entry_work": stats.max_entry_work,
    })
}
fn analysis_json(analysis: &Analysis, threshold: Option<i64>) -> Json {
    let interval = analysis.interval.map(|interval| {
        json!({
            "lower": pair_json(interval.lower), "upper": pair_json(interval.upper),
            "lower_rank": interval.lower_rank, "upper_rank": interval.upper_rank,
            "coverage_numerator": interval.covered_outcomes.to_string(),
            "coverage_denominator": interval.total_outcomes.to_string(),
        })
    });
    let reason = if threshold.is_some() || analysis.interval.is_none() {
        analysis.reason.map(|reason| format!("{reason:?}"))
    } else {
        None
    };
    json!({
        "method": "exact binomial order-statistic population-median interval",
        "nominal_confidence_percent": 95,
        "assumption": "independent identically distributed epoch ratios; ABBA does not prove drift cancellation",
        "median_lower": pair_json(analysis.median_lower),
        "median_upper": pair_json(analysis.median_upper),
        "interval": interval, "threshold_bp": threshold,
        "gate_decision": threshold.map(|_| format!("{:?}", analysis.decision)),
        "inconclusive_reason": reason,
    })
}
struct Outcome {
    data: Json,
    text: String,
    gate_failure: Option<String>,
}
fn execute(args: Vec<String>) -> Result<Option<Outcome>, String> {
    let Some(options) = options(args)? else {
        return Ok(None);
    };
    // Both compilation phases precede run_abba and every timed window.
    let mut baseline = SourceCase::compile(&options.baseline, &options.baseline_entry)
        .map_err(|error| format!("baseline {}: {error}", options.baseline.display()))?;
    let mut candidate = SourceCase::compile(&options.path, &options.entry)
        .map_err(|error| format!("candidate {}: {error}", options.path.display()))?;
    if options.path == options.baseline && baseline.source_id != candidate.source_id {
        return Err("source changed while preparing the same-file comparison".into());
    }
    let report =
        run_abba(options.config, &mut baseline, &mut candidate).map_err(|e| e.to_string())?;
    let analysis =
        analyze_pairs(report.pairs(), options.threshold.unwrap_or(0)).map_err(|e| e.to_string())?;
    let gate_failure = options.threshold.and_then(|_| match analysis.decision {
        Decision::Pass => None,
        Decision::Regression => Some("benchmark confidence gate detected a regression".into()),
        Decision::Inconclusive => Some("benchmark confidence gate is inconclusive".into()),
    });
    let status = match (options.threshold, analysis.decision) {
        (None, _) => "measured",
        (Some(_), Decision::Pass) => "passed",
        (Some(_), Decision::Regression) => "regression",
        (Some(_), Decision::Inconclusive) => "inconclusive",
    };
    let median = analysis.median_upper.regression_bp_bounds();
    let text = format!(
        "{} vs {}: {} paired epochs, {} body calls, provider {} ({})\nmedian upper ratio: {}..{} basis points; {}\nincludes Rust/JIT dispatch and storage overhead; compilation/setup are outside windows\n",
        options.path.display(),
        options.baseline.display(),
        report.epochs().len(),
        report.total_body_calls(),
        report.provider().as_str(),
        report.unit().as_str(),
        median.floor,
        median.ceil,
        status,
    );
    let data = json!({
        "schema": "vesper.bench/1", "status": status,
        "provider": report.provider().as_str(), "unit": report.unit().as_str(),
        "candidate": {"path": options.path, "entry": options.entry, "source_blake3": candidate.source_id, "jit": stats_json(candidate.runner.stats())},
        "baseline": {"path": options.baseline, "entry": options.baseline_entry, "source_blake3": baseline.source_id, "jit": stats_json(baseline.runner.stats())},
        "config": {"epochs": options.config.epochs, "warmup_epochs": options.config.warmup_epochs, "operations_per_window": options.config.operations_per_window},
        "body_calls": report.total_body_calls(), "elapsed_run_ns": report.elapsed_run_nanos().to_string(),
        "pairs": report.pairs().iter().copied().map(pair_json).collect::<Vec<_>>(),
        "analysis": analysis_json(&analysis, options.threshold),
        "evidence": {"hardware_pmu": "unavailable", "zero_allocation": "unavailable", "vectorization": "unavailable"},
        "scope": "unoptimized finite scalar JIT kernels; measured windows include Rust dispatch/loop/output storage; no hard deadline or sandbox guarantee",
    });
    Ok(Some(Outcome {
        data,
        text,
        gate_failure,
    }))
}
pub fn run(args: Vec<String>) -> Result<(), String> {
    let json_output = args.iter().any(|arg| arg == "--json");
    let outcome = match execute(args) {
        Ok(outcome) => outcome,
        Err(error) => {
            if json_output {
                println!(
                    "{}",
                    json!({"schema":"vesper.bench/1","status":"error","error":error})
                );
            }
            return Err(error);
        }
    };
    let Some(outcome) = outcome else {
        return Ok(());
    };
    if json_output {
        println!("{}", outcome.data);
    } else {
        print!("{}", outcome.text);
    }
    outcome.gate_failure.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inferred_growth_is_rejected_without_calling_the_checker() {
        let mut source = String::from("fn main(){let x=0;");
        for _ in 0..25 {
            source.push_str("let x=(x,x);");
        }
        source.push('}');
        let parsed = vesper::parser::parse(&source);
        assert!(parsed.diagnostics.is_empty());
        assert!(scalar_source(&parsed.module).is_err());
        let generic =
            vesper::parser::parse("fn dup[T:mod copy](in x:T)->(T,T){(x,x)} fn main(){dup(0);}");
        assert!(generic.diagnostics.is_empty());
        assert!(scalar_source(&generic.module).is_err());
    }
}
