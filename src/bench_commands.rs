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
use vesper_bench::wire::{PmuField, PmuSample, backend_bound_bp};
use vesper_bench_pmu::{
    AllocationCounts, AllocationDelta, AllocationObserver, CounterDelta, CountingAllocator,
    EventDelta, HardwareEvent, KpcCounters, PerfEventSet, PerfGroup, Provider, RdpmcCounters,
    Unavailable, UnavailableReason, host_pmu_status,
};
use vesper_loom_cas::{DerivationInputs, Domain, ObjectId, Store};

/// Zero-allocation evidence (`--expect-zero-alloc`) reads this allocator's
/// per-thread counters. It forwards every call to `System`, the default.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
const MAX_SOURCE_BYTES: u64 = 256 * 1024;
const MAX_CALLS: usize = 1_000_000;
const HELP: &str = r#"Usage: vesper bench FILE [options]

Compile checked finite scalar source kernels before paired A/B/B/A measurement.
The default baseline is a separately compiled copy of FILE.

  --baseline FILE          Local baseline source
  --compare git:REV        Baseline is FILE as committed at REV, resolved with
                           git and cached as a baseline archive in the Loom CAS
                           (--store DIR, default $VESPER_STORE or
                           ~/.vesper/store); a cached archive is reused
  --store DIR              Loom CAS directory for --compare
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
  --deterministic          Simulated-clock mode: count Vesper evaluator steps
                           per call (exact, zero variance) instead of timing
                           native code; gates compare the exact ratio
  --asm                    Print the measured functions' Cranelift CLIF and
                           machine-code listing (same IR and codegen flags as
                           the benchmark JIT) with vectorization evidence
  --expect-vectorized      Fail unless the candidate's compiled functions
                           contain vector (SIMD) instructions
  --expect-zero-alloc      Count heap allocations of the measuring thread in
                           every measured window; fail unless all are zero
  --pmu-events LIST        Extra counters, comma-separated: ref-cycles,
                           l1d-read-misses, llc-read-misses, branch-mispredicts,
                           simd-ops, tma-slots, tma-backend-bound (Linux perf;
                           each reported as counted or unavailable), plus
                           per-window 64-byte PmuSampleWire records
  --json                   Machine-readable report, including failures
  -h, --help               Show help

Inputs: bounded regular UTF-8 files containing ordinary nongeneric pure scalar
functions. Recursive/indirect/aggregate/imported/fallible kernels are unsupported.
Measurements: std::time::Instant nanoseconds, including Rust/JIT dispatch, loop
and result-storage overhead. Optional PMU counts include clock/observer dispatch boundary overhead.
Unavailable PMU is inconclusive with no clock fallback and reports why (for
example no hardware PMU device, perf_event_paranoid, or kpc without root).
PMU availability supplies no evidence about allocation or SIMD.
Deterministic steps are a cost model of the reference evaluator, not machine
cycles. Vectorization evidence inspects CLIF value types (exact for Cranelift)
and lists vector-register machine instructions.
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
    compare: Option<String>,
    store: Option<PathBuf>,
    deterministic: bool,
    asm: bool,
    expect_vectorized: bool,
    expect_zero_alloc: bool,
    events: Vec<HardwareEvent>,
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
    let mut compare = None;
    let mut store = None;
    let (mut deterministic, mut asm, mut expect_vectorized, mut expect_zero_alloc) =
        (false, false, false, false);
    let mut events = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--json" => {}
            "--compare" => {
                let value = argument(&mut args, "--compare")?;
                let rev = value
                    .strip_prefix("git:")
                    .ok_or("--compare requires git:REV")?;
                validate_rev(rev)?;
                if compare.replace(rev.to_owned()).is_some() {
                    return Err("--compare may only be supplied once".into());
                }
                forwarded.extend([arg, value]);
            }
            "--store" => {
                let value = argument(&mut args, "--store")?;
                store = Some(PathBuf::from(&value));
                forwarded.extend([arg, value]);
            }
            "--deterministic" => deterministic = true,
            "--asm" => asm = true,
            "--expect-vectorized" => expect_vectorized = true,
            "--expect-zero-alloc" => expect_zero_alloc = true,
            "--pmu-events" => {
                let value = argument(&mut args, "--pmu-events")?;
                for name in value.split(',') {
                    let event = HardwareEvent::parse(name.trim())
                        .ok_or_else(|| format!("unknown --pmu-events entry '{name}'"))?;
                    if !events.contains(&event) {
                        events.push(event);
                    }
                }
            }
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
    if compare.is_some() && baseline.is_some() {
        return Err("--compare and --baseline are mutually exclusive".into());
    }
    if store.is_some() && compare.is_none() {
        return Err("--store requires --compare".into());
    }
    if deterministic
        && (pmu
            || provider.is_some()
            || metric != Metric::Time
            || !events.is_empty()
            || expect_zero_alloc
            || layouts.is_some())
    {
        return Err(
            "--deterministic counts evaluator steps; it excludes --pmu, --pmu-provider, --metric, --pmu-events, --expect-zero-alloc and --randomize-layout"
                .into(),
        );
    }
    if layouts.is_some() && (asm || expect_vectorized || expect_zero_alloc || !events.is_empty()) {
        return Err(
            "--asm, --expect-vectorized, --expect-zero-alloc and --pmu-events are per-run evidence; not supported with --randomize-layout"
                .into(),
        );
    }
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
        compare,
        store,
        deterministic,
        asm,
        expect_vectorized,
        expect_zero_alloc,
        events,
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
    world: vesper::check::World,
}
impl SourceCase {
    fn compile(path: &Path, entry: &str) -> Result<Self, String> {
        Self::compile_text(&read_source(path)?, entry)
    }
    fn compile_text(source: &str, entry: &str) -> Result<Self, String> {
        if source.len() as u64 > MAX_SOURCE_BYTES {
            return Err("bench source exceeds 256 KiB".into());
        }
        source_guard(source)?;
        let parsed = vesper::parser::parse(source);
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
            world: checked.world,
        })
    }
}

// ---------------------------------------------------------------------------
// `--compare git:REV`: baselines resolved with git, cached in the Loom CAS.
// ---------------------------------------------------------------------------

const BASELINE_SCHEMA: &str = "vesper.bench.baseline/1";

fn validate_rev(rev: &str) -> Result<(), String> {
    if rev.is_empty()
        || rev.len() > 256
        || rev.starts_with('-')
        || rev.contains(':')
        || rev.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(
            "--compare git:REV requires a revision without whitespace, ':' or a leading '-'".into(),
        );
    }
    Ok(())
}

/// The git operations `--compare` needs, so tests can script them.
trait Git {
    fn run(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String>;
}
struct SystemGit;
impl Git for SystemGit {
    fn run(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "git unavailable: no git executable on PATH".to_string()
                } else {
                    format!("git unavailable: {e}")
                }
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut message: String = stderr.trim().chars().take(512).collect();
            if message.is_empty() {
                message = output.status.to_string();
            }
            return Err(format!("git {}: {message}", args[0]));
        }
        Ok(output.stdout)
    }
}

/// A baseline's identity: a full commit ID and the repository-relative path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GitBaseline {
    commit: String,
    path: String,
    repo_dir: PathBuf,
}
/// `canonicalize` on Windows returns `\\?\C:\...`, which git does not
/// accept for `-C`; drop the verbatim prefix for drive paths.
fn plain_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\")
            && rest.as_bytes().get(1) == Some(&b':')
        {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}
fn git_text(git: &dyn Git, dir: &Path, args: &[&str]) -> Result<String, String> {
    let bytes = git.run(dir, args)?;
    if bytes.len() > 4096 {
        return Err(format!("git {}: oversized output", args[0]));
    }
    String::from_utf8(bytes)
        .map(|s| s.trim().to_owned())
        .map_err(|_| format!("git {}: non-UTF-8 output", args[0]))
}
fn resolve_git(git: &dyn Git, candidate: &Path, rev: &str) -> Result<GitBaseline, String> {
    validate_rev(rev)?;
    let file = fs::canonicalize(candidate).map_err(|e| format!("{}: {e}", candidate.display()))?;
    let dir = plain_path(file.parent().ok_or("candidate has no parent directory")?);
    let spec = format!("{rev}^{{commit}}");
    let commit = git_text(
        git,
        &dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ],
    )
    .map_err(|e| format!("cannot resolve git:{rev}: {e}"))?;
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("git:{rev} did not resolve to a full commit ID"));
    }
    let top = git_text(git, &dir, &["rev-parse", "--show-toplevel"])?;
    let top = fs::canonicalize(&top).map_err(|e| format!("git toplevel {top}: {e}"))?;
    let relative = file
        .strip_prefix(&top)
        .map_err(|_| "candidate is outside its git work tree")?;
    let mut parts = Vec::new();
    for part in relative.components() {
        match part {
            std::path::Component::Normal(p) => {
                parts.push(p.to_str().ok_or("candidate path is not UTF-8")?.to_owned())
            }
            _ => return Err("candidate path is not a plain work-tree path".into()),
        }
    }
    if parts.is_empty() {
        return Err("candidate path is empty".into());
    }
    Ok(GitBaseline {
        commit: commit.to_ascii_lowercase(),
        path: parts.join("/"),
        repo_dir: dir,
    })
}
fn git_source(git: &dyn Git, baseline: &GitBaseline) -> Result<String, String> {
    let spec = format!("{}:{}", baseline.commit, baseline.path);
    let size: u64 = git_text(git, &baseline.repo_dir, &["cat-file", "-s", &spec])
        .map_err(|e| format!("{} at {}: {e}", baseline.path, baseline.commit))?
        .parse()
        .map_err(|_| "git cat-file -s: not a size")?;
    if size > MAX_SOURCE_BYTES {
        return Err("baseline source exceeds 256 KiB".into());
    }
    let bytes = git.run(&baseline.repo_dir, &["cat-file", "blob", &spec])?;
    if bytes.len() as u64 != size {
        return Err("git cat-file returned a different size than it reported".into());
    }
    String::from_utf8(bytes).map_err(|_| "baseline source must be UTF-8".into())
}
fn default_store() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("VESPER_STORE").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .ok_or("--compare needs --store DIR, $VESPER_STORE or a home directory")?;
    Ok(PathBuf::from(home).join(".vesper").join("store"))
}
fn locator_bytes(b: &GitBaseline) -> Vec<u8> {
    format!("{BASELINE_SCHEMA}\ncommit {}\npath {}\n", b.commit, b.path).into_bytes()
}
fn baseline_inputs(locator: ObjectId, b: &GitBaseline) -> DerivationInputs {
    DerivationInputs::new(Domain::Artifact, "vesper-bench-baseline", BASELINE_SCHEMA)
        .with_option("git.commit", b.commit.clone())
        .with_option("git.path", b.path.clone())
        .with_input("locator", locator)
}
fn encode_archive(b: &GitBaseline, source: &str) -> Vec<u8> {
    serde_json::to_vec(
        &json!({"schema":BASELINE_SCHEMA,"commit":b.commit,"path":b.path,
        "source_blake3":blake3::hash(source.as_bytes()).to_hex().to_string(),"source":source}),
    )
    .expect("JSON object serializes")
}
fn decode_archive(bytes: &[u8], b: &GitBaseline) -> Result<String, String> {
    let corrupt = |why: &str| format!("CAS baseline archive rejected: {why}");
    if bytes.len() as u64 > 8 * MAX_SOURCE_BYTES {
        return Err(corrupt("oversized"));
    }
    let data: Json = serde_json::from_slice(bytes).map_err(|_| corrupt("not JSON"))?;
    if data["schema"] != BASELINE_SCHEMA {
        return Err(corrupt("unknown schema"));
    }
    if data["commit"] != b.commit.as_str() || data["path"] != b.path.as_str() {
        return Err(corrupt("commit/path differ from the derivation"));
    }
    let source = data["source"]
        .as_str()
        .ok_or_else(|| corrupt("no source"))?;
    if source.len() as u64 > MAX_SOURCE_BYTES {
        return Err(corrupt("source exceeds 256 KiB"));
    }
    if data["source_blake3"] != blake3::hash(source.as_bytes()).to_hex().as_str() {
        return Err(corrupt("source hash mismatch"));
    }
    Ok(source.to_owned())
}
struct CasBaseline {
    source: String,
    git: GitBaseline,
    archive: ObjectId,
    cache: &'static str,
    store: PathBuf,
}
fn load_cas_baseline(
    store_dir: &Path,
    git: &dyn Git,
    candidate: &Path,
    rev: &str,
) -> Result<CasBaseline, String> {
    let b = resolve_git(git, candidate, rev)?;
    let store = Store::open(store_dir).map_err(|e| format!("CAS {}: {e}", store_dir.display()))?;
    let cas = |e: vesper_loom_cas::CasError| format!("CAS: {e}");
    let locator = store
        .put_blob(Domain::Source, &locator_bytes(&b))
        .map_err(cas)?
        .id;
    let inputs = baseline_inputs(locator, &b);
    if let Some(id) = store.lookup_derivation(&inputs).map_err(cas)? {
        let source = decode_archive(&store.read_blob(&id).map_err(cas)?, &b)?;
        return Ok(CasBaseline {
            source,
            git: b,
            archive: id,
            cache: "hit",
            store: store.root().to_path_buf(),
        });
    }
    let source = git_source(git, &b)?;
    let id = store
        .put_blob(Domain::Artifact, &encode_archive(&b, &source))
        .map_err(cas)?
        .id;
    store.publish_derivation(&inputs, id).map_err(cas)?;
    Ok(CasBaseline {
        source,
        git: b,
        archive: id,
        cache: "populated",
        store: store.root().to_path_buf(),
    })
}
fn cas_json(c: &CasBaseline, rev: &str) -> Json {
    json!({"rev":rev,"commit":c.git.commit,"path":c.git.path,"archive":c.archive.to_string(),
        "cache":c.cache,"store":c.store,
        "scope":"the baseline source archive is content-addressed in the Loom CAS and compiled in this process; native code is not cached"})
}

// ---------------------------------------------------------------------------
// `--deterministic`: exact evaluator step counts (simulated clock).
// ---------------------------------------------------------------------------

const STEP_FUEL: u64 = 50_000_000;

/// Evaluator steps for one call of `entry`, with its result.
fn evaluator_steps(world: &vesper::check::World, entry: &str) -> Result<(u64, String), String> {
    let mut runtime = vesper::runtime::Runtime::new(world);
    // The benchmark JIT compiles unoptimized IR; count the same program.
    runtime.optimize = false;
    runtime.fuel = STEP_FUEL;
    let value = runtime.run(entry, Vec::new())?;
    let steps = STEP_FUEL - runtime.fuel;
    if steps == 0 {
        return Err("evaluator reported zero steps".into());
    }
    Ok((steps, format!("{value:?}")))
}
fn deterministic_arm(case: &mut SourceCase, repetitions: usize) -> Result<(u64, String), String> {
    let (steps, value) = evaluator_steps(&case.world, &case.entry)?;
    for _ in 1..repetitions {
        if evaluator_steps(&case.world, &case.entry)? != (steps, value.clone()) {
            return Err("evaluator step count is not deterministic".into());
        }
    }
    let native = format!("{:?}", case.runner.run(&case.entry)?);
    if native != value {
        return Err(format!(
            "native result {native} differs from the evaluator's {value}"
        ));
    }
    Ok((steps, value))
}
fn deterministic_outcome(
    options: &Options,
    baseline: &mut SourceCase,
    candidate: &mut SourceCase,
) -> Result<Outcome, String> {
    let repetitions = options.config.epochs.clamp(2, 64);
    let (base, base_value) = deterministic_arm(baseline, repetitions)?;
    let (cand, cand_value) = deterministic_arm(candidate, repetitions)?;
    let pair = PairedSample::new(base, cand, 1).map_err(|e| e.to_string())?;
    let bounds = pair.regression_bp_bounds();
    let (status, gate_failure) = match options.threshold {
        None => ("measured", None),
        Some(t) if bounds.ceil <= i128::from(t) => ("passed", None),
        Some(_) => (
            "regression",
            Some("deterministic step gate detected a regression".to_string()),
        ),
    };
    let text = format!(
        "{} vs {}: deterministic evaluator steps per call: candidate {cand}, baseline {base} ({} identical repetitions)\nratio: {}..{} basis points; {status}\nsimulated clock (reference evaluator steps), not machine cycles\n",
        options.path.display(),
        baseline_label(options),
        repetitions,
        bounds.floor,
        bounds.ceil,
    );
    Ok(Outcome {
        data: json!({"schema":"vesper.bench/1","status":status,"mode":"deterministic","metric":"steps",
            "provider":"vesper.runtime.evaluator_steps","unit":"evaluator steps per call",
            "candidate":{"path":options.path,"entry":options.entry,"source_blake3":candidate.source_id,"steps_per_call":cand.to_string(),"result":cand_value},
            "baseline":{"path":baseline_path_json(options),"entry":options.baseline_entry,"source_blake3":baseline.source_id,"steps_per_call":base.to_string(),"result":base_value},
            "repetitions":repetitions,"variance":"0: every repetition gave the same count",
            "regression_basis_points":{"floor":bounds.floor.to_string(),"ceil":bounds.ceil.to_string()},
            "threshold_bp":options.threshold,
            "gate_decision":options.threshold.map(|_| if gate_failure.is_some() {"Regression"} else {"Pass"}),
            "scope":"exact step count of Vesper's reference evaluator on the unoptimized IR (one unit per evaluator step), cross-checked against the native result; a deterministic cost model, not cycles or retired instructions",
            "evidence":{"hardware_pmu":"unavailable","zero_allocation":"unavailable","vectorization":"unavailable"}}),
        text,
        gate_failure,
    })
}

// ---------------------------------------------------------------------------
// `--asm` and `--expect-vectorized`: CLIF and machine listings.
// ---------------------------------------------------------------------------

/// The benchmark JIT's ISA flags (`NativeBatch`): non-PIC, no colocated
/// libcalls, NaN canonicalization for DetFloat programs, host target.
fn batch_isa(
    world: &vesper::check::World,
) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>, String> {
    use cranelift_codegen::settings::{self, Configurable};
    let mut flags = settings::builder();
    flags
        .set("use_colocated_libcalls", "false")
        .map_err(|e| e.to_string())?;
    flags.set("is_pic", "false").map_err(|e| e.to_string())?;
    if vesper::native::needs_nan_canonicalization(world) {
        flags
            .set("enable_nan_canonicalization", "true")
            .map_err(|e| e.to_string())?;
    }
    cranelift_native::builder()
        .map_err(str::to_string)?
        .finish(settings::Flags::new(flags))
        .map_err(|e| e.to_string())
}
/// Machine-listing lines that name a vector register. On x86-64 scalar
/// floats also live in xmm registers, so this is a listing aid, not the
/// vectorization decision (which uses CLIF value types).
fn vector_register_line(line: &str) -> bool {
    const AARCH64: [&str; 8] = [".16b", ".8b", ".8h", ".4h", ".4s", ".2s", ".2d", ".1d"];
    line.contains("%xmm")
        || line.contains("%ymm")
        || line.contains("%zmm")
        || AARCH64.iter().any(|a| line.contains(a))
        || line.contains(" z0") // SVE
}
struct Listing {
    data: Json,
    text: String,
    vector_instructions: usize,
}
fn asm_listing(case: &SourceCase) -> Result<Listing, String> {
    let mut functions = Vec::new();
    vesper::native::object_with_observer(
        &case.world,
        &case.entry,
        None,
        false,
        Some(&mut |f| functions.push(f)),
    )?;
    if functions.is_empty() {
        return Err("no compiled functions to list".into());
    }
    let isa = batch_isa(&case.world)?;
    let mut text = String::new();
    let mut entries = Vec::new();
    let mut total_vector = 0usize;
    for f in functions {
        let func = &f.function;
        let mut vector = 0usize;
        let mut instructions = 0usize;
        for block in func.layout.blocks() {
            for inst in func.layout.block_insts(block) {
                instructions += 1;
                let dfg = &func.dfg;
                if dfg.ctrl_typevar(inst).is_vector()
                    || dfg
                        .inst_results(inst)
                        .iter()
                        .any(|v| dfg.value_type(*v).is_vector())
                    || dfg
                        .inst_args(inst)
                        .iter()
                        .any(|v| dfg.value_type(*v).is_vector())
                {
                    vector += 1;
                }
            }
        }
        total_vector += vector;
        let clif = func.display().to_string();
        let mut ctx = cranelift_codegen::Context::for_function(func.clone());
        ctx.set_disasm(true);
        let compiled = ctx
            .compile(
                &*isa,
                &mut cranelift_codegen::control::ControlPlane::default(),
            )
            .map_err(|e| format!("{}: {:?}", f.symbol, e.inner))?;
        let machine = compiled
            .vcode
            .clone()
            .ok_or("Cranelift produced no machine listing")?;
        let bytes = compiled.code_buffer();
        let vector_lines = machine.lines().filter(|l| vector_register_line(l)).count();
        text.push_str(&format!(
            ";; {} ({}): {} CLIF instructions, {} vector-typed; {} code bytes\n{clif}\n;; machine code ({})\n{machine}\n",
            f.symbol,
            f.source,
            instructions,
            vector,
            bytes.len(),
            isa.triple(),
        ));
        entries.push(
            json!({"symbol":f.symbol,"source":f.source,"clif":clif,"machine":machine,
            "clif_instructions":instructions,"vector_clif_instructions":vector,
            "machine_vector_register_lines":vector_lines,"code_bytes":bytes.len(),
            "code_blake3":blake3::hash(bytes).to_hex().to_string()}),
        );
    }
    Ok(Listing {
        data: json!({"target":isa.triple().to_string(),"functions":entries,
            "scope":"the benchmark's unoptimized IR recompiled with the benchmark JIT's ISA flags; code bytes are this recompilation's, not read from JIT memory"}),
        text,
        vector_instructions: total_vector,
    })
}
fn vectorization_status(listing: &Listing) -> &'static str {
    if listing.vector_instructions > 0 {
        "vectorized"
    } else {
        "scalar"
    }
}
fn baseline_path_json(options: &Options) -> Json {
    match &options.compare {
        Some(rev) => json!(format!("git:{rev}")),
        None => json!(options.baseline),
    }
}
fn baseline_label(options: &Options) -> String {
    match &options.compare {
        Some(rev) => format!("git:{rev}"),
        None => options.baseline.display().to_string(),
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
    windows: &[ObservedWindow<CliObservation>],
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
        let d = w
            .observation
            .core
            .as_ref()
            .ok_or("PMU window lacks counters")?;
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
/// The core cycles/instructions provider `--pmu-provider` selected.
enum CoreCounters {
    Group(PerfGroup),
    Rdpmc(RdpmcCounters),
    Kpc(KpcCounters),
}
enum CoreToken {
    Group(vesper_bench_pmu::Snapshot),
    Rdpmc(vesper_bench_pmu::RdpmcSnapshot),
    Kpc(vesper_bench_pmu::KpcSnapshot),
}
/// Every observer a run requested, nested so that the allocation counter is
/// innermost (closest to the body), then the core counters, then the
/// optional extended events.
#[derive(Default)]
struct CliObserver {
    core: Option<CoreCounters>,
    events: Option<PerfEventSet>,
    alloc: Option<AllocationObserver>,
}
struct CliToken {
    events: Option<vesper_bench_pmu::EventSetToken>,
    core: Option<CoreToken>,
    alloc: Option<vesper_bench_pmu::alloc::AllocationToken>,
}
struct CliObservation {
    core: Option<CounterDelta>,
    events: Option<EventDelta>,
    alloc: Option<AllocationDelta>,
}
impl WindowObserver for CliObserver {
    type Token = CliToken;
    type Observation = CliObservation;
    type Error = Unavailable;
    fn before_window(&mut self, i: Invocation, n: usize) -> Result<CliToken, Unavailable> {
        let events = match &mut self.events {
            Some(e) => Some(e.before_window(i, n)?),
            None => None,
        };
        let core = match &mut self.core {
            Some(CoreCounters::Group(c)) => Some(CoreToken::Group(c.before_window(i, n)?)),
            Some(CoreCounters::Rdpmc(c)) => Some(CoreToken::Rdpmc(c.before_window(i, n)?)),
            Some(CoreCounters::Kpc(c)) => Some(CoreToken::Kpc(c.before_window(i, n)?)),
            None => None,
        };
        let alloc = match &mut self.alloc {
            Some(a) => Some(a.before_window(i, n)?),
            None => None,
        };
        Ok(CliToken {
            events,
            core,
            alloc,
        })
    }
    fn after_window(
        &mut self,
        token: CliToken,
        completion: WindowCompletion,
    ) -> Result<CliObservation, Unavailable> {
        let stale = || Unavailable {
            reason: UnavailableReason::ForeignOrStaleToken,
            operation: "bench observer token",
            os_error: None,
        };
        let alloc = match (&mut self.alloc, token.alloc) {
            (Some(a), Some(t)) => Some(a.after_window(t, completion.clone())?),
            (None, None) => None,
            _ => return Err(stale()),
        };
        let core = match (&mut self.core, token.core) {
            (Some(CoreCounters::Group(c)), Some(CoreToken::Group(t))) => {
                Some(c.after_window(t, completion.clone())?)
            }
            (Some(CoreCounters::Rdpmc(c)), Some(CoreToken::Rdpmc(t))) => {
                Some(c.after_window(t, completion.clone())?)
            }
            (Some(CoreCounters::Kpc(c)), Some(CoreToken::Kpc(t))) => {
                Some(c.after_window(t, completion.clone())?)
            }
            (None, None) => None,
            _ => return Err(stale()),
        };
        let events = match (&mut self.events, token.events) {
            (Some(e), Some(t)) => Some(e.after_window(t, completion)?),
            (None, None) => None,
            _ => return Err(stale()),
        };
        Ok(CliObservation {
            core,
            events,
            alloc,
        })
    }
}
fn open_core(provider: Provider) -> Result<CoreCounters, Unavailable> {
    Ok(match provider {
        Provider::LinuxGroupRead => CoreCounters::Group(PerfGroup::open()?),
        Provider::LinuxRdpmc => CoreCounters::Rdpmc(RdpmcCounters::open()?),
        Provider::MacosKpc => CoreCounters::Kpc(KpcCounters::open()?),
    })
}
/// Allocation evidence over the measured (non-warmup) windows.
fn allocation_json(windows: &[ObservedWindow<CliObservation>]) -> Result<(Json, bool), String> {
    let mut total = AllocationCounts::default();
    let mut allocating_windows = 0usize;
    let mut measured = 0usize;
    for w in windows.iter().filter(|w| !w.invocation.warmup) {
        let c = w
            .observation
            .alloc
            .as_ref()
            .ok_or("allocation window lacks counts")?
            .counts;
        measured += 1;
        allocating_windows += usize::from(!c.is_zero_allocation());
        let add = |a: u64, b: u64| a.checked_add(b).ok_or("allocation count overflow");
        total = AllocationCounts {
            allocations: add(total.allocations, c.allocations)?,
            reallocations: add(total.reallocations, c.reallocations)?,
            deallocations: add(total.deallocations, c.deallocations)?,
            bytes: add(total.bytes, c.bytes)?,
        };
    }
    if measured == 0 {
        return Err("no measured windows for allocation evidence".into());
    }
    let zero = allocating_windows == 0;
    Ok((
        json!({"status":if zero {"zero"} else {"allocated"},"measured_windows":measured,
            "allocating_windows":allocating_windows,"allocations":total.allocations.to_string(),
            "reallocations":total.reallocations.to_string(),"deallocations":total.deallocations.to_string(),
            "bytes":total.bytes.to_string(),
            "scope":"counting global allocator, measuring thread only, around each measured window after warmup; includes the harness's dispatch, clock and result storage inside the window"}),
        zero,
    ))
}
/// One text line per requested event: its total or why it is unavailable.
fn events_text(data: &Json, requested: &[HardwareEvent]) -> String {
    let reason = |u: &Json| {
        format!(
            "{} at {}{}",
            u["reason"].as_str().unwrap_or("unknown"),
            u["operation"].as_str().unwrap_or("?"),
            u["errno"]
                .as_i64()
                .map_or(String::new(), |e| format!(" (OS error {e})"))
        )
    };
    if data["status"] == "unavailable" {
        return format!(
            "PMU events: all unavailable: {}\n",
            reason(&data["unavailable"])
        );
    }
    let mut text = String::new();
    for event in requested {
        let e = &data["events"][event.name()];
        if e["status"] == "counted" {
            text.push_str(&format!(
                "PMU event {}: {} in measured windows\n",
                event.name(),
                e["total"].as_str().unwrap_or("?")
            ));
        } else {
            text.push_str(&format!(
                "PMU event {}: unavailable: {}\n",
                event.name(),
                if e["unavailable"].is_object() {
                    reason(&e["unavailable"])
                } else {
                    e["reason"].as_str().unwrap_or("unknown").to_owned()
                }
            ));
        }
    }
    text
}
/// Per-event totals and the per-window `PmuSampleWire` records.
fn events_json(windows: &[ObservedWindow<CliObservation>], requested: &[HardwareEvent]) -> Json {
    let mut summary = serde_json::Map::new();
    for event in requested {
        let mut total = Some(0u64);
        let mut reason = None;
        for w in windows.iter().filter(|w| !w.invocation.warmup) {
            match w.observation.events.as_ref().and_then(|e| e.get(*event)) {
                Some(Ok(n)) => total = total.and_then(|t| t.checked_add(*n)),
                Some(Err(u)) => {
                    total = None;
                    reason.get_or_insert_with(|| u.clone());
                }
                None => total = None,
            }
        }
        summary.insert(
            event.name().into(),
            match (total, reason) {
                (Some(n), None) => json!({"status":"counted","total":n.to_string()}),
                (_, Some(u)) => json!({"status":"unavailable","unavailable":unavailable_json(&u)}),
                (None, None) => json!({"status":"unavailable","reason":"overflow or missing"}),
            },
        );
    }
    let mut samples = Vec::new();
    for (index, w) in windows.iter().enumerate() {
        let mut sample = PmuSample::new();
        if let Some(core) = &w.observation.core {
            let _ = sample.set(PmuField::InstructionsRetired, core.instructions());
        }
        let event = |e| match w.observation.events.as_ref().and_then(|d| d.get(e)) {
            Some(Ok(n)) => Some(*n),
            _ => None,
        };
        for (field, e) in [
            (PmuField::RefCycles, HardwareEvent::RefCycles),
            (PmuField::L1dReadMisses, HardwareEvent::L1dReadMisses),
            (PmuField::LlcMisses, HardwareEvent::LlcReadMisses),
            (
                PmuField::BranchMispredicts,
                HardwareEvent::BranchMispredicts,
            ),
            (PmuField::SimdVecUopsRetired, HardwareEvent::SimdOperations),
        ] {
            if let Some(n) = event(e) {
                let _ = sample.set(field, n);
            }
        }
        if let (Some(be), Some(slots)) = (
            event(HardwareEvent::TmaBackendBoundSlots),
            event(HardwareEvent::TmaSlots),
        ) && let Some(bp) = backend_bound_bp(be, slots)
        {
            let _ = sample.set(PmuField::TmaBackendBoundSlotsBp, bp);
        }
        if let Ok((wire, mask)) = sample.to_wire_masked()
            && let Ok(bytes) = wire.encode()
        {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            samples.push(
                json!({"window":index,"warmup":w.invocation.warmup,"wire_hex":hex,
                "available_mask":mask.bits(),
                "unavailable":sample.unavailable().iter().map(|f| f.name()).collect::<Vec<_>>()}),
            );
        }
    }
    json!({"events":summary,"pmu_sample_wire":{"schema":"PmuSampleWire/64-byte little-endian (§41.2)",
        "note":"fields outside available_mask are encoded as 0 and are not measurements; arena_bump_bytes and icache_rand_offset_bytes are never measured here (no region arena, no code-entry jitter)",
        "samples":samples}})
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
/// Static evidence and CAS provenance shared by every report shape.
struct Extras {
    cas: Option<Json>,
    listings: Option<(Listing, Listing)>,
}
fn attach_extras(options: &Options, extras: Extras, outcome: &mut Outcome) {
    if let Some(cas) = extras.cas {
        outcome.data["baseline"]["cas"] = cas;
    }
    if let Some((candidate, baseline)) = extras.listings {
        let status = vectorization_status(&candidate);
        outcome.data["evidence"]["vectorization"] = json!(status);
        outcome.data["vectorization"] = json!({"candidate":status,"baseline":vectorization_status(&baseline),
            "candidate_vector_clif_instructions":candidate.vector_instructions,
            "baseline_vector_clif_instructions":baseline.vector_instructions,
            "method":"vector-typed CLIF values in every compiled function of the measured closure (Cranelift emits SIMD machine instructions only for those)"});
        if options.asm {
            outcome.data["asm"] = json!({"candidate":candidate.data,"baseline":baseline.data});
            outcome.text.push_str(";; candidate\n");
            outcome.text.push_str(&candidate.text);
            outcome.text.push_str(";; baseline\n");
            outcome.text.push_str(&baseline.text);
        }
        if options.expect_vectorized {
            let passed = status == "vectorized";
            outcome.data["vectorization"]["gate"] = json!(if passed { "Pass" } else { "Fail" });
            if !passed {
                add_failure(
                    outcome,
                    "--expect-vectorized: the candidate's compiled functions contain no vector instructions",
                );
            }
        }
    }
}
fn add_failure(outcome: &mut Outcome, message: &str) {
    outcome.text.push_str(message);
    outcome.text.push('\n');
    outcome.gate_failure = Some(match outcome.gate_failure.take() {
        Some(first) => format!("{first}; {message}"),
        None => message.to_owned(),
    });
}
fn measure(options: Options) -> Result<Option<Outcome>, String> {
    // Baseline resolution and both compilations precede every timed window.
    let cas = match &options.compare {
        Some(rev) => {
            let store = match &options.store {
                Some(dir) => dir.clone(),
                None => default_store()?,
            };
            Some((
                load_cas_baseline(&store, &SystemGit, &options.path, rev)?,
                rev.clone(),
            ))
        }
        None => None,
    };
    let mut baseline = match &cas {
        Some((c, rev)) => SourceCase::compile_text(&c.source, &options.baseline_entry)
            .map_err(|error| format!("baseline git:{rev}:{}: {error}", c.git.path))?,
        None => SourceCase::compile(&options.baseline, &options.baseline_entry)
            .map_err(|error| format!("baseline {}: {error}", options.baseline.display()))?,
    };
    let mut candidate = SourceCase::compile(&options.path, &options.entry)
        .map_err(|error| format!("candidate {}: {error}", options.path.display()))?;
    if cas.is_none()
        && options.path == options.baseline
        && baseline.source_id != candidate.source_id
    {
        return Err("source changed while preparing the same-file comparison".into());
    }
    let listings = if options.asm || options.expect_vectorized {
        Some((asm_listing(&candidate)?, asm_listing(&baseline)?))
    } else {
        None
    };
    let extras = Extras {
        cas: cas.as_ref().map(|(c, rev)| cas_json(c, rev)),
        listings,
    };
    if options.deterministic {
        let mut outcome = deterministic_outcome(&options, &mut baseline, &mut candidate)?;
        attach_extras(&options, extras, &mut outcome);
        return Ok(Some(outcome));
    }
    let observed = options.pmu || options.expect_zero_alloc || !options.events.is_empty();
    let mut alloc_unavailable = None;
    let mut events_unavailable = None;
    let (report, pmu, windows) = if observed {
        let mut observer = CliObserver::default();
        if options.pmu {
            match open_core(options.provider) {
                Ok(core) => observer.core = Some(core),
                Err(e) => {
                    let mut outcome = unavailable_outcome(options.metric, options.provider, &e);
                    attach_extras(&options, extras, &mut outcome);
                    return Ok(Some(outcome));
                }
            }
        }
        if options.expect_zero_alloc {
            match AllocationObserver::open() {
                Ok(a) => observer.alloc = Some(a),
                Err(e) => alloc_unavailable = Some(e),
            }
        }
        if !options.events.is_empty() {
            match PerfEventSet::open(&options.events) {
                Ok(e) => observer.events = Some(e),
                Err(e) => events_unavailable = Some(e),
            }
        }
        let (report, windows) =
            match run_abba_observed(options.config, &mut baseline, &mut candidate, &mut observer) {
                Ok(run) => run.into_parts(),
                Err(e) => {
                    let mut outcome = observed_error_outcome(options.metric, options.provider, e);
                    attach_extras(&options, extras, &mut outcome);
                    return Ok(Some(outcome));
                }
            };
        let pmu = if options.pmu {
            Some(validate_pmu(
                options.config,
                &report,
                &windows,
                options.provider,
            )?)
        } else {
            None
        };
        (report, pmu, windows)
    } else {
        (
            run_abba(options.config, &mut baseline, &mut candidate).map_err(|e| e.to_string())?,
            None,
            Vec::new(),
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
        baseline_label(&options),
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
        "baseline": {"path": baseline_path_json(&options), "entry": options.baseline_entry, "source_blake3": baseline.source_id, "jit": stats_json(baseline.runner.stats())},
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
    let mut outcome = Outcome {
        data,
        text,
        gate_failure,
    };
    if options.expect_zero_alloc {
        match alloc_unavailable {
            Some(e) => {
                outcome.data["allocation"] =
                    json!({"status":"unavailable","unavailable":unavailable_json(&e)});
                add_failure(
                    &mut outcome,
                    &format!("--expect-zero-alloc: allocation evidence unavailable ({e})"),
                );
            }
            None => {
                let (allocation, zero) = allocation_json(&windows)?;
                outcome.data["evidence"]["zero_allocation"] = allocation["status"].clone();
                let summary = format!(
                    "allocations in measured windows: {} ({} of {} windows allocated)",
                    allocation["allocations"].as_str().unwrap_or("?"),
                    allocation["allocating_windows"],
                    allocation["measured_windows"],
                );
                outcome.data["allocation"] = allocation;
                if zero {
                    outcome.text.push_str(&summary);
                    outcome.text.push_str("; zero-allocation gate passed\n");
                } else {
                    add_failure(&mut outcome, &format!("--expect-zero-alloc: {summary}"));
                }
            }
        }
    }
    if !options.events.is_empty() {
        outcome.data["pmu_events"] = match events_unavailable {
            Some(e) => {
                json!({"status":"unavailable","unavailable":unavailable_json(&e),"host":host_json()})
            }
            None => {
                let mut data = events_json(&windows, &options.events);
                data["host"] = host_json();
                data
            }
        };
        let text = events_text(&outcome.data["pmu_events"], &options.events);
        outcome.text.push_str(&text);
    }
    attach_extras(&options, extras, &mut outcome);
    Ok(Some(outcome))
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
            "--compare git:",
            "--deterministic",
            "--asm",
            "--expect-zero-alloc",
            "--expect-vectorized",
            "--pmu-events",
        ] {
            assert!(HELP.contains(mechanism), "help lacks {mechanism}");
        }
        // Claims the command does not make (CAS baselines and --asm exist
        // since wave-eight lane 15; the README row may mention them).
        for absent in ["12-cycle", "--causal"] {
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

#[cfg(test)]
mod metronome_rest_tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "vesper-bench-rest-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(plain_path(&fs::canonicalize(path).unwrap()))
        }
        fn file(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, text).unwrap();
            path
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }
    fn run_cli(list: &[&str]) -> Outcome {
        execute(args(list)).unwrap().unwrap()
    }
    const SMALL: &str = "fn main()->i64{wrapping_add(1,2)}";
    const LARGER: &str =
        "fn f(x:i64)->i64{wrapping_mul(x,x)} fn main()->i64{wrapping_add(f(3),f(4))}";

    #[test]
    fn new_flags_parse_and_reject_unsupported_combinations() {
        let o = options(args(&[
            "c.ves",
            "--compare",
            "git:main~2",
            "--store",
            "s",
            "--asm",
            "--expect-vectorized",
            "--expect-zero-alloc",
            "--pmu-events",
            "l1d-read-misses,tma-slots,l1d-read-misses",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(o.compare.as_deref(), Some("main~2"));
        assert!(o.asm && o.expect_vectorized && o.expect_zero_alloc);
        assert_eq!(
            o.events,
            [HardwareEvent::L1dReadMisses, HardwareEvent::TmaSlots]
        );
        assert!(
            options(args(&["c.ves", "--deterministic"]))
                .unwrap()
                .unwrap()
                .deterministic
        );
        for bad in [
            vec!["c.ves", "--compare", "main"],
            vec!["c.ves", "--compare", "git:"],
            vec!["c.ves", "--compare", "git:-x"],
            vec!["c.ves", "--compare", "git:a b"],
            vec!["c.ves", "--compare", "git:a:b"],
            vec!["c.ves", "--compare", "git:a", "--baseline", "b.ves"],
            vec!["c.ves", "--store", "s"],
            vec!["c.ves", "--deterministic", "--pmu"],
            vec!["c.ves", "--deterministic", "--metric", "cycles"],
            vec!["c.ves", "--deterministic", "--expect-zero-alloc"],
            vec!["c.ves", "--deterministic", "--randomize-layout", "6"],
            vec!["c.ves", "--asm", "--randomize-layout", "6"],
            vec!["c.ves", "--pmu-events", "cycles"],
        ] {
            assert!(options(args(&bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn deterministic_mode_is_exact_and_gates_the_exact_ratio() {
        let dir = Dir::new("det");
        let small = dir.file("small.ves", SMALL);
        let larger = dir.file("larger.ves", LARGER);
        let same = run_cli(&[small.to_str().unwrap(), "--deterministic", "--json"]);
        assert_eq!(same.data["mode"], "deterministic");
        assert_eq!(
            same.data["candidate"]["steps_per_call"],
            same.data["baseline"]["steps_per_call"]
        );
        assert_eq!(same.data["regression_basis_points"]["floor"], "0");
        let slower = run_cli(&[
            larger.to_str().unwrap(),
            "--baseline",
            small.to_str().unwrap(),
            "--deterministic",
            "--max-regression-bp",
            "100",
        ]);
        let cand: u64 = slower.data["candidate"]["steps_per_call"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let base: u64 = slower.data["baseline"]["steps_per_call"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(cand > base, "{cand} vs {base}");
        assert_eq!(slower.data["status"], "regression");
        assert!(slower.gate_failure.is_some());
        assert_eq!(slower.data["candidate"]["result"], "25");
        // The reverse comparison passes a 0 bp gate; reruns are identical.
        let faster = run_cli(&[
            small.to_str().unwrap(),
            "--baseline",
            larger.to_str().unwrap(),
            "--deterministic",
            "--max-regression-bp",
            "0",
        ]);
        assert_eq!(faster.data["status"], "passed");
        assert!(faster.gate_failure.is_none());
        let again = run_cli(&[
            small.to_str().unwrap(),
            "--baseline",
            larger.to_str().unwrap(),
            "--deterministic",
            "--max-regression-bp",
            "0",
        ]);
        assert_eq!(
            again.data["regression_basis_points"],
            faster.data["regression_basis_points"]
        );
    }

    #[test]
    fn asm_lists_clif_and_machine_code_and_vectorization_gate_fails_on_scalar() {
        let dir = Dir::new("asm");
        let k = dir.file("k.ves", LARGER);
        let o = run_cli(&[
            k.to_str().unwrap(),
            "--asm",
            "--samples",
            "1",
            "--iterations",
            "2",
        ]);
        let functions = o.data["asm"]["candidate"]["functions"].as_array().unwrap();
        assert_eq!(functions.len(), 2, "main and f");
        for f in functions {
            assert!(f["clif"].as_str().unwrap().contains("function u0:"));
            assert!(f["machine"].as_str().unwrap().lines().count() > 2);
            assert!(f["code_bytes"].as_u64().unwrap() > 0);
            assert_eq!(f["vector_clif_instructions"], 0);
        }
        assert!(
            functions
                .iter()
                .any(|f| f["clif"].as_str().unwrap().contains("imul"))
        );
        assert_eq!(o.data["evidence"]["vectorization"], "scalar");
        assert!(o.text.contains(";; machine code ("));
        assert!(o.gate_failure.is_none());
        let gated = run_cli(&[
            k.to_str().unwrap(),
            "--expect-vectorized",
            "--samples",
            "1",
            "--iterations",
            "2",
        ]);
        assert_eq!(gated.data["vectorization"]["gate"], "Fail");
        assert!(gated.data.get("asm").is_none());
        assert!(gated.gate_failure.unwrap().contains("--expect-vectorized"));
        assert!(vector_register_line("  addps %xmm1, %xmm0"));
        assert!(vector_register_line("  add v0.4s, v1.4s, v2.4s"));
        assert!(!vector_register_line("  addq %rsi, %rax"));
    }

    #[test]
    fn zero_alloc_gate_uses_measuring_thread_counts() {
        let dir = Dir::new("alloc");
        let k = dir.file("k.ves", LARGER);
        let o = run_cli(&[
            k.to_str().unwrap(),
            "--expect-zero-alloc",
            "--samples",
            "6",
            "--iterations",
            "32",
        ]);
        assert_eq!(o.data["allocation"]["measured_windows"], 24);
        assert_eq!(o.data["evidence"]["zero_allocation"], "zero");
        assert_eq!(o.data["allocation"]["allocations"], "0");
        assert!(o.gate_failure.is_none(), "{:?}", o.gate_failure);
        // Synthetic windows: one allocating measured window fails; warmups do not count.
        let window = |warmup, allocations| ObservedWindow {
            invocation: Invocation {
                warmup,
                ..expected_invocation(RunConfig::default(), 0)
            },
            completion: WindowCompletion {
                invocation: expected_invocation(RunConfig::default(), 0),
                planned_operations: 1,
                attempted_operations: 1,
                successful_operations: 1,
                body: BodyStatus::Completed,
                clock: ClockStatus::Measured(1),
            },
            observation: CliObservation {
                core: None,
                events: None,
                alloc: Some(AllocationDelta {
                    counts: AllocationCounts {
                        allocations,
                        bytes: allocations * 8,
                        ..AllocationCounts::default()
                    },
                }),
            },
        };
        let (data, zero) = allocation_json(&[window(true, 5), window(false, 0)]).unwrap();
        assert!(zero && data["status"] == "zero");
        let (data, zero) = allocation_json(&[window(false, 0), window(false, 2)]).unwrap();
        assert!(!zero);
        assert_eq!(
            (data["allocating_windows"].as_u64(), data["bytes"].as_str()),
            (Some(1), Some("16"))
        );
        assert!(allocation_json(&[window(true, 0)]).is_err());
    }

    #[test]
    fn pmu_events_are_counted_or_reported_with_wire_records() {
        let dir = Dir::new("events");
        let k = dir.file("k.ves", SMALL);
        let o = run_cli(&[
            k.to_str().unwrap(),
            "--pmu-events",
            "ref-cycles,l1d-read-misses,llc-read-misses,branch-mispredicts,simd-ops,tma-slots,tma-backend-bound",
            "--samples",
            "1",
            "--warmup",
            "0",
            "--iterations",
            "2",
        ]);
        let events = &o.data["pmu_events"];
        if events["status"] == "unavailable" {
            eprintln!("BENCH_EVENTS_UNAVAILABLE: {}", events["unavailable"]);
            return;
        }
        for event in HardwareEvent::ALL {
            let status = &events["events"][event.name()]["status"];
            assert!(status == "counted" || status == "unavailable", "{status}");
            if status == "unavailable" {
                eprintln!(
                    "BENCH_EVENT_UNAVAILABLE: {} {}",
                    event.name(),
                    events["events"][event.name()]
                );
            }
        }
        for event in HardwareEvent::ALL {
            assert!(
                o.text.contains(&format!("PMU event {}: ", event.name())),
                "text report lacks {}",
                event.name()
            );
        }
        let samples = events["pmu_sample_wire"]["samples"].as_array().unwrap();
        assert_eq!(samples.len(), 4);
        for sample in samples {
            let hex = sample["wire_hex"].as_str().unwrap();
            assert_eq!(hex.len(), 128);
            let bytes: Vec<u8> = (0..64)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
                .collect();
            let wire = vesper_bench::wire::PmuSampleWire::decode(&bytes).unwrap();
            let mask = vesper_bench::wire::AvailabilityMask::new(
                sample["available_mask"].as_u64().unwrap() as u16,
            )
            .unwrap();
            for field in PmuField::ALL {
                if !mask.contains(field) {
                    assert_eq!(wire.get(field), 0, "{}", field.name());
                }
            }
            // Never measured by this command.
            assert!(!mask.contains(PmuField::ArenaBumpBytes));
            assert!(!mask.contains(PmuField::IcacheRandOffsetBytes));
        }
    }

    /// Scripted git: a commit, a toplevel and one blob per `commit:path`.
    struct FakeGit {
        commit: String,
        top: PathBuf,
        blobs: BTreeMap<String, Vec<u8>>,
        calls: RefCell<Vec<String>>,
    }
    impl Git for FakeGit {
        fn run(&self, _: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
            self.calls.borrow_mut().push(args.join(" "));
            match args {
                ["rev-parse", "--verify", "--quiet", "--end-of-options", spec] => {
                    if spec.starts_with("main") {
                        Ok(format!("{}\n", self.commit).into_bytes())
                    } else {
                        Err("git rev-parse: exit status: 1".into())
                    }
                }
                ["rev-parse", "--show-toplevel"] => {
                    Ok(format!("{}\n", self.top.display()).into_bytes())
                }
                ["cat-file", "-s", spec] => self
                    .blobs
                    .get(*spec)
                    .map(|b| format!("{}\n", b.len()).into_bytes())
                    .ok_or_else(|| "git cat-file: missing".into()),
                ["cat-file", "blob", spec] => self
                    .blobs
                    .get(*spec)
                    .cloned()
                    .ok_or_else(|| "git cat-file: missing".into()),
                _ => Err(format!("unexpected git {args:?}")),
            }
        }
    }

    #[test]
    fn cas_baselines_are_populated_once_then_reused_and_verified() {
        let dir = Dir::new("cas");
        fs::create_dir_all(dir.0.join("src")).unwrap();
        let candidate = dir.file("src/k.ves", LARGER);
        let store = dir.0.join("store");
        let commit = "ab".repeat(20);
        let git = FakeGit {
            commit: commit.clone(),
            top: dir.0.clone(),
            blobs: BTreeMap::from([(format!("{commit}:src/k.ves"), SMALL.as_bytes().to_vec())]),
            calls: RefCell::new(Vec::new()),
        };
        let first = load_cas_baseline(&store, &git, &candidate, "main").unwrap();
        assert_eq!((first.cache, first.source.as_str()), ("populated", SMALL));
        assert_eq!(first.git.path, "src/k.ves");
        assert_eq!(first.git.commit, commit);
        git.calls.borrow_mut().clear();
        let second = load_cas_baseline(&store, &git, &candidate, "main").unwrap();
        assert_eq!((second.cache, second.archive), ("hit", first.archive));
        assert!(
            git.calls
                .borrow()
                .iter()
                .all(|c| !c.starts_with("cat-file")),
            "a cache hit must not read the blob from git: {:?}",
            git.calls.borrow()
        );
        // The archive decodes only for its own commit and path, and only intact.
        let archive = Store::open(&store)
            .unwrap()
            .read_blob(&first.archive)
            .unwrap();
        assert_eq!(decode_archive(&archive, &first.git).unwrap(), SMALL);
        let other = GitBaseline {
            path: "other.ves".into(),
            ..first.git.clone()
        };
        assert!(decode_archive(&archive, &other).is_err());
        let mut tampered: Json = serde_json::from_slice(&archive).unwrap();
        tampered["source"] = json!("fn main()->i64{9}");
        assert!(decode_archive(&serde_json::to_vec(&tampered).unwrap(), &first.git).is_err());
        assert!(decode_archive(b"not json", &first.git).is_err());
        // Unknown revisions, non-commit output and outside paths are errors.
        assert!(load_cas_baseline(&store, &git, &candidate, "nope").is_err());
        let short = FakeGit {
            commit: "abc".into(),
            ..git
        };
        assert!(resolve_git(&short, &candidate, "main").is_err());
        let elsewhere = FakeGit {
            top: dir.0.join("src").join("deeper"),
            commit: commit.clone(),
            blobs: BTreeMap::new(),
            calls: RefCell::new(Vec::new()),
        };
        fs::create_dir_all(&elsewhere.top).unwrap();
        assert!(resolve_git(&elsewhere, &candidate, "main").is_err());
        let huge = FakeGit {
            top: dir.0.clone(),
            commit: "cd".repeat(20),
            blobs: BTreeMap::from([(
                format!("{}:src/k.ves", "cd".repeat(20)),
                vec![b' '; MAX_SOURCE_BYTES as usize + 1],
            )]),
            calls: RefCell::new(Vec::new()),
        };
        assert!(
            load_cas_baseline(&store, &huge, &candidate, "main")
                .err()
                .unwrap()
                .contains("256 KiB")
        );
    }

    #[test]
    fn compare_git_runs_against_a_real_repository_when_git_exists() {
        let dir = Dir::new("git");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir.0)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
        };
        match git(&["--version"]) {
            Ok(out) if out.status.success() => {}
            _ => {
                let error = SystemGit.run(&dir.0, &["--version"]).err();
                eprintln!("GIT_UNAVAILABLE: {error:?}; real-git path not exercised");
                return;
            }
        }
        let ok = |args: &[&str]| assert!(git(args).unwrap().status.success(), "git {args:?}");
        ok(&["init", "-q"]);
        let k = dir.file("k.ves", SMALL);
        ok(&["add", "k.ves"]);
        ok(&[
            "-c",
            "user.name=bench",
            "-c",
            "user.email=bench@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "baseline",
        ]);
        fs::write(&k, LARGER).unwrap();
        let store = dir.0.join(".store");
        let flags = [
            k.to_str().unwrap(),
            "--compare",
            "git:HEAD",
            "--store",
            store.to_str().unwrap(),
            "--deterministic",
            "--max-regression-bp",
            "0",
        ];
        let first = run_cli(&flags);
        assert_eq!(first.data["baseline"]["cas"]["cache"], "populated");
        assert_eq!(first.data["baseline"]["path"], "git:HEAD");
        assert_eq!(first.data["baseline"]["result"], "3");
        assert_eq!(first.data["candidate"]["result"], "25");
        assert_eq!(first.data["status"], "regression");
        let second = run_cli(&flags);
        assert_eq!(second.data["baseline"]["cas"]["cache"], "hit");
        assert_eq!(
            second.data["baseline"]["cas"]["archive"],
            first.data["baseline"]["cas"]["archive"]
        );
        // The measured (non-deterministic) path accepts the same baseline.
        let timed = run_cli(&[
            k.to_str().unwrap(),
            "--compare",
            "git:HEAD",
            "--store",
            store.to_str().unwrap(),
            "--samples",
            "1",
            "--iterations",
            "2",
        ]);
        assert_eq!(timed.data["status"], "measured");
        assert_eq!(timed.data["baseline"]["cas"]["cache"], "hit");
        assert!(
            execute(args(&[
                k.to_str().unwrap(),
                "--compare",
                "git:does-not-exist",
                "--store",
                store.to_str().unwrap()
            ]))
            .is_err()
        );
    }
}
