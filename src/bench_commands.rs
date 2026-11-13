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
    Analysis, Arm, BenchCase, BodyStatus, ClockStatus, Decision, Invocation, ObservedRunError,
    ObservedWindow, PairedSample, RunConfig, RunError, RunReport, WindowCompletion, WindowObserver,
    analyze_pairs, run_abba, run_abba_observed,
};

use vesper_bench::layout::{
    ENV_PADDING_VAR, HeapShim, LayoutPlan, LayoutVerdict, MAX_LAYOUT_RUNS, MIN_LAYOUT_RUNS,
    env_padding_value, layout_plan, layout_survival, with_stack_padding,
};
use vesper_bench_pmu::{
    CounterDelta, KpcCounters, PerfGroup, Provider, RdpmcCounters, Unavailable, host_pmu_status,
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
  --pmu                    Strictly require real hardware counter observations
  --pmu-provider group|rdpmc|kpc
                           Counter mechanism (implies --pmu). group: Linux
                           perf_event_open group-read syscalls (default);
                           rdpmc: Linux x86_64 user-space RDPMC via the perf
                           mmap page; kpc: macOS AArch64 kperf fixed counters,
                           root only
  --metric time|cycles|instructions
                           Selected metric (default: time); counters imply --pmu
  --max-regression-bp N     Gate the SELECTED metric with the paired-median 95% CI
  --randomize-layout N     Stabilizer-style: run N (6..32) child processes, each
                           with its own heap offset, stack padding and
                           environment size, and report whether the A/B
                           difference survives layout randomization
  --layout-seed N          Seed for --randomize-layout (default: 0)
  --json                   Machine-readable report, including failures
  -h, --help               Show help

Inputs: bounded regular UTF-8 files containing ordinary nongeneric pure scalar
functions. Recursive/indirect/aggregate/imported/fallible kernels are unsupported.
Measurements: std::time::Instant nanoseconds, including Rust/JIT dispatch, loop
and result-storage overhead. Optional PMU counts include clock/observer dispatch boundary overhead.
Unavailable PMU is inconclusive with no clock fallback and reports why (for
example no hardware PMU device, perf_event_paranoid, or kpc without root).
PMU availability supplies no evidence about allocation or SIMD.
Layout randomization moves data and stack placement, not machine code. Causal
profiling is a library API (vesper_bench::causal) for instrumented host
threads; this command does not run it.
The elapsed budget is checked between
windows, not a hard timeout. Exact large ratio bounds are decimal JSON strings.
Without an explicit threshold this command measures; it does not enforce a gate.
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Metric {
    Time,
    Cycles,
    Instructions,
}
impl Metric {
    fn name(self) -> &'static str {
        match self {
            Self::Time => "time",
            Self::Cycles => "cycles",
            Self::Instructions => "instructions",
        }
    }
    fn provider(self, pmu: Provider) -> &'static str {
        if self == Self::Time {
            "std::time::Instant"
        } else {
            pmu.as_str()
        }
    }
    fn unit(self) -> &'static str {
        match self {
            Self::Time => "nanoseconds",
            Self::Cycles => "cycles",
            Self::Instructions => "instructions",
        }
    }
}

struct Options {
    path: PathBuf,
    baseline: PathBuf,
    entry: String,
    baseline_entry: String,
    config: RunConfig,
    threshold: Option<i64>,
    metric: Metric,
    pmu: bool,
    provider: Provider,
    layouts: Option<usize>,
    layout_seed: u64,
    layout_run: Option<(u64, u32)>,
    forwarded: Vec<String>,
}
/// Plain `--pmu` keeps meaning the group-read provider on every platform;
/// RDPMC and kpc are explicit opt-ins.
fn default_provider() -> Provider {
    Provider::LinuxGroupRead
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
    let mut forwarded = Vec::new();
    let mut args = args.into_iter();
    let mut path = None;
    let mut provider = None;
    let mut layouts = None;
    let mut layout_seed = 0u64;
    let mut layout_run = None;
    let mut baseline = None;
    let mut entry = String::from("main");
    let mut baseline_entry = None;
    let mut config = RunConfig::default();
    let mut threshold = None;
    let mut metric = Metric::Time;
    let mut pmu = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--json" => {}
            "--pmu" => {
                pmu = true;
                forwarded.push(arg);
            }
            "--pmu-provider" => {
                let value = argument(&mut args, "--pmu-provider")?;
                provider = Some(match value.as_str() {
                    "group" => Provider::LinuxGroupRead,
                    "rdpmc" => Provider::LinuxRdpmc,
                    "kpc" => Provider::MacosKpc,
                    _ => return Err("--pmu-provider requires group, rdpmc or kpc".into()),
                });
                forwarded.extend([arg, value]);
            }
            "--randomize-layout" => {
                let n = count(&mut args, "--randomize-layout")?;
                if !(MIN_LAYOUT_RUNS..=32).contains(&n) {
                    return Err("--randomize-layout requires 6..32 layouts".into());
                }
                layouts = Some(n);
            }
            "--layout-seed" => {
                layout_seed = argument(&mut args, "--layout-seed")?
                    .parse()
                    .map_err(|_| "--layout-seed requires a nonnegative integer")?;
            }
            // Internal: one child of --randomize-layout.
            "--layout-run" => {
                let value = argument(&mut args, "--layout-run")?;
                let (seed, run) = value
                    .split_once(':')
                    .and_then(|(s, r)| Some((s.parse().ok()?, r.parse().ok()?)))
                    .ok_or("--layout-run requires SEED:RUN")?;
                if run as usize >= MAX_LAYOUT_RUNS {
                    return Err("--layout-run index out of range".into());
                }
                layout_run = Some((seed, run));
            }
            "--metric" => {
                let value = argument(&mut args, "--metric")?;
                metric = match value.as_str() {
                    "time" => Metric::Time,
                    "cycles" => Metric::Cycles,
                    "instructions" => Metric::Instructions,
                    _ => return Err("--metric requires time, cycles or instructions".into()),
                };
                forwarded.extend([arg, value]);
            }
            "--baseline" => {
                let value = argument(&mut args, "--baseline")?;
                baseline = Some(PathBuf::from(&value));
                forwarded.extend([arg, value]);
            }
            "--entry" => {
                entry = argument(&mut args, "--entry")?;
                forwarded.extend([arg, entry.clone()]);
            }
            "--baseline-entry" => {
                let value = argument(&mut args, "--baseline-entry")?;
                baseline_entry = Some(value.clone());
                forwarded.extend([arg, value]);
            }
            "--samples" | "--warmup" | "--iterations" => {
                let n = count(&mut args, &arg)?;
                match arg.as_str() {
                    "--samples" => config.epochs = n,
                    "--warmup" => config.warmup_epochs = n,
                    _ => config.operations_per_window = n,
                }
                forwarded.extend([arg, n.to_string()]);
            }
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
                forwarded.push(arg.clone());
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
        metric,
        pmu: pmu || provider.is_some() || metric != Metric::Time,
        provider: provider.unwrap_or_else(default_provider),
        layouts: if layout_run.is_some() { None } else { layouts },
        layout_seed,
        layout_run,
        forwarded,
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
fn pair_json(pair: PairedSample, metric: Metric) -> Json {
    let bounds = pair.regression_bp_bounds();
    let baseline_key = format!(
        "baseline_total_{}",
        if metric == Metric::Time {
            "ns"
        } else {
            metric.name()
        }
    );
    let candidate_key = format!(
        "candidate_total_{}",
        if metric == Metric::Time {
            "ns"
        } else {
            metric.name()
        }
    );
    json!({
        baseline_key: pair.baseline_total().to_string(),
        candidate_key: pair.candidate_total().to_string(),
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
fn analysis_json(analysis: &Analysis, threshold: Option<i64>, metric: Metric) -> Json {
    let interval = analysis.interval.map(|interval| {
        json!({
            "lower": pair_json(interval.lower, metric), "upper": pair_json(interval.upper, metric),
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
        "median_lower": pair_json(analysis.median_lower, metric),
        "median_upper": pair_json(analysis.median_upper, metric),
        "interval": interval, "threshold_bp": threshold,
        "gate_decision": threshold.map(|_| format!("{:?}", analysis.decision)),
        "inconclusive_reason": reason,
    })
}
fn invocation_json(i: Invocation) -> Json {
    json!({"arm": match i.arm {Arm::Baseline=>"baseline",Arm::Candidate=>"candidate"},
        "epoch":i.epoch,"window":i.window,"operation":i.operation,"warmup":i.warmup,"layout_offset_bytes":i.layout_offset_bytes})
}
fn completion_json(c: &WindowCompletion) -> Json {
    let clock = match &c.clock {
        ClockStatus::Measured(n) => json!({"status":"measured","elapsed_ns":n.to_string()}),
        ClockStatus::Failed(e) => json!({"status":"failed","error":e.to_string()}),
    };
    json!({"invocation":invocation_json(c.invocation),"planned_operations":c.planned_operations,
        "attempted_operations":c.attempted_operations,"successful_operations":c.successful_operations,
        "body":format!("{:?}",c.body),"clock":clock})
}
fn unavailable_json(e: &Unavailable) -> Json {
    json!({"reason":format!("{:?}",e.reason),"operation":e.operation,"errno":e.os_error})
}
fn expected_windows(config: RunConfig) -> Result<usize, String> {
    if config.epochs == 0
        || config.epochs > 64
        || config.warmup_epochs > 64
        || config.operations_per_window == 0
        || config.operations_per_window > 65536
    {
        return Err("invalid observed benchmark configuration".into());
    }
    config
        .epochs
        .checked_add(config.warmup_epochs)
        .and_then(|n| n.checked_mul(4))
        .filter(|n| *n <= 512)
        .ok_or_else(|| "observed window count overflow/limit".into())
}
fn expected_invocation(config: RunConfig, index: usize) -> Invocation {
    let epoch = index / 4;
    let window = index % 4;
    Invocation {
        arm: [Arm::Baseline, Arm::Candidate, Arm::Candidate, Arm::Baseline][window],
        epoch,
        window,
        operation: 0,
        warmup: epoch < config.warmup_epochs,
        layout_offset_bytes: vesper_bench::layout_offset_bytes(config.layout_seed, epoch as u64),
    }
}
/// Arithmetic DTO only, not a constructor for hardware evidence. Production
/// values are copied exclusively from validated provider-produced observations.
#[derive(Clone, Copy)]
struct MetricWindow {
    invocation: Invocation,
    operations: u64,
    value: u64,
}
fn aggregate_pairs(
    config: RunConfig,
    windows: &[MetricWindow],
) -> Result<Vec<PairedSample>, String> {
    if windows.len() != expected_windows(config)? {
        return Err("observed window count mismatch".into());
    }
    let operations =
        u64::try_from(config.operations_per_window).map_err(|_| "operation count overflow")?;
    let mut result = Vec::with_capacity(config.epochs);
    let mut values = [0u64; 4];
    for (index, w) in windows.iter().enumerate() {
        if w.invocation != expected_invocation(config, index) || w.operations != operations {
            return Err("observed identity/operation count mismatch".into());
        }
        if !w.invocation.warmup && w.value == 0 {
            return Err("nonpositive observed metric".into());
        }
        values[index % 4] = w.value;
        if index % 4 == 3 && !w.invocation.warmup {
            let baseline = values[0]
                .checked_add(values[3])
                .ok_or("baseline counter total overflow")?;
            let candidate = values[1]
                .checked_add(values[2])
                .ok_or("candidate counter total overflow")?;
            result.push(
                PairedSample::new(
                    baseline,
                    candidate,
                    operations
                        .checked_mul(2)
                        .ok_or("paired operation overflow")?,
                )
                .map_err(|e| e.to_string())?,
            );
        }
    }
    Ok(result)
}
struct PmuData {
    windows: Json,
    cycles: Vec<PairedSample>,
    instructions: Vec<PairedSample>,
}
fn validate_pmu(
    config: RunConfig,
    report: &RunReport,
    windows: &[ObservedWindow<CounterDelta>],
    provider: Provider,
) -> Result<PmuData, String> {
    if windows.len() != expected_windows(config)?
        || report.epochs().len() != config.epochs
        || report.pairs().len() != config.epochs
    {
        return Err("PMU/clock report size mismatch".into());
    }
    let planned_calls = windows
        .len()
        .checked_mul(config.operations_per_window)
        .ok_or("body count overflow")?;
    if report.total_body_calls() != planned_calls {
        return Err("PMU total body count mismatch".into());
    }
    let mut clocks = Vec::with_capacity(windows.len());
    let mut cycles = Vec::with_capacity(windows.len());
    let mut instructions = Vec::with_capacity(windows.len());
    let mut json_windows = Vec::with_capacity(windows.len());
    for (index, w) in windows.iter().enumerate() {
        let c = &w.completion;
        let d = &w.observation;
        if w.invocation != expected_invocation(config, index)
            || c.invocation != w.invocation
            || c.planned_operations != config.operations_per_window
            || c.attempted_operations != config.operations_per_window
            || c.successful_operations != config.operations_per_window
            || c.body != BodyStatus::Completed
        {
            return Err("PMU actual invocation/completion mismatch".into());
        }
        let ns = match c.clock {
            ClockStatus::Measured(n) => n,
            ClockStatus::Failed(_) => return Err("PMU window has failed clock".into()),
        };
        let operations =
            u64::try_from(c.successful_operations).map_err(|_| "operation count overflow")?;
        if !w.invocation.warmup {
            let sample = &report.epochs()[w.invocation.epoch - config.warmup_epochs].windows
                [w.invocation.window];
            if sample.arm != w.invocation.arm
                || sample.operations != operations
                || sample.elapsed_nanos != ns
                || ns == 0
            {
                return Err("PMU/Instant actual window mismatch".into());
            }
        }
        if d.provider_kind() != provider
            || d.unit() != "hardware events"
            || d.cycles() == 0
            || d.instructions() == 0
            || d.enabled_nanos() == 0
            || d.enabled_nanos() != d.running_nanos()
        {
            return Err("invalid actual PMU evidence".into());
        }
        let data = |value| MetricWindow {
            invocation: w.invocation,
            operations,
            value,
        };
        clocks.push(data(ns));
        cycles.push(data(d.cycles()));
        instructions.push(data(d.instructions()));
        json_windows.push(
            json!({"invocation":invocation_json(w.invocation),"completion":completion_json(c),
            "cycles":d.cycles().to_string(),"instructions":d.instructions().to_string(),
            "enabled_ns":d.enabled_nanos().to_string(),"running_ns":d.running_nanos().to_string()}),
        );
    }
    if aggregate_pairs(config, &clocks)?.as_slice() != report.pairs() {
        return Err("PMU/clock paired totals mismatch".into());
    }
    Ok(PmuData {
        windows: Json::Array(json_windows),
        cycles: aggregate_pairs(config, &cycles)?,
        instructions: aggregate_pairs(config, &instructions)?,
    })
}
fn primary_error_json(e: &RunError<String, String>) -> Json {
    match e {
        RunError::Harness(error) => json!({"kind":"harness","error":error.to_string()}),
        RunError::Baseline(f) | RunError::Candidate(f) => {
            json!({"kind":"callback","arm":if matches!(e,RunError::Baseline(_)){"baseline"}else{"candidate"},
            "phase":format!("{:?}",f.phase),"invocation":invocation_json(f.invocation),"error":f.error,"cleanup_errors":f.cleanup_errors})
        }
    }
}
fn host_json() -> Json {
    let h = host_pmu_status();
    json!({"perf_event_paranoid":h.perf_event_paranoid,"cpu_pmu_device":h.cpu_pmu_device,"rdpmc_sysfs":h.rdpmc_sysfs})
}
fn unavailable_outcome(metric: Metric, provider: Provider, error: &Unavailable) -> Outcome {
    let message = error.to_string();
    let provider = provider.as_str();
    Outcome {
        data: json!({"schema":"vesper.bench/1","status":"inconclusive","metric":metric.name(),
        "provider_requested":provider,"unit":metric.unit(),"error":message,
        "pmu":{"requested":true,"status":"unavailable","provider":provider,"unavailable":unavailable_json(error),"host":host_json()},
        "evidence":{"hardware_pmu":"unavailable","zero_allocation":"unavailable","vectorization":"unavailable"}}),
        text: format!("{message}; requested PMU measurement is inconclusive (no fallback)\n"),
        gate_failure: Some(message),
    }
}
fn observed_error_outcome(
    metric: Metric,
    provider: Provider,
    error: ObservedRunError<String, String, Unavailable>,
) -> Outcome {
    let provider = provider.as_str();
    let message = error.to_string();
    let (status, primary, observer, completion, observer_cleanup_errors) = match &error {
        ObservedRunError::Run {
            error,
            observer_error,
            completion,
        } => (
            "error",
            Some(primary_error_json(error)),
            observer_error.as_ref(),
            completion.as_ref(),
            None,
        ),
        ObservedRunError::Observer {
            error,
            completion,
            cleanup_errors,
        } => (
            if *cleanup_errors == 0 {
                "inconclusive"
            } else {
                "error"
            },
            None,
            Some(error),
            completion.as_ref(),
            Some(*cleanup_errors),
        ),
    };
    let secondary=observer.map(|e|json!({"phase":format!("{:?}",e.phase),"invocation":invocation_json(e.invocation),"unavailable":unavailable_json(&e.error)}));
    Outcome {
        data: json!({"schema":"vesper.bench/1","status":status,"metric":metric.name(),"provider_requested":provider,"unit":metric.unit(),
        "error":message,"primary_error":primary,"observer_error":secondary,"observer_cleanup_errors":observer_cleanup_errors,"completion":completion.map(completion_json),
        "pmu":{"requested":true,"status":if observer.is_some(){"unavailable"}else{"incomplete"},"provider":provider},
        "evidence":{"hardware_pmu":"unavailable","zero_allocation":"unavailable","vectorization":"unavailable"}}),
        text: format!("{message}; requested PMU run failed without fallback\n"),
        gate_failure: Some(message),
    }
}

struct Outcome {
    data: Json,
    text: String,
    gate_failure: Option<String>,
}
type Observed = (RunReport, Vec<ObservedWindow<CounterDelta>>);
fn observe<O>(
    options: &Options,
    baseline: &mut SourceCase,
    candidate: &mut SourceCase,
    opened: Result<O, Unavailable>,
) -> Result<Observed, Box<Outcome>>
where
    O: WindowObserver<Observation = CounterDelta, Error = Unavailable>,
{
    let mut counters =
        opened.map_err(|e| Box::new(unavailable_outcome(options.metric, options.provider, &e)))?;
    run_abba_observed(options.config, baseline, candidate, &mut counters)
        .map(|run| run.into_parts())
        .map_err(|e| Box::new(observed_error_outcome(options.metric, options.provider, e)))
}
fn plan_json(plan: LayoutPlan) -> Json {
    json!({"seed":plan.seed.to_string(),"run":plan.run,"heap_offset_bytes":plan.heap_offset_bytes,
        "stack_padding_bytes":plan.stack_padding_bytes,"env_padding_bytes":plan.env_padding_bytes,
        "run_seed":plan.run_seed.to_string()})
}
fn execute(args: Vec<String>) -> Result<Option<Outcome>, String> {
    let Some(options) = options(args)? else {
        return Ok(None);
    };
    if let Some(layouts) = options.layouts {
        let exe = std::env::current_exe().map_err(|e| format!("current executable: {e}"))?;
        return randomize_layouts(&options, layouts, |plan| {
            let output = std::process::Command::new(&exe)
                .arg("bench")
                .args(&options.forwarded)
                .args(["--json", "--layout-run"])
                .arg(format!("{}:{}", plan.seed, plan.run))
                .env(ENV_PADDING_VAR, env_padding_value(plan.env_padding_bytes))
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|e| format!("layout run {}: {e}", plan.run))?;
            let stdout = String::from_utf8(output.stdout)
                .map_err(|_| format!("layout run {}: non-UTF-8 report", plan.run))?;
            let line = stdout
                .lines()
                .rev()
                .find(|l| l.starts_with('{'))
                .ok_or_else(|| format!("layout run {}: no JSON report", plan.run))?;
            serde_json::from_str(line).map_err(|e| format!("layout run {}: {e}", plan.run))
        })
        .map(Some);
    }
    if let Some((seed, run)) = options.layout_run {
        let plan = layout_plan(seed, run);
        let shim = HeapShim::new(plan.heap_offset_bytes).map_err(|e| e.to_string())?;
        let outcome = with_stack_padding(plan.stack_padding_bytes, || measure(options));
        drop(shim);
        let env = std::env::var_os(ENV_PADDING_VAR).map_or(0, |v| v.len());
        return outcome.map(|o| {
            o.map(|mut o| {
                o.data["layout"] = plan_json(plan);
                o.data["layout"]["observed_env_padding_bytes"] = json!(env);
                o
            })
        });
    }
    measure(options)
}
/// Sums the selected-metric pairs of one child report into one layout sample.
fn child_sample(data: &Json, metric: Metric) -> Result<PairedSample, String> {
    if data["status"] != "measured" {
        return Err(format!(
            "layout run did not measure: {}",
            data["error"].as_str().unwrap_or("unknown status")
        ));
    }
    let suffix = if metric == Metric::Time {
        "ns"
    } else {
        metric.name()
    };
    let pairs = data["pairs"]
        .as_array()
        .filter(|p| !p.is_empty())
        .ok_or("layout run report has no pairs")?;
    let field = |pair: &Json, key: String| -> Result<u64, String> {
        pair[&key]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("layout run pair lacks {key}"))
    };
    let (mut baseline, mut candidate, mut operations) = (0u64, 0u64, 0u64);
    for pair in pairs {
        baseline = baseline
            .checked_add(field(pair, format!("baseline_total_{suffix}"))?)
            .ok_or("layout baseline total overflow")?;
        candidate = candidate
            .checked_add(field(pair, format!("candidate_total_{suffix}"))?)
            .ok_or("layout candidate total overflow")?;
        operations = operations
            .checked_add(
                pair["operations_per_arm"]
                    .as_u64()
                    .ok_or("layout run pair lacks operations")?,
            )
            .ok_or("layout operation overflow")?;
    }
    PairedSample::new(baseline, candidate, operations).map_err(|e| e.to_string())
}
fn randomize_layouts(
    options: &Options,
    layouts: usize,
    mut run_child: impl FnMut(LayoutPlan) -> Result<Json, String>,
) -> Result<Outcome, String> {
    let mut samples = Vec::with_capacity(layouts);
    let mut runs = Vec::with_capacity(layouts);
    for run in 0..layouts {
        let plan = layout_plan(options.layout_seed, run as u32);
        let data = run_child(plan)?;
        if data["status"] == "measured"
            && (data["layout"]["run"] != json!(plan.run)
                || data["layout"]["observed_env_padding_bytes"]
                    != json!(env_padding_value(plan.env_padding_bytes).len()))
        {
            return Err(format!("layout run {run}: report does not match its plan"));
        }
        let sample = match child_sample(&data, options.metric) {
            Ok(sample) => sample,
            Err(message) => {
                // A child that could not measure (e.g. PMU unavailable) makes
                // the whole randomized comparison inconclusive; keep its report.
                return Ok(Outcome {
                    data: json!({"schema":"vesper.bench/1","status":if data["status"]=="inconclusive"{"inconclusive"}else{"error"},
                        "metric":options.metric.name(),"error":message,"layout_run":plan_json(plan),"child":data}),
                    text: format!("layout run {run}: {message}\n"),
                    gate_failure: Some(message),
                });
            }
        };
        samples.push(sample);
        runs.push(json!({"plan":plan_json(plan),"sample":pair_json(sample, options.metric),
            "source_blake3":{"candidate":data["candidate"]["source_blake3"],"baseline":data["baseline"]["source_blake3"]}}));
    }
    let survival = layout_survival(&samples).map_err(|e| e.to_string())?;
    let analysis =
        analyze_pairs(&samples, options.threshold.unwrap_or(0)).map_err(|e| e.to_string())?;
    let gate_failure = options.threshold.and_then(|_| match analysis.decision {
        Decision::Pass => None,
        Decision::Regression => Some("benchmark confidence gate detected a regression".into()),
        Decision::Inconclusive => Some("benchmark confidence gate is inconclusive".into()),
    });
    let median = survival.analysis.median_upper.regression_bp_bounds();
    let verdict = survival.verdict.as_str();
    let text = format!(
        "{} vs {}: {} randomized layouts (heap offset, stack padding, environment size), provider {} ({})\nmedian upper ratio: {}..{} basis points; layout verdict: {}{}\nlayouts move data and stack placement only, not machine code\n",
        options.path.display(),
        options.baseline.display(),
        layouts,
        options.metric.provider(options.provider),
        options.metric.unit(),
        median.floor,
        median.ceil,
        verdict,
        match survival.verdict {
            LayoutVerdict::DoesNotSurvive => " (the difference may be a layout artifact)",
            _ => "",
        },
    );
    Ok(Outcome {
        data: json!({"schema":"vesper.bench/1","status":"layout_randomized","metric":options.metric.name(),
            "provider":options.metric.provider(options.provider),"unit":options.metric.unit(),
            "candidate":{"path":options.path,"entry":options.entry},"baseline":{"path":options.baseline,"entry":options.baseline_entry},
            "layout":{"seed":options.layout_seed.to_string(),"layouts":layouts,"runs":runs,
                "verdict":verdict,"survives":survival.verdict.survives(),
                "method":"one aggregate paired sample per randomized child process; exact >=95% median-ratio interval tested against ratio 1",
                "scope":"randomizes heap offset (live block + allocator state), stack padding (real frames) and environment size per process; machine code, function entries and globals are not relocated"},
            "analysis":analysis_json(&analysis, options.threshold, options.metric)}),
        text,
        gate_failure,
    })
}
fn measure(options: Options) -> Result<Option<Outcome>, String> {
    // Both compilation phases precede run_abba and every timed window.
    let mut baseline = SourceCase::compile(&options.baseline, &options.baseline_entry)
        .map_err(|error| format!("baseline {}: {error}", options.baseline.display()))?;
    let mut candidate = SourceCase::compile(&options.path, &options.entry)
        .map_err(|error| format!("candidate {}: {error}", options.path.display()))?;
    if options.path == options.baseline && baseline.source_id != candidate.source_id {
        return Err("source changed while preparing the same-file comparison".into());
    }
    let (report, pmu) = if options.pmu {
        let observed = match options.provider {
            Provider::LinuxGroupRead => {
                observe(&options, &mut baseline, &mut candidate, PerfGroup::open())
            }
            Provider::LinuxRdpmc => observe(
                &options,
                &mut baseline,
                &mut candidate,
                RdpmcCounters::open(),
            ),
            Provider::MacosKpc => {
                observe(&options, &mut baseline, &mut candidate, KpcCounters::open())
            }
        };
        let (report, windows) = match observed {
            Ok(parts) => parts,
            Err(outcome) => return Ok(Some(*outcome)),
        };
        let pmu = validate_pmu(options.config, &report, &windows, options.provider)?;
        (report, Some(pmu))
    } else {
        (
            run_abba(options.config, &mut baseline, &mut candidate).map_err(|e| e.to_string())?,
            None,
        )
    };
    let selected = match options.metric {
        Metric::Time => report.pairs(),
        Metric::Cycles => {
            &pmu.as_ref()
                .ok_or("counter metric requires actual PMU evidence")?
                .cycles
        }
        Metric::Instructions => {
            &pmu.as_ref()
                .ok_or("counter metric requires actual PMU evidence")?
                .instructions
        }
    };
    let analysis =
        analyze_pairs(selected, options.threshold.unwrap_or(0)).map_err(|e| e.to_string())?;
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
        options.metric.provider(options.provider),
        options.metric.unit(),
        median.floor,
        median.ceil,
        status,
    );
    let mut data = json!({
        "schema": "vesper.bench/1", "status": status, "metric":options.metric.name(),
        "provider": options.metric.provider(options.provider), "unit": options.metric.unit(),
        "candidate": {"path": options.path, "entry": options.entry, "source_blake3": candidate.source_id, "jit": stats_json(candidate.runner.stats())},
        "baseline": {"path": options.baseline, "entry": options.baseline_entry, "source_blake3": baseline.source_id, "jit": stats_json(baseline.runner.stats())},
        "config": {"epochs": options.config.epochs, "warmup_epochs": options.config.warmup_epochs, "operations_per_window": options.config.operations_per_window},
        "body_calls": report.total_body_calls(), "elapsed_run_ns": report.elapsed_run_nanos().to_string(),
        "pairs": selected.iter().copied().map(|p|pair_json(p,options.metric)).collect::<Vec<_>>(),
        "analysis": analysis_json(&analysis, options.threshold,options.metric),
        "evidence": {"hardware_pmu": "unavailable", "zero_allocation": "unavailable", "vectorization": "unavailable"},
        "scope": "unoptimized finite scalar JIT kernels; measured windows include Rust dispatch/loop/output storage; no hard deadline or sandbox guarantee",
    });
    let text = if let Some(pmu) = pmu {
        data["clock"] = json!({"provider":report.provider().as_str(),"unit":report.unit().as_str(),"elapsed_run_ns":report.elapsed_run_nanos().to_string(),
            "pairs":report.pairs().iter().copied().map(|p|pair_json(p,Metric::Time)).collect::<Vec<_>>()});
        data["pmu"] = json!({"requested":true,"status":"measured","provider":options.provider.as_str(),
        "cycles_unit":"cycles","instructions_unit":"instructions","windows":pmu.windows,
        "cycles_pairs":pmu.cycles.iter().copied().map(|p|pair_json(p,Metric::Cycles)).collect::<Vec<_>>(),
        "instructions_pairs":pmu.instructions.iter().copied().map(|p|pair_json(p,Metric::Instructions)).collect::<Vec<_>>(),
        "scope":match options.provider {
            Provider::LinuxGroupRead => "current-thread user hardware counts include enable-return, dispatch, Instant and observer boundary overhead through disable; syscall reads; no body-only calibration",
            Provider::LinuxRdpmc => "current-thread user hardware counts read with RDPMC under the perf mmap sequence lock; include dispatch, Instant and observer boundary overhead between reads; no body-only calibration",
            Provider::MacosKpc => "current-thread fixed kpc counters (cycles, instructions); include dispatch, Instant and observer boundary overhead between reads; no body-only calibration",
        }});
        data["evidence"]["hardware_pmu"] = json!("available");
        if options.metric != Metric::Time {
            data.as_object_mut()
                .expect("report object")
                .remove("elapsed_run_ns");
        }
        format!(
            "{text}PMU: {}; hardware counts include clock/dispatch/observer boundary overhead; availability supplies no allocation/SIMD evidence\n",
            options.provider.as_str()
        )
    } else {
        text
    };
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

#[cfg(test)]
mod pmu_tests {
    use super::*;
    use vesper_bench::{CallbackFailure, CallbackPhase, ObservationFailure, ObservationPhase};
    #[test]
    fn ordinary_metric_dtos_aggregate_checked_measured_pairs_only() {
        let config = RunConfig {
            epochs: 2,
            warmup_epochs: 1,
            operations_per_window: 3,
            ..RunConfig::default()
        };
        let mut windows: Vec<_> = (0..12)
            .map(|index| MetricWindow {
                invocation: expected_invocation(config, index),
                operations: 3,
                value: if index < 4 {
                    u64::MAX
                } else {
                    100 + index as u64
                },
            })
            .collect();
        let pairs = aggregate_pairs(config, &windows).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].baseline_total(), 211);
        assert_eq!(pairs[0].candidate_total(), 211);
        assert_eq!(pairs[0].operations_per_arm(), 6);
        let original = windows[4];
        windows[4].operations = 4;
        assert!(aggregate_pairs(config, &windows).is_err());
        windows[4] = original;
        windows[4].invocation.arm = Arm::Candidate;
        assert!(aggregate_pairs(config, &windows).is_err());
        windows[4] = original;
        windows[4].value = 0;
        assert!(aggregate_pairs(config, &windows).is_err());
        windows[4] = original;
        windows[4].value = u64::MAX;
        assert!(aggregate_pairs(config, &windows).is_err());
        windows[4] = original;
        assert!(aggregate_pairs(config, &windows[..11]).is_err());
    }
    #[test]
    fn selected_metric_labels_all_pair_and_analysis_bounds() {
        let pairs = vec![PairedSample::new(100, 101, 2).unwrap(); 6];
        let analysis = analyze_pairs(&pairs, 1_000_000).unwrap();
        for metric in [Metric::Time, Metric::Cycles, Metric::Instructions] {
            let data = analysis_json(&analysis, Some(1_000_000), metric);
            let suffix = if metric == Metric::Time {
                "ns"
            } else {
                metric.name()
            };
            assert!(data["median_upper"][format!("baseline_total_{suffix}")].is_string());
            if metric != Metric::Time {
                assert!(data["median_upper"].get("baseline_total_ns").is_none());
            }
        }
    }
    #[test]
    fn primary_callback_error_is_retained_with_secondary_hardware_unavailability() {
        let invocation = expected_invocation(RunConfig::default(), 0);
        let completion = WindowCompletion {
            invocation,
            planned_operations: 3,
            attempted_operations: 2,
            successful_operations: 1,
            body: BodyStatus::Failed,
            clock: ClockStatus::Measured(1),
        };
        let error = ObservedRunError::Run {
            error: RunError::Baseline(CallbackFailure {
                phase: CallbackPhase::Run,
                invocation,
                error: "primary body error".to_string(),
                cleanup_errors: 2,
            }),
            observer_error: Some(ObservationFailure {
                phase: ObservationPhase::After,
                invocation,
                error: Unavailable {
                    reason: vesper_bench_pmu::UnavailableReason::OsError,
                    operation: "group read",
                    os_error: Some(5),
                },
            }),
            completion: Some(completion),
        };
        let outcome = observed_error_outcome(Metric::Instructions, Provider::LinuxGroupRead, error);
        assert_eq!(outcome.data["status"], "error");
        assert_eq!(outcome.data["primary_error"]["error"], "primary body error");
        assert_eq!(outcome.data["primary_error"]["cleanup_errors"], 2);
        assert_eq!(
            outcome.data["observer_error"]["unavailable"]["operation"],
            "group read"
        );
        assert!(outcome.data.get("pairs").is_none());
        assert!(outcome.gate_failure.is_some());
        let cleanup_failure = observed_error_outcome(
            Metric::Cycles,
            Provider::LinuxGroupRead,
            ObservedRunError::Observer {
                error: ObservationFailure {
                    phase: ObservationPhase::Before,
                    invocation,
                    error: Unavailable {
                        reason: vesper_bench_pmu::UnavailableReason::OsError,
                        operation: "group read",
                        os_error: Some(5),
                    },
                },
                cleanup_errors: 1,
                completion: None,
            },
        );
        assert_eq!(cleanup_failure.data["status"], "error");
        assert_eq!(cleanup_failure.data["observer_cleanup_errors"], 1);
    }
}

#[cfg(test)]
mod metronome_tests {
    use super::*;
    fn parse(args: &[&str]) -> Options {
        options(args.iter().map(|s| s.to_string()).collect())
            .unwrap()
            .unwrap()
    }
    #[test]
    fn readme_bench_row_names_only_flags_this_command_has() {
        let readme = include_str!("../README.md");
        let row = readme
            .lines()
            .find(|l| l.starts_with("| `vesper bench` |"))
            .expect("README has a vesper bench row");
        let flags: Vec<&str> = row
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .filter(|w| w.starts_with("--") && w.len() > 2)
            .collect();
        assert!(flags.len() >= 3, "{flags:?}");
        for flag in flags {
            assert!(HELP.contains(flag), "README names {flag}, help lacks it");
        }
        for mechanism in [
            "rdpmc",
            "kpc",
            "group",
            "randomize-layout",
            "vesper_bench::causal",
        ] {
            assert!(HELP.contains(mechanism), "help lacks {mechanism}");
        }
        // Claims the command does not make.
        for absent in ["12-cycle", "CAS", "--causal", "--asm", "cargo-show-asm"] {
            assert!(!row.contains(absent), "README row still claims {absent}");
        }
    }
    #[test]
    fn layout_and_provider_flags_parse_and_forward_only_measurement_arguments() {
        let o = parse(&[
            "c.ves",
            "--baseline",
            "b.ves",
            "--samples",
            "6",
            "--pmu-provider",
            "rdpmc",
            "--randomize-layout",
            "8",
            "--layout-seed",
            "9",
            "--max-regression-bp",
            "50",
            "--json",
        ]);
        assert_eq!(o.provider, Provider::LinuxRdpmc);
        assert!(o.pmu);
        assert_eq!((o.layouts, o.layout_seed), (Some(8), 9));
        assert_eq!(
            o.forwarded,
            [
                "c.ves",
                "--baseline",
                "b.ves",
                "--samples",
                "6",
                "--pmu-provider",
                "rdpmc"
            ]
        );
        let child = parse(&["c.ves", "--randomize-layout", "8", "--layout-run", "9:3"]);
        assert_eq!((child.layouts, child.layout_run), (None, Some((9, 3))));
        assert_eq!(parse(&["c.ves"]).provider, default_provider());
        for bad in [
            vec!["c.ves", "--randomize-layout", "5"],
            vec!["c.ves", "--randomize-layout", "33"],
            vec!["c.ves", "--pmu-provider", "nope"],
            vec!["c.ves", "--layout-run", "1"],
            vec!["c.ves", "--layout-run", "1:64"],
            vec!["c.ves", "--layout-seed", "-1"],
        ] {
            assert!(options(bad.iter().map(|s| s.to_string()).collect()).is_err());
        }
    }
    fn child(plan: LayoutPlan, baseline: u64, candidate: u64) -> Json {
        json!({"schema":"vesper.bench/1","status":"measured",
            "layout":{"run":plan.run,"observed_env_padding_bytes":env_padding_value(plan.env_padding_bytes).len()},
            "pairs":[pair_json(PairedSample::new(baseline, candidate, 4).unwrap(), Metric::Time),
                     pair_json(PairedSample::new(baseline, candidate, 4).unwrap(), Metric::Time)]})
    }
    #[test]
    fn layout_driver_aggregates_children_and_tests_survival() {
        let o = parse(&[
            "c.ves",
            "--randomize-layout",
            "6",
            "--max-regression-bp",
            "0",
        ]);
        let mut seen = Vec::new();
        let outcome = randomize_layouts(&o, 6, |plan| {
            seen.push(plan.run);
            assert_eq!(plan, layout_plan(0, plan.run));
            Ok(child(plan, 100, 150))
        })
        .unwrap();
        assert_eq!(seen, [0, 1, 2, 3, 4, 5]);
        assert_eq!(outcome.data["layout"]["verdict"], "candidate_slower");
        assert_eq!(outcome.data["layout"]["survives"], true);
        assert_eq!(
            outcome.data["layout"]["runs"][0]["sample"]["baseline_total_ns"],
            "200"
        );
        assert!(
            outcome.gate_failure.is_some(),
            "a 50% regression fails a 0bp gate"
        );
        // Alternating wins and losses: the difference does not survive.
        let mixed = randomize_layouts(&o, 6, |plan| {
            Ok(if plan.run % 2 == 0 {
                child(plan, 100, 70)
            } else {
                child(plan, 100, 130)
            })
        })
        .unwrap();
        assert_eq!(mixed.data["layout"]["verdict"], "does_not_survive");
        // A child that reports a different plan is rejected.
        assert!(
            randomize_layouts(&o, 6, |plan| Ok(child(layout_plan(1, plan.run + 1), 1, 1))).is_err()
        );
        // A child that could not measure makes the comparison inconclusive.
        let unavailable = randomize_layouts(&o, 6, |_| {
            Ok(json!({"status":"inconclusive","error":"PMU unavailable"}))
        })
        .unwrap();
        assert_eq!(unavailable.data["status"], "inconclusive");
        assert!(unavailable.gate_failure.is_some());
        assert!(child_sample(&json!({"status":"measured","pairs":[]}), Metric::Time).is_err());
    }
}
