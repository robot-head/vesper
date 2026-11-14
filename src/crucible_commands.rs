//! `vesper fuzz` and `vesper verify --refute`: Vesper Crucible (§29.1) over
//! source functions (integration B, issue #136).
//!
//! A target is a checked source function whose parameters are `i64`, `u64`,
//! `u32`, `u8` or `bool`. Each Crucible execution draws one value per
//! parameter from the ChoiceTape and calls the function in the evaluator.
//! Coverage feedback is coarse: the outcome class and the result value are
//! reported as probe edges. With `--native-coverage` the target instead runs
//! as Cranelift code instrumented with edge counters and comparison-operand
//! logging ([`vesper::native_coverage`], lane 23), which feed Crucible's
//! corpus selection and input-to-state solver; the evaluator still runs every
//! input and must agree on success or failure.
//!
//! * `fuzz` fails when the evaluator returns an error (an escaping `Fail`,
//!   an abort, fuel exhaustion is *not* a failure but an invalid run), when
//!   `--native` JIT code disagrees with the evaluator, or, with
//!   `--differential <shadow>`, when the shadow function's outcome differs.
//! * `verify --refute` searches for inputs on which a `bool` contract
//!   function returns `false`, optionally under a `--requires` predicate,
//!   and reports a counterexample or "no counterexample found within budget".
//!
//! Counterexamples are shrunk by Crucible and can be written as regression
//! functions (`// @test regression` comment plus a zero-argument function
//! that calls the target with the shrunk arguments) into a user-named file:
//! the file is the input module followed by the regression cases, so it
//! checks on its own, and is canonically formatted.
use std::{
    fs,
    path::{Path, PathBuf},
};
use vesper::{
    ast::{Function, Type},
    check::{self, World},
    parser,
    runtime::{Runtime, Value},
};
use vesper_crucible::{
    ChoiceTape, FuzzConfig, Invalid, PropError, Refutation, Source, Strategy, Verdict,
};

const FUZZ_HELP: &str = "Usage: vesper fuzz <file.ves> --target <fn> [options]\n\nRun Crucible coverage-guided fuzzing over a source function in the evaluator.\nParameters must be i64, u64, u32, u8 or bool.\n\nOptions:\n  --target <fn>          Function to fuzz (required)\n  --native               Also run each input through the Cranelift JIT and compare\n  --native-coverage      Run the target as instrumented native code: edge counters\n                         and comparison operands guide the corpus and solver\n                         (with --differential, the shadow runs natively too)\n  --differential <fn>    Compare every outcome with a shadow implementation\n  --corpus <dir>         Replay saved tapes first, then save the campaign corpus\n  --regressions <file>   Write found failures as @test regression cases\n  --execs <n>            Execution budget (default 2000, 200 with --native)\n  --seed <n>             Campaign seed (default 0xc0ffee)\n  --fuel <steps>         Evaluator step limit per run (default 100000); with\n                         --native-coverage also the native block-entry budget\n  --random               Unguided random search\n";
const REFUTE_HELP: &str = "Usage: vesper verify --refute <file.ves> --contract <fn> [options]\n\nSearch for a counterexample to a bool-returning contract with Crucible before Lean proving.\n\nOptions:\n  --contract <fn>        Contract predicate (alias --target, required)\n  --requires <fn>        Precondition predicate over the same parameters\n  --native-coverage      Run the contract as instrumented native code whose edges\n                         and comparison operands guide the search; every result\n                         is cross-checked with the evaluator\n  --regressions <file>   Write a refutation as an @test regression case\n  --execs <n>            Execution budget (default 5000)\n  --seed <n>             Campaign seed (default 0xc0ffee)\n  --fuel <steps>         Evaluator step limit per run (default 100000)\n";

/// Supported parameter carriers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carrier {
    I64,
    U64,
    U32,
    U8,
    Bool,
}

impl Carrier {
    fn of(ty: &Type) -> Option<Self> {
        match ty.name() {
            "i64" => Some(Carrier::I64),
            "u64" => Some(Carrier::U64),
            "u32" => Some(Carrier::U32),
            "u8" => Some(Carrier::U8),
            "bool" => Some(Carrier::Bool),
            _ => None,
        }
    }
    fn draw(self, src: &mut Source) -> Result<Value, Invalid> {
        use vesper_crucible::Arbitrary;
        Ok(match self {
            Carrier::I64 => Value::Int(i64::arbitrary(src)?),
            Carrier::U64 => Value::UInt(src.draw_u64()?, 64),
            Carrier::U32 => Value::UInt(u64::from(src.draw_u32()?), 32),
            Carrier::U8 => Value::UInt(u64::from(src.draw_u8()?), 8),
            Carrier::Bool => Value::Bool(src.draw_bool()?),
        })
    }
}

/// Source literal of a drawn argument.
fn literal(value: &Value) -> String {
    match value {
        Value::Int(n) => n.to_string(),
        Value::UInt(n, _) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

struct Loaded {
    source: String,
    world: World,
}

fn load(path: &Path) -> Result<Loaded, String> {
    let source = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let world = check_source(&source).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Loaded { source, world })
}

fn check_source(source: &str) -> Result<World, String> {
    let parsed = parser::parse(source);
    if let Some(d) = parsed.diagnostics.first() {
        return Err(format!(
            "{} syntax error(s); first: {} {}",
            parsed.diagnostics.len(),
            d.code,
            d.message
        ));
    }
    let checked = check::check(&parsed.module);
    if let Some(d) = checked.diagnostics.first() {
        return Err(format!(
            "{} semantic error(s); first: {} {}",
            checked.diagnostics.len(),
            d.code,
            d.message
        ));
    }
    Ok(checked.world)
}

/// A function's signature reduced to drawable carriers.
struct Target {
    name: String,
    carriers: Vec<Carrier>,
    result: String,
    effectful: bool,
}

fn target(world: &World, name: &str, role: &str) -> Result<Target, String> {
    let f: &Function = world
        .functions
        .get(name)
        .ok_or_else(|| format!("{role} '{name}' is not a function of the module"))?;
    if !f.generics.is_empty() {
        return Err(format!(
            "{role} '{name}' is generic; fuzz a concrete instance"
        ));
    }
    let mut carriers = Vec::new();
    for p in &f.params {
        if p.pass != vesper::ast::Pass::In {
            return Err(format!(
                "{role} '{name}' parameter '{}' is not passed by value",
                p.name
            ));
        }
        carriers.push(Carrier::of(&p.ty).ok_or_else(|| {
            format!(
                "{role} '{name}' parameter '{}' has type {}; supported: i64, u64, u32, u8, bool",
                p.name,
                p.ty.name()
            )
        })?);
    }
    if f.body.is_none() {
        return Err(format!("{role} '{name}' has no body"));
    }
    Ok(Target {
        name: name.to_string(),
        carriers,
        result: f.result.name().to_string(),
        effectful: !f.effects.instances.is_empty() || f.effects.tail.is_some(),
    })
}

/// The signature text after the parameter list (`-> T eff [...]`), taken
/// from the source so regression cases carry the target's effect row.
fn signature_tail(source: &str, world: &World, name: &str) -> Result<String, String> {
    let f = &world.functions[name];
    let body = f.body.as_ref().ok_or("target has no body")?;
    let header = source
        .get(f.span.start..body.span.start)
        .ok_or("target span is outside the source")?;
    let open = header
        .find('(')
        .ok_or("target header has no parameter list")?;
    let mut depth = 0usize;
    for (i, c) in header[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(header[open + i + 1..].trim().to_string());
                }
            }
            _ => {}
        }
    }
    Err("unbalanced target parameter list".into())
}

/// One evaluator reused across executions (lowering and optimizing the
/// module once); fuel is reset before every call.
struct Evaluator<'a> {
    runtime: std::cell::RefCell<Runtime<'a>>,
    fuel: u64,
}

impl<'a> Evaluator<'a> {
    fn new(world: &'a World, fuel: usize) -> Self {
        Self {
            runtime: std::cell::RefCell::new(Runtime::new(world)),
            fuel: fuel as u64,
        }
    }
    fn call(&self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        let mut runtime = self.runtime.borrow_mut();
        runtime.fuel = self.fuel;
        runtime.run(name, args)
    }
}

/// Stable failure class of an evaluator error: digits removed, so a shrunk
/// input that fails the same way keeps the same kind.
fn error_kind(prefix: &str, message: &str) -> String {
    let class: String = message
        .chars()
        .filter(|c| !c.is_ascii_digit())
        .take(96)
        .collect();
    format!("{prefix}: {class}")
}

fn is_fuel(message: &str) -> bool {
    message.contains("fuel")
}

/// Integer literals of the module, the comparison operands a concolic
/// engine would see first. They are reported as equality comparisons
/// against each drawn argument (in tape encoding), so Crucible's
/// input-to-state solver can substitute them into the tape. This is a
/// source-level stand-in for Cranelift comparison instrumentation.
fn literals(source: &str) -> Vec<u64> {
    let mut out = std::collections::BTreeSet::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() && out.len() < 64 {
        let c = bytes[i];
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if c.is_ascii_digit() && boundary {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let token = source[start..i].replace('_', "");
            let value = match token.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16).ok(),
                None => token.parse().ok(),
            };
            if let Some(v) = value {
                out.insert(v);
            }
            continue;
        }
        i += 1;
    }
    out.into_iter().collect()
}

fn zigzag(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

/// Tape encodings of `literal` for `carrier` (both signs for `i64`).
fn encodings(carrier: Carrier, literal: u64) -> Vec<u64> {
    match carrier {
        Carrier::I64 => i64::try_from(literal)
            .map(|n| vec![zigzag(n), zigzag(-n)])
            .unwrap_or_default(),
        Carrier::U64 => vec![literal],
        Carrier::U32 => (literal <= u64::from(u32::MAX))
            .then_some(literal)
            .into_iter()
            .collect(),
        Carrier::U8 => (literal <= u64::from(u8::MAX))
            .then_some(literal)
            .into_iter()
            .collect(),
        Carrier::Bool => Vec::new(),
    }
}

fn tape_value(value: &Value) -> Option<u64> {
    match value {
        Value::Int(n) => Some(zigzag(*n)),
        Value::UInt(n, _) => Some(*n),
        _ => None,
    }
}

fn report_literals(
    probe: &mut vesper_crucible::Probe,
    carriers: &[Carrier],
    args: &[Value],
    literals: &[u64],
) {
    for (i, (carrier, arg)) in carriers.iter().zip(args).enumerate() {
        let Some(drawn) = tape_value(arg) else {
            continue;
        };
        for (j, literal) in literals.iter().enumerate() {
            for (k, encoded) in encodings(*carrier, *literal).into_iter().enumerate() {
                let site = 0x4000_0000 | ((i as u32) << 16) | ((j as u32) << 1) | k as u32;
                probe.eq(site, drawn, encoded);
            }
        }
    }
}

fn edge_of(outcome: &Result<Value, String>) -> u32 {
    let text = match outcome {
        Ok(v) => format!("ok:{v}"),
        Err(e) => format!("err:{e}"),
    };
    let hash = blake3::hash(text.as_bytes());
    u32::from_le_bytes(hash.as_bytes()[..4].try_into().expect("4 bytes"))
}

fn parse_u64(text: &str, flag: &str) -> Result<u64, String> {
    let parsed = if let Some(hex) = text.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        text.parse()
    };
    parsed.map_err(|_| format!("{flag} requires a non-negative integer"))
}

fn next<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<&'a String, String> {
    it.next().ok_or_else(|| format!("{flag} requires a value"))
}

struct Common {
    input: Option<PathBuf>,
    target: Option<String>,
    regressions: Option<PathBuf>,
    execs: Option<u64>,
    seed: u64,
    fuel: usize,
}

impl Common {
    fn new() -> Self {
        Self {
            input: None,
            target: None,
            regressions: None,
            execs: None,
            seed: 0x00c0_ffee,
            fuel: 100_000,
        }
    }
    /// Returns false when `arg` is not a common option.
    fn take<'a>(
        &mut self,
        arg: &'a String,
        it: &mut impl Iterator<Item = &'a String>,
    ) -> Result<bool, String> {
        match arg.as_str() {
            "--regressions" => {
                self.regressions = Some(PathBuf::from(next(it, arg)?));
            }
            "--execs" => {
                let n = parse_u64(next(it, arg)?, arg)?;
                if n == 0 || n > 10_000_000 {
                    return Err("--execs must be between 1 and 10000000".into());
                }
                self.execs = Some(n);
            }
            "--seed" => self.seed = parse_u64(next(it, arg)?, arg)?,
            "--fuel" => {
                let n = parse_u64(next(it, arg)?, arg)?;
                if n == 0 {
                    return Err("--fuel must be positive".into());
                }
                self.fuel = usize::try_from(n).map_err(|_| "--fuel is too large")?;
            }
            _ if arg.starts_with('-') => return Ok(false),
            _ => {
                if self.input.replace(PathBuf::from(arg)).is_some() {
                    return Err("only one input module is supported".into());
                }
            }
        }
        Ok(true)
    }
}

fn config(seed: u64, execs: u64, random: bool) -> FuzzConfig {
    FuzzConfig {
        seed,
        max_execs: execs,
        strategy: if random {
            Strategy::Random
        } else {
            Strategy::Guided
        },
        ..FuzzConfig::default()
    }
}

/// Draw one argument per carrier.
fn draw_args(carriers: &[Carrier], src: &mut Source) -> Result<Vec<Value>, Invalid> {
    carriers.iter().map(|c| c.draw(src)).collect()
}

/// Regenerate the arguments a tape encodes (strict replay).
fn args_of(carriers: &[Carrier], tape: &ChoiceTape) -> Result<Vec<Value>, String> {
    let mut src = Source::replay(tape);
    draw_args(carriers, &mut src).map_err(|e| format!("tape does not regenerate arguments: {e:?}"))
}

/// Native JIT outcome of `name(args)` through a synthesized zero-argument
/// wrapper compiled by the native batch backend.
/// `tail` is the target's signature after its parameter list (`-> T`, or
/// empty for an implicit `Unit`), taken from the source.
fn native_outcome(
    source: &str,
    target: &Target,
    tail: &str,
    args: &[Value],
) -> Result<Result<Value, String>, String> {
    let call = args.iter().map(literal).collect::<Vec<_>>().join(", ");
    let wrapped = format!(
        "{source}\nfn __vesper_fuzz_case() {tail} {{\n    {}({call})\n}}\n",
        target.name
    );
    let world = check_source(&wrapped)?;
    let mut batch = vesper::native::batch::NativeBatch::compile(
        &world,
        &["__vesper_fuzz_case"],
        vesper::native::batch::BatchLimits::default(),
    )?;
    Ok(batch.run("__vesper_fuzz_case"))
}

/// Machine value of a drawn argument (`i64` as its two's-complement bits).
fn machine_value(value: &Value) -> u64 {
    match value {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        Value::Bool(b) => u64::from(*b),
        _ => unreachable!("carriers draw only integers and booleans"),
    }
}

/// What one `--native-coverage` execution returned: the result's machine
/// value (when the result type is a supported scalar) or the raised
/// `Fail[..]` instance.
type NativeResult = Result<Option<u64>, String>;

/// One `--native-coverage` execution: run the instrumented native code,
/// feed its edges and comparison operands to Crucible, and cross-check the
/// outcome class and, for scalar results, the value against the evaluator.
/// Native fuel or stack exhaustion is an invalid run, like evaluator fuel.
fn native_coverage_run(
    coverage: &mut vesper::native_coverage::NativeCoverage,
    evaluator: &Evaluator<'_>,
    name: &str,
    args: &[Value],
    src: &mut Source,
    shadow: bool,
) -> Result<NativeResult, PropError> {
    use vesper::native_coverage::NativeOutcome;
    let machine: Vec<u64> = args.iter().map(machine_value).collect();
    let outcome = coverage
        .run(&machine)
        .map_err(|e| PropError::fail("native-run", e))?;
    if shadow {
        coverage.feed_shadow(src.probe());
    } else {
        coverage.feed(src.probe());
    }
    if matches!(outcome, NativeOutcome::Exhausted { .. }) {
        return Err(PropError::Invalid(Invalid::Rejected));
    }
    let reference = evaluator.call(name, args.to_vec());
    if let Err(e) = &reference
        && is_fuel(e)
    {
        return Err(PropError::Invalid(Invalid::Rejected));
    }
    match (outcome, reference) {
        (NativeOutcome::Returned, Ok(v)) => {
            let native = coverage.last_result();
            if let Some(n) = native
                && tape_value(&v).is_some()
                && machine_value(&v) != n
            {
                return Err(PropError::fail(
                    "native-divergence",
                    format!("{name}: native code returned {n:#x}, evaluator returned {v}"),
                ));
            }
            Ok(Ok(native))
        }
        (NativeOutcome::Failed { effect }, Err(_)) => Ok(Err(effect)),
        (NativeOutcome::Returned, Err(e)) => Err(PropError::fail(
            "native-divergence",
            format!("{name}: native code returned, evaluator failed: {e}"),
        )),
        (NativeOutcome::Failed { effect }, Ok(v)) => Err(PropError::fail(
            "native-divergence",
            format!("{name}: native code raised {effect}, evaluator returned {v}"),
        )),
        (NativeOutcome::Exhausted { .. }, _) => Err(PropError::Invalid(Invalid::Rejected)),
    }
}

/// One `verify --refute --native-coverage` evaluation of a `bool` contract:
/// the native result guides the search through `probe`, and the evaluator's
/// result decides (they must agree; a disagreement is a backend bug and
/// stops the campaign). A run that exhausts its native fuel or stack keeps
/// the evaluator's result and reports no native coverage.
fn native_contract(
    coverage: &mut vesper::native_coverage::NativeCoverage,
    evaluator: &Evaluator<'_>,
    name: &str,
    args: &[Value],
    probe: &mut vesper_crucible::Probe,
) -> bool {
    use vesper::native_coverage::NativeOutcome;
    let machine: Vec<u64> = args.iter().map(machine_value).collect();
    let outcome = match coverage.run(&machine) {
        Ok(o) => o,
        Err(e) => panic!("native contract run failed: {e}"),
    };
    let exhausted = matches!(outcome, NativeOutcome::Exhausted { .. });
    if !exhausted {
        coverage.feed(probe);
    }
    let reference = evaluator.call(name, args.to_vec());
    let holds = match &reference {
        Ok(Value::Bool(b)) => *b,
        Ok(other) => panic!("contract returned {other}"),
        Err(e) => panic!("contract evaluation failed: {e}"),
    };
    match outcome {
        NativeOutcome::Returned => {
            let native = coverage.last_result().map(|v| v != 0);
            if native.is_some_and(|n| n != holds) {
                panic!(
                    "native-divergence: native code says {name} is {}, evaluator says {holds}",
                    !holds
                );
            }
        }
        NativeOutcome::Failed { effect } => {
            panic!("native-divergence: native code raised {effect}, evaluator returned {holds}")
        }
        NativeOutcome::Exhausted { .. } => {}
    }
    holds
}

fn native_result_text(r: &NativeResult) -> String {
    match r {
        Ok(Some(v)) => format!("returned {v:#x}"),
        Ok(None) => "returned".into(),
        Err(effect) => format!("raised {effect}"),
    }
}

fn same_outcome(a: &Result<Value, String>, b: &Result<Value, String>) -> bool {
    match (a, b) {
        (Ok(x), Ok(y)) => x.to_string() == y.to_string(),
        (Err(x), Err(y)) => error_kind("", x) == error_kind("", y),
        _ => false,
    }
}

fn outcome_text(o: &Result<Value, String>) -> String {
    match o {
        Ok(v) => format!("returned {v}"),
        Err(e) => format!("failed: {e}"),
    }
}

/// Append regression functions to `out` (created from the input module
/// when it does not exist yet) and format the result canonically.
fn write_regressions(
    loaded: &Loaded,
    out: &Path,
    cases: &[(String, String, String)],
) -> Result<(), String> {
    let base = if out.exists() {
        fs::read_to_string(out).map_err(|e| format!("{}: {e}", out.display()))?
    } else {
        loaded.source.clone()
    };
    let mut text = base.trim_end().to_string();
    text.push('\n');
    let existing = check_source(&text).map_err(|e| {
        format!(
            "{}: regression file does not check before appending: {e}",
            out.display()
        )
    })?;
    let mut used: std::collections::BTreeSet<String> = existing.functions.keys().cloned().collect();
    for (stem, comment, body) in cases {
        let mut n = 1;
        let name = loop {
            let candidate = format!("{stem}_{n}");
            if !used.contains(&candidate) {
                break candidate;
            }
            n += 1;
        };
        used.insert(name.clone());
        text.push_str(&format!("\n// @test regression: {comment}\n"));
        text.push_str(&body.replace("{name}", &name));
    }
    let formatted = vesper_fmt::format_source(&text, vesper_fmt::Limits::default())
        .map_err(|e| format!("formatting regression cases: {e:?}"))?
        .output;
    check_source(&formatted).map_err(|e| format!("regression cases do not check: {e}"))?;
    fs::write(out, formatted).map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "wrote {} regression case(s) to {}",
        cases.len(),
        out.display()
    );
    Ok(())
}

fn tape_inline(tape: &ChoiceTape) -> String {
    tape.values
        .iter()
        .map(|v| format!("{v:#x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Silence the default panic hook while Crucible catches property panics.
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;
struct QuietPanics(Option<PanicHook>);
impl QuietPanics {
    fn new() -> Self {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        Self(Some(previous))
    }
}
impl Drop for QuietPanics {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            std::panic::set_hook(previous);
        }
    }
}

pub fn fuzz(args: Vec<String>) -> Result<(), String> {
    let mut common = Common::new();
    let mut native = false;
    let mut native_coverage = false;
    let mut differential = None;
    let mut corpus = None;
    let mut random = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{FUZZ_HELP}");
                return Ok(());
            }
            "--target" => common.target = Some(next(&mut it, arg)?.clone()),
            "--native" => native = true,
            "--native-coverage" => native_coverage = true,
            "--random" => random = true,
            "--differential" => differential = Some(next(&mut it, arg)?.clone()),
            "--corpus" => corpus = Some(PathBuf::from(next(&mut it, arg)?)),
            _ => {
                if !common.take(arg, &mut it)? {
                    return Err(format!(
                        "unknown fuzz option '{arg}'; see vesper fuzz --help"
                    ));
                }
            }
        }
    }
    if native_coverage && native {
        return Err("--native-coverage cannot be combined with --native".into());
    }
    let input = common
        .input
        .clone()
        .ok_or("missing source file; see vesper fuzz --help")?;
    let name = common
        .target
        .clone()
        .ok_or("fuzz requires --target <fn>; see vesper fuzz --help")?;
    let loaded = load(&input)?;
    let target = target(&loaded.world, &name, "fuzz target")?;
    let tail = signature_tail(&loaded.source, &loaded.world, &name)?;
    let shadow = match &differential {
        Some(shadow) => {
            let s = self::target(&loaded.world, shadow, "differential shadow")?;
            if s.carriers != target.carriers || s.result != target.result {
                return Err(format!(
                    "differential shadow '{shadow}' must have the same parameter and result types as '{name}'"
                ));
            }
            Some(s)
        }
        None => None,
    };
    if native {
        if target.effectful {
            return Err(format!(
                "--native supports effect-free targets; '{name}' declares effects (fuzz it in the evaluator)"
            ));
        }
        let zeros: Vec<Value> = target
            .carriers
            .iter()
            .map(|c| match c {
                Carrier::I64 => Value::Int(0),
                Carrier::U64 => Value::UInt(0, 64),
                Carrier::U32 => Value::UInt(0, 32),
                Carrier::U8 => Value::UInt(0, 8),
                Carrier::Bool => Value::Bool(false),
            })
            .collect();
        native_outcome(&loaded.source, &target, &tail, &zeros)
            .map_err(|e| format!("--native: '{name}' is outside the native batch subset: {e}"))?
            .map_err(|e| format!("--native: '{name}' failed natively on zero arguments: {e}"))?;
    }
    let compile_coverage = |target: &str| {
        let mut compiled = vesper::native_coverage::NativeCoverage::compile(
            &loaded.source,
            &loaded.world,
            target,
        )?;
        compiled.set_fuel(common.fuel as u64);
        let stats = compiled.stats();
        println!(
            "native coverage{}: {} function(s), {} edge counter(s), {} comparison site(s), fuel {} block(s)",
            if target == name {
                String::new()
            } else {
                format!(" (shadow {target})")
            },
            stats.functions,
            stats.edges,
            stats.comparisons,
            common.fuel
        );
        Ok::<_, String>(compiled)
    };
    let mut coverage = if native_coverage {
        Some(compile_coverage(&name)?)
    } else {
        None
    };
    // `--native-coverage --differential`: the shadow runs natively too.
    let mut shadow_coverage = match (&coverage, &shadow) {
        (Some(_), Some(s)) => Some(compile_coverage(&s.name)?),
        _ => None,
    };
    let execs = common.execs.unwrap_or(if native { 200 } else { 2000 });
    let evaluator = Evaluator::new(&loaded.world, common.fuel);
    let source = &loaded.source;
    let constants = literals(source);
    let mut property = |src: &mut Source| -> Result<(), PropError> {
        let args = draw_args(&target.carriers, src)?;
        if let Some(coverage) = coverage.as_mut() {
            let ours = native_coverage_run(coverage, &evaluator, &name, &args, src, false)?;
            let (Some(other), Some(s)) = (shadow_coverage.as_mut(), &shadow) else {
                return match ours {
                    Ok(_) => Ok(()),
                    Err(effect) => Err(PropError::fail(
                        format!("native {effect}"),
                        format!("{name} raised {effect}"),
                    )),
                };
            };
            let theirs = native_coverage_run(other, &evaluator, &s.name, &args, src, true)?;
            if ours != theirs {
                return Err(PropError::fail(
                    "divergence",
                    format!(
                        "{name} {}, shadow {} {} (both native)",
                        native_result_text(&ours),
                        s.name,
                        native_result_text(&theirs)
                    ),
                ));
            }
            return Ok(());
        }
        let outcome = evaluator.call(&name, args.clone());
        src.probe().edge(edge_of(&outcome));
        report_literals(src.probe(), &target.carriers, &args, &constants);
        if let Err(e) = &outcome
            && is_fuel(e)
        {
            return Err(PropError::Invalid(Invalid::Rejected));
        }
        if let Some(shadow) = &shadow {
            let other = evaluator.call(&shadow.name, args.clone());
            if let Err(e) = &other
                && is_fuel(e)
            {
                return Err(PropError::Invalid(Invalid::Rejected));
            }
            if !same_outcome(&outcome, &other) {
                return Err(PropError::fail(
                    "divergence",
                    format!(
                        "{name} {}, shadow {} {}",
                        outcome_text(&outcome),
                        shadow.name,
                        outcome_text(&other)
                    ),
                ));
            }
        } else if let Err(e) = &outcome {
            return Err(PropError::fail(error_kind("evaluator", e), e.clone()));
        }
        if native {
            let jit = native_outcome(source, &target, &tail, &args)
                .map_err(|e| PropError::fail("native-compile", e))?;
            if !same_outcome(&outcome, &jit) {
                return Err(PropError::fail(
                    "native-divergence",
                    format!(
                        "evaluator {}, native {}",
                        outcome_text(&outcome),
                        outcome_text(&jit)
                    ),
                ));
            }
        }
        Ok(())
    };
    let _quiet = QuietPanics::new();
    // Saved corpus tapes are regression inputs: replay them first.
    let mut found = None;
    let mut replayed = 0usize;
    if let Some(dir) = &corpus
        && dir.is_dir()
    {
        {
            let mut entries: Vec<_> = fs::read_dir(dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "tape"))
                .collect();
            entries.sort();
            for path in entries {
                let text =
                    fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let tape = ChoiceTape::from_text(&text)
                    .map_err(|e| format!("{}: {e:?}", path.display()))?;
                replayed += 1;
                if let Verdict::Fail(_) = vesper_crucible::replay(&mut property, &tape) {
                    let shrunk = vesper_crucible::shrink(
                        &mut property,
                        &tape,
                        &vesper_crucible::ShrinkConfig::default(),
                    )
                    .map_err(|e| {
                        format!("corpus tape {} stopped failing: {e:?}", path.display())
                    })?;
                    found = Some((
                        shrunk.tape,
                        shrunk.failure,
                        format!("corpus {}", path.display()),
                    ));
                    break;
                }
            }
        }
    }
    let mut report = None;
    if found.is_none() {
        let r = vesper_crucible::fuzz(&config(common.seed, execs, random), &mut property);
        if let Some(c) = &r.counterexample {
            found = Some((
                c.tape.clone(),
                c.failure.clone(),
                format!("execution {}", c.found_after_execs),
            ));
        }
        report = Some(r);
    }
    drop(_quiet);
    if let Some(r) = &report {
        println!(
            "fuzz {name}: {} execution(s), {} invalid, {} corpus entr{}, {} feature(s){}",
            r.execs,
            r.invalid_runs,
            r.corpus.len(),
            if r.corpus.len() == 1 { "y" } else { "ies" },
            r.features,
            if replayed > 0 {
                format!(", {replayed} corpus tape(s) replayed")
            } else {
                String::new()
            }
        );
        if let Some(dir) = &corpus {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            // The failing tape is saved too, so the next run replays it first.
            let failing = r.counterexample.as_ref().map(|c| &c.tape);
            for tape in r.corpus.iter().chain(failing) {
                let digest = blake3::hash(tape.to_text().as_bytes()).to_hex();
                let path = dir.join(format!("{}.tape", &digest[..16]));
                fs::write(&path, tape.to_text()).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }
    if let Some(s) = &shadow {
        println!(
            "differential against {}: isolation unavailable (both sides ran in {}); {}",
            s.name,
            if native_coverage {
                "one process as native code, cross-checked with the evaluator"
            } else {
                "the evaluator"
            },
            vesper_migrate::shadow::ShadowReport::<(), ()>::LIMITATION
        );
    }
    let Some((tape, failure, found_at)) = found else {
        println!("no failure found within budget");
        return Ok(());
    };
    let args = args_of(&target.carriers, &tape)?;
    let call = args.iter().map(literal).collect::<Vec<_>>().join(", ");
    println!("counterexample ({found_at}): {name}({call})");
    println!("failure [{}]: {}", failure.kind, failure.detail);
    println!("tape: {}", tape_inline(&tape));
    if let Some(out) = &common.regressions {
        let body = format!("fn {{name}}() {tail} {{\n    {name}({call})\n}}\n");
        write_regressions(
            &loaded,
            out,
            &[(
                format!("{name}_regression"),
                format!(
                    "vesper fuzz [{}] tape {}",
                    failure.kind.replace('\n', " "),
                    tape_inline(&tape)
                ),
                body,
            )],
        )?;
    }
    Err(format!("fuzz: counterexample found for '{name}'"))
}

pub fn refute(args: Vec<String>) -> Result<(), String> {
    let mut common = Common::new();
    let mut requires = None;
    let mut native_coverage = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{REFUTE_HELP}");
                return Ok(());
            }
            "--refute" => {}
            "--contract" | "--target" => common.target = Some(next(&mut it, arg)?.clone()),
            "--requires" => requires = Some(next(&mut it, arg)?.clone()),
            "--native-coverage" => native_coverage = true,
            _ => {
                if !common.take(arg, &mut it)? {
                    return Err(format!(
                        "unknown verify --refute option '{arg}'; see vesper verify --refute --help"
                    ));
                }
            }
        }
    }
    let input = common
        .input
        .clone()
        .ok_or("missing source file; see vesper verify --refute --help")?;
    let name = common
        .target
        .clone()
        .ok_or("verify --refute requires --contract <fn>")?;
    let loaded = load(&input)?;
    let contract = target(&loaded.world, &name, "contract")?;
    if contract.result != "bool" {
        return Err(format!(
            "contract '{name}' must return bool, not {}",
            contract.result
        ));
    }
    let precondition = match &requires {
        Some(r) => {
            let p = target(&loaded.world, r, "precondition")?;
            if p.carriers != contract.carriers || p.result != "bool" {
                return Err(format!(
                    "precondition '{r}' must take the contract's parameters and return bool"
                ));
            }
            Some(p)
        }
        None => None,
    };
    let execs = common.execs.unwrap_or(5000);
    let evaluator = Evaluator::new(&loaded.world, common.fuel);
    let constants = literals(&loaded.source);
    let coverage = if native_coverage {
        let mut compiled =
            vesper::native_coverage::NativeCoverage::compile(&loaded.source, &loaded.world, &name)?;
        compiled.set_fuel(common.fuel as u64);
        let stats = compiled.stats();
        println!(
            "native coverage: {} function(s), {} edge counter(s), {} comparison site(s), fuel {} block(s)",
            stats.functions, stats.edges, stats.comparisons, common.fuel
        );
        Some(std::cell::RefCell::new(compiled))
    } else {
        None
    };
    let quiet = QuietPanics::new();
    let refutation = vesper_crucible::refute(
        &config(common.seed, execs, false),
        |src: &mut Source| -> Result<Vec<Value>, Invalid> {
            let args = draw_args(&contract.carriers, src)?;
            if let Some(p) = &precondition {
                match evaluator.call(&p.name, args.clone()) {
                    Ok(Value::Bool(holds)) => src.assume(holds)?,
                    Ok(other) => panic!("precondition returned {other}"),
                    Err(e) if is_fuel(&e) => src.assume(false)?,
                    Err(e) => panic!("precondition failed: {e}"),
                }
            }
            Ok(args)
        },
        |args: &Vec<Value>, probe| {
            if let Some(coverage) = &coverage {
                return native_contract(&mut coverage.borrow_mut(), &evaluator, &name, args, probe);
            }
            let outcome = evaluator.call(&name, args.clone());
            probe.edge(edge_of(&outcome));
            report_literals(probe, &contract.carriers, args, &constants);
            match outcome {
                Ok(Value::Bool(b)) => b,
                Ok(other) => panic!("contract returned {other}"),
                Err(e) => panic!("contract evaluation failed: {e}"),
            }
        },
    );
    drop(quiet);
    match refutation {
        Refutation::Survived { execs, witnesses } => {
            println!(
                "refute {name}: no counterexample found within budget ({execs} execution(s), {} boundary witness(es)); continue with Lean proving",
                witnesses.len()
            );
            Ok(())
        }
        Refutation::Refuted {
            input,
            counterexample,
        } => {
            let call = input.iter().map(literal).collect::<Vec<_>>().join(", ");
            println!(
                "refute {name}: counterexample {name}({call}) is false (found after {} execution(s), shrunk in {} run(s))",
                counterexample.found_after_execs, counterexample.shrink.runs
            );
            println!("tape: {}", tape_inline(&counterexample.tape));
            if let Some(out) = &common.regressions {
                // Keep the contract's effect row: calling it needs it even
                // where this input does not raise.
                let tail = signature_tail(&loaded.source, &loaded.world, &name)?;
                let body = format!("fn {{name}}() {tail} {{\n    {name}({call})\n}}\n");
                write_regressions(
                    &loaded,
                    out,
                    &[(
                        format!("{name}_refuted"),
                        format!(
                            "vesper verify --refute: {name} is false here; tape {}",
                            tape_inline(&counterexample.tape)
                        ),
                        body,
                    )],
                )?;
            }
            Err(format!("verify --refute: contract '{name}' is refuted"))
        }
        Refutation::Crashed { counterexample } => Err(format!(
            "verify --refute: '{name}' could not be evaluated: [{}] {} (tape {})",
            counterexample.failure.kind,
            counterexample.failure.detail,
            tape_inline(&counterexample.tape)
        )),
        Refutation::Unreproducible {
            counterexample,
            error,
        } => Err(format!(
            "verify --refute: a counterexample to '{name}' did not reproduce ({error:?}); tape {}",
            tape_inline(&counterexample.tape)
        )),
    }
}
