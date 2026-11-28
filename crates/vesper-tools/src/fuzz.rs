//! `vesper fuzz` and `vesper verify --refute`: Vesper Crucible (§29.1) over
//! source functions (integration B, issue #136).
//!
//! A target is a checked source function whose parameters are `i64`, `u64`,
//! `u32`, `u8` or `bool`. Each Crucible execution draws one value per
//! parameter from the ChoiceTape and calls the function in the evaluator.
//! Coverage feedback is coarse: the outcome class and the result value are
//! reported as probe edges. With `--native-coverage` the target instead runs
//! as Cranelift code instrumented with edge counters and comparison-operand
//! logging (the compiler's `native_coverage`, lane 23), which feed Crucible's
//! corpus selection and input-to-state solver; the evaluator still runs every
//! input and must agree on success or failure.
//!
//! The checker, the evaluator and the native backends are the shipped
//! `vesper`'s: one `vesper tool-facts fuzz-serve` process per campaign answers
//! every call (#2670), so the tool does not link the compiler library.
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
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::{
    cell::RefCell,
    fmt, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};
use vesper_crucible::{
    ChoiceTape, CmpKind, FuzzConfig, Invalid, PropError, Refutation, Source, Strategy, Verdict,
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

/// The bits of each unsigned carrier.
const U64_BITS: u32 = 64;
const U32_BITS: u32 = 32;
const U8_BITS: u32 = 8;

impl Carrier {
    fn of(name: &str) -> Option<Self> {
        match name {
            "i64" => Some(Carrier::I64),
            "u64" => Some(Carrier::U64),
            "u32" => Some(Carrier::U32),
            "u8" => Some(Carrier::U8),
            "bool" => Some(Carrier::Bool),
            _ => None,
        }
    }
    /// The carrier's source type name, as the compiler names it.
    fn name(self) -> &'static str {
        match self {
            Carrier::I64 => "i64",
            Carrier::U64 => "u64",
            Carrier::U32 => "u32",
            Carrier::U8 => "u8",
            Carrier::Bool => "bool",
        }
    }
    fn draw(self, src: &mut Source) -> Result<Value, Invalid> {
        use vesper_crucible::Arbitrary;
        Ok(match self {
            Carrier::I64 => Value::Int(i64::arbitrary(src)?),
            Carrier::U64 => Value::UInt(src.draw_u64()?, U64_BITS),
            Carrier::U32 => Value::UInt(u64::from(src.draw_u32()?), U32_BITS),
            Carrier::U8 => Value::UInt(u64::from(src.draw_u8()?), U8_BITS),
            Carrier::Bool => Value::Bool(src.draw_bool()?),
        })
    }
}

/// A drawn argument: an integer or a boolean of one carrier.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Int(i64),
    UInt(u64, u32),
    Bool(bool),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::UInt(n, _) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
        }
    }
}

impl Value {
    fn carrier(&self) -> Carrier {
        match self {
            Value::Int(_) => Carrier::I64,
            Value::UInt(_, U64_BITS) => Carrier::U64,
            Value::UInt(_, U32_BITS) => Carrier::U32,
            Value::UInt(..) => Carrier::U8,
            Value::Bool(_) => Carrier::Bool,
        }
    }
    /// The argument as the compiler reads it: its carrier and machine value.
    fn wire(&self) -> WireArg {
        WireArg {
            carrier: self.carrier().name(),
            value: machine_value(self),
        }
    }
}

/// Source literal of a drawn argument.
fn literal(value: &Value) -> String {
    value.to_string()
}

/// What one evaluator or native call returned: the value's display text and,
/// for an integer or a boolean, its machine value.
#[derive(Clone, Debug)]
struct Returned {
    text: String,
    machine: Option<u64>,
    boolean: Option<bool>,
}

impl fmt::Display for Returned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// An argument as the compiler reads it (`Arg::Wire` in the root package's
/// `src/tool_facts_fuzz.rs`).
#[derive(Serialize)]
struct WireArg {
    carrier: &'static str,
    value: u64,
}

/// The `eval` request.
#[derive(Serialize)]
struct EvalRequest<'a> {
    op: &'static str,
    name: &'a str,
    args: Vec<WireArg>,
}

/// A call's answer (`Call` in the root package's `src/tool_facts_fuzz.rs`),
/// or the compiler's panic while making it.
#[derive(Deserialize)]
struct CallAnswer {
    ok: Option<String>,
    machine: Option<u64>,
    bool: Option<bool>,
    err: Option<String>,
    panic: Option<String>,
}

impl CallAnswer {
    fn outcome(self) -> Result<Returned, String> {
        if let Some(error) = self.err {
            return Err(error);
        }
        Ok(Returned {
            text: self.ok.unwrap_or_default(),
            machine: self.machine,
            boolean: self.bool,
        })
    }
}

/// A call's outcome as the compiler reported it (`outcome` in the root
/// package's `src/tool_facts_fuzz.rs`).
fn call_outcome(answer: &Json) -> Result<Returned, String> {
    if let Some(error) = answer.get("err") {
        return Err(error.as_str().unwrap_or_default().to_owned());
    }
    Ok(Returned {
        text: answer["ok"].as_str().unwrap_or_default().to_owned(),
        machine: answer["machine"].as_u64(),
        boolean: answer["bool"].as_bool(),
    })
}

/// The compiler service a campaign keeps running (`KIND` in the root
/// package's `src/tool_facts_fuzz.rs`).
const SERVE: &str = "fuzz-serve";

/// The shipped `vesper`, checking the module once and then answering every
/// call of the campaign over its standard streams.
struct Compiler {
    io: RefCell<Pipe>,
}

struct Pipe {
    child: Child,
    /// `None` once closed.
    stdin: Option<ChildStdin>,
    stdout: BufReader<SpinRead>,
    /// The last answer line, reused.
    line: String,
}

impl Compiler {
    /// Start the compiler on `path`; it checks the module first.
    fn start(path: &Path, fuel: usize) -> Result<Self, String> {
        let vesper = crate::compiler::locate()?;
        let mut child = Command::new(&vesper)
            .args([crate::compiler::FACTS_COMMAND, SERVE, "--source"])
            .arg(path)
            .args(["--fuel", &fuel.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", vesper.display()))?;
        let stdin = child.stdin.take().ok_or("compiler stdin")?;
        let stdout = BufReader::new(
            SpinRead::new(child.stdout.take().ok_or("compiler stdout")?)
                .map_err(|e| format!("{SERVE}: {e}"))?,
        );
        let compiler = Self {
            io: RefCell::new(Pipe {
                child,
                stdin: Some(stdin),
                stdout,
                line: String::new(),
            }),
        };
        let ready: Json = compiler.receive()?;
        if let Some(error) = ready.get("error") {
            return Err(error.as_str().unwrap_or_default().to_owned());
        }
        Ok(compiler)
    }

    fn receive<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        let mut io = self.io.borrow_mut();
        let io = &mut *io;
        io.line.clear();
        let read = io
            .stdout
            .read_line(&mut io.line)
            .map_err(|e| format!("{SERVE}: {e}"))?;
        if read == 0 {
            return Err(format!("{SERVE}: the vesper compiler stopped answering"));
        }
        serde_json::from_str(&io.line).map_err(|e| format!("{SERVE}: invalid answer: {e}"))
    }

    fn send(&self, request: &impl Serialize) -> Result<(), String> {
        let mut io = self.io.borrow_mut();
        let mut line = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        line.push(b'\n');
        let stdin = io.stdin.as_mut().ok_or("compiler stdin closed")?;
        stdin
            .write_all(&line)
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("{SERVE}: {e}"))
    }

    /// One request and its answer. A request the compiler could not read is
    /// an `error`; a panic in the compiler panics here with its message, as
    /// it did when the tool ran the compiler in process.
    fn ask(&self, request: Json) -> Result<Json, String> {
        self.send(&request)?;
        let answer: Json = self.receive()?;
        if let Some(message) = answer.get("panic") {
            panic!("{}", message.as_str().unwrap_or_default());
        }
        Ok(answer)
    }

    /// [`Self::ask`] inside a campaign, where the transport cannot fail
    /// without ending it.
    fn must(&self, request: Json) -> Json {
        self.ask(request).unwrap_or_else(|e| panic!("{e}"))
    }

    /// One evaluator call, the campaign's hot path: typed both ways rather
    /// than through JSON trees.
    fn eval(&self, name: &str, args: &[Value]) -> Result<Returned, String> {
        let args: Vec<WireArg> = args.iter().map(Value::wire).collect();
        let answer: CallAnswer = self
            .send(&EvalRequest {
                op: "eval",
                name,
                args,
            })
            .and_then(|()| self.receive())
            .unwrap_or_else(|e| panic!("{e}"));
        if let Some(message) = answer.panic {
            panic!("{message}");
        }
        answer.outcome()
    }

    /// Whether `text` checks, with its function names.
    fn check(&self, text: &str) -> Result<Vec<String>, String> {
        let answer = self.ask(json!({ "op": "check", "text": text }))?;
        if let Some(error) = answer.get("error") {
            return Err(error.as_str().unwrap_or_default().to_owned());
        }
        Ok(answer["functions"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .filter_map(|n| n.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl Drop for Compiler {
    fn drop(&mut self) {
        let io = self.io.get_mut();
        // Closing stdin ends the compiler's request loop.
        drop(io.stdin.take());
        let _ = io.child.wait();
    }
}

struct Loaded {
    source: String,
    compiler: Compiler,
}

fn load(path: &Path, fuel: usize) -> Result<Loaded, String> {
    let source = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let compiler = Compiler::start(path, fuel).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Loaded { source, compiler })
}

/// A function's signature reduced to drawable carriers.
struct Target {
    name: String,
    carriers: Vec<Carrier>,
    result: String,
    effectful: bool,
}

fn target(compiler: &Compiler, name: &str, role: &str) -> Result<Target, String> {
    let answer = compiler.ask(json!({ "op": "target", "name": name, "role": role }))?;
    if let Some(error) = answer.get("error") {
        return Err(error.as_str().unwrap_or_default().to_owned());
    }
    let carriers = answer["carriers"]
        .as_array()
        .ok_or("the compiler sent no carriers")?
        .iter()
        .map(|c| c.as_str().and_then(Carrier::of).ok_or("unknown carrier"))
        .collect::<Result<_, _>>()?;
    Ok(Target {
        name: name.to_string(),
        carriers,
        result: answer["result"].as_str().unwrap_or_default().to_owned(),
        effectful: answer["effectful"].as_bool().unwrap_or(false),
    })
}

/// The signature text after the parameter list (`-> T eff [...]`), taken
/// from the source so regression cases carry the target's effect row.
fn signature_tail(compiler: &Compiler, name: &str) -> Result<String, String> {
    let answer = compiler.ask(json!({ "op": "tail", "name": name }))?;
    if let Some(error) = answer.get("error") {
        return Err(error.as_str().unwrap_or_default().to_owned());
    }
    Ok(answer["tail"].as_str().unwrap_or_default().to_owned())
}

/// The campaign's evaluator: the compiler's, which checks and lowers the
/// module once and resets the fuel before every call.
struct Evaluator<'a> {
    compiler: &'a Compiler,
}

impl<'a> Evaluator<'a> {
    fn new(compiler: &'a Compiler) -> Self {
        Self { compiler }
    }
    fn call(&self, name: &str, args: Vec<Value>) -> Result<Returned, String> {
        self.compiler.eval(name, &args)
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

fn edge_of(outcome: &Result<Returned, String>) -> u32 {
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
/// wrapper the compiler builds with the native batch backend.
/// `tail` is the target's signature after its parameter list (`-> T`, or
/// empty for an implicit `Unit`), taken from the source.
fn native_outcome(
    compiler: &Compiler,
    target: &Target,
    tail: &str,
    args: &[Value],
) -> Result<Result<Returned, String>, String> {
    let args: Vec<WireArg> = args.iter().map(Value::wire).collect();
    let answer = compiler.ask(json!({
        "op": "native", "name": target.name, "tail": tail, "args": args,
    }))?;
    if let Some(error) = answer.get("compile_error") {
        return Err(error.as_str().unwrap_or_default().to_owned());
    }
    Ok(call_outcome(&answer))
}

/// Machine value of a drawn argument (`i64` as its two's-complement bits).
fn machine_value(value: &Value) -> u64 {
    match value {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        Value::Bool(b) => u64::from(*b),
    }
}

/// What one `--native-coverage` execution returned: the result's machine
/// value (when the result type is a supported scalar) or the raised
/// `Fail[..]` instance.
type NativeResult = Result<Option<u64>, String>;

/// How one native coverage run ended.
enum NativeOutcome {
    Returned,
    /// The target raised `Fail` instance `effect`.
    Failed {
        effect: String,
    },
    /// The run used up its fuel or stack budget; its result and coverage
    /// past that point are meaningless.
    Exhausted,
}

/// The comparison kinds a feed reports, matched by name.
const CMP_KINDS: [CmpKind; 6] = [
    CmpKind::Eq,
    CmpKind::Ne,
    CmpKind::Lt,
    CmpKind::Le,
    CmpKind::Gt,
    CmpKind::Ge,
];

/// The fields of one edge feed event (`[slot, hits]`) and one comparison
/// event (`[site, kind, lhs, rhs]`).
const EDGE_EVENT_FIELDS: usize = 2;
const CMP_EVENT_FIELDS: usize = 4;

/// One run of a target the compiler compiled with native coverage
/// instrumentation: how it ended, its scalar result and its feed.
struct CoverageRun {
    outcome: NativeOutcome,
    result: Option<u64>,
    feed: Json,
}

impl CoverageRun {
    /// Report the run's coverage to Crucible, as the compiler's
    /// `NativeCoverage::feed` would have in process.
    fn feed(&self, probe: &mut vesper_crucible::Probe) {
        let Some(events) = self.feed.as_array() else {
            return;
        };
        for event in events {
            let Some(fields) = event.as_array() else {
                continue;
            };
            let word = |i: usize| fields.get(i).and_then(Json::as_u64).unwrap_or_default();
            match fields.len() {
                EDGE_EVENT_FIELDS => {
                    for _ in 0..word(1) {
                        probe.edge(word(0) as u32);
                    }
                }
                CMP_EVENT_FIELDS => {
                    let name = fields[1].as_str().unwrap_or_default();
                    if let Some(kind) = CMP_KINDS.iter().find(|k| format!("{k:?}") == name) {
                        probe.cmp(word(0) as u32, *kind, word(2), word(3));
                    }
                }
                _ => {}
            }
        }
    }
}

/// A fuzz target compiled once with coverage instrumentation, in the
/// compiler process.
struct NativeCoverage<'a> {
    compiler: &'a Compiler,
    id: u64,
    functions: u64,
    edges: u64,
    comparisons: u64,
}

impl<'a> NativeCoverage<'a> {
    fn compile(compiler: &'a Compiler, target: &str) -> Result<Self, String> {
        let answer = compiler.ask(json!({ "op": "coverage", "name": target }))?;
        if let Some(error) = answer.get("error") {
            return Err(error.as_str().unwrap_or_default().to_owned());
        }
        let count = |field: &str| answer[field].as_u64().unwrap_or_default();
        Ok(Self {
            compiler,
            id: count("id"),
            functions: count("functions"),
            edges: count("edges"),
            comparisons: count("comparisons"),
        })
    }

    /// Run the target on machine-value `args`; `shadow` labels the feed as
    /// the second target of a differential campaign.
    fn run(&self, args: &[u64], shadow: bool) -> Result<CoverageRun, String> {
        let mut answer = self.compiler.must(json!({
            "op": "coverage_run", "id": self.id, "args": args, "shadow": shadow,
        }));
        if let Some(error) = answer.get("run_error") {
            return Err(error.as_str().unwrap_or_default().to_owned());
        }
        let outcome = match answer["outcome"].as_str() {
            Some("returned") => NativeOutcome::Returned,
            Some("failed") => NativeOutcome::Failed {
                effect: answer["effect"].as_str().unwrap_or_default().to_owned(),
            },
            _ => NativeOutcome::Exhausted,
        };
        Ok(CoverageRun {
            outcome,
            result: answer["result"].as_u64(),
            feed: answer["feed"].take(),
        })
    }
}

/// One `--native-coverage` execution: run the instrumented native code,
/// feed its edges and comparison operands to Crucible, and cross-check the
/// outcome class and, for scalar results, the value against the evaluator.
/// Returns the native result and the evaluator's outcome, which a
/// differential campaign compares for results native code does not report.
/// Native fuel or stack exhaustion is an invalid run, like evaluator fuel.
fn native_coverage_run(
    coverage: &NativeCoverage<'_>,
    evaluator: &Evaluator<'_>,
    name: &str,
    args: &[Value],
    src: &mut Source,
    shadow: bool,
) -> Result<(NativeResult, Result<Returned, String>), PropError> {
    let machine: Vec<u64> = args.iter().map(machine_value).collect();
    let run = coverage
        .run(&machine, shadow)
        .map_err(|e| PropError::fail("native-run", e))?;
    run.feed(src.probe());
    if matches!(run.outcome, NativeOutcome::Exhausted) {
        return Err(PropError::Invalid(Invalid::Rejected));
    }
    let reference = evaluator.call(name, args.to_vec());
    if let Err(e) = &reference
        && is_fuel(e)
    {
        return Err(PropError::Invalid(Invalid::Rejected));
    }
    match (run.outcome, reference) {
        (NativeOutcome::Returned, Ok(v)) => {
            let native = run.result;
            if let Some(n) = native
                && v.machine.is_some_and(|m| m != n)
            {
                return Err(PropError::fail(
                    "native-divergence",
                    format!("{name}: native code returned {n:#x}, evaluator returned {v}"),
                ));
            }
            Ok((Ok(native), Ok(v)))
        }
        (NativeOutcome::Failed { effect }, Err(e)) => Ok((Err(effect), Err(e))),
        (NativeOutcome::Returned, Err(e)) => Err(PropError::fail(
            "native-divergence",
            format!("{name}: native code returned, evaluator failed: {e}"),
        )),
        (NativeOutcome::Failed { effect }, Ok(v)) => Err(PropError::fail(
            "native-divergence",
            format!("{name}: native code raised {effect}, evaluator returned {v}"),
        )),
        (NativeOutcome::Exhausted, _) => Err(PropError::Invalid(Invalid::Rejected)),
    }
}

/// One `verify --refute --native-coverage` evaluation of a `bool` contract:
/// the native result guides the search through `probe`, and the evaluator's
/// result decides (they must agree; a disagreement is a backend bug and
/// stops the campaign). A run that exhausts its native fuel or stack keeps
/// the evaluator's result and reports no native coverage.
fn native_contract(
    coverage: &NativeCoverage<'_>,
    evaluator: &Evaluator<'_>,
    name: &str,
    args: &[Value],
    probe: &mut vesper_crucible::Probe,
) -> bool {
    let machine: Vec<u64> = args.iter().map(machine_value).collect();
    let run = match coverage.run(&machine, false) {
        Ok(run) => run,
        Err(e) => panic!("native contract run failed: {e}"),
    };
    let exhausted = matches!(run.outcome, NativeOutcome::Exhausted);
    if !exhausted {
        run.feed(probe);
    }
    let reference = evaluator.call(name, args.to_vec());
    let holds = match &reference {
        Ok(v) => match v.boolean {
            Some(b) => b,
            None => panic!("contract returned {v}"),
        },
        Err(e) => panic!("contract evaluation failed: {e}"),
    };
    match run.outcome {
        NativeOutcome::Returned => {
            let native = run.result.map(|v| v != 0);
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
        NativeOutcome::Exhausted => {}
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

fn same_outcome(a: &Result<Returned, String>, b: &Result<Returned, String>) -> bool {
    match (a, b) {
        (Ok(x), Ok(y)) => x.to_string() == y.to_string(),
        (Err(x), Err(y)) => error_kind("", x) == error_kind("", y),
        _ => false,
    }
}

fn outcome_text(o: &Result<Returned, String>) -> String {
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
    let existing = loaded.compiler.check(&text).map_err(|e| {
        format!(
            "{}: regression file does not check before appending: {e}",
            out.display()
        )
    })?;
    let mut used: std::collections::BTreeSet<String> = existing.into_iter().collect();
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
    loaded
        .compiler
        .check(&formatted)
        .map_err(|e| format!("regression cases do not check: {e}"))?;
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
    let loaded = load(&input, common.fuel)?;
    let target = target(&loaded.compiler, &name, "fuzz target")?;
    let tail = signature_tail(&loaded.compiler, &name)?;
    let shadow = match &differential {
        Some(shadow) => {
            let s = self::target(&loaded.compiler, shadow, "differential shadow")?;
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
                Carrier::U64 => Value::UInt(0, U64_BITS),
                Carrier::U32 => Value::UInt(0, U32_BITS),
                Carrier::U8 => Value::UInt(0, U8_BITS),
                Carrier::Bool => Value::Bool(false),
            })
            .collect();
        native_outcome(&loaded.compiler, &target, &tail, &zeros)
            .map_err(|e| format!("--native: '{name}' is outside the native batch subset: {e}"))?
            .map_err(|e| format!("--native: '{name}' failed natively on zero arguments: {e}"))?;
    }
    let compile_coverage = |target: &str| {
        let compiled = NativeCoverage::compile(&loaded.compiler, target)?;
        println!(
            "native coverage{}: {} function(s), {} edge counter(s), {} comparison site(s), fuel {} block(s)",
            if target == name {
                String::new()
            } else {
                format!(" (shadow {target})")
            },
            compiled.functions,
            compiled.edges,
            compiled.comparisons,
            common.fuel
        );
        Ok::<_, String>(compiled)
    };
    let coverage = if native_coverage {
        Some(compile_coverage(&name)?)
    } else {
        None
    };
    // `--native-coverage --differential`: the shadow runs natively too.
    let shadow_coverage = match (&coverage, &shadow) {
        (Some(_), Some(s)) => Some(compile_coverage(&s.name)?),
        _ => None,
    };
    let execs = common.execs.unwrap_or(if native { 200 } else { 2000 });
    let evaluator = Evaluator::new(&loaded.compiler);
    let source = &loaded.source;
    let constants = literals(source);
    let mut property = |src: &mut Source| -> Result<(), PropError> {
        let args = draw_args(&target.carriers, src)?;
        if let Some(coverage) = coverage.as_ref() {
            let (ours, our_value) =
                native_coverage_run(coverage, &evaluator, &name, &args, src, false)?;
            let (Some(other), Some(s)) = (shadow_coverage.as_ref(), &shadow) else {
                return match ours {
                    Ok(_) => Ok(()),
                    Err(effect) => Err(PropError::fail(
                        format!("native {effect}"),
                        format!("{name} raised {effect}"),
                    )),
                };
            };
            let (theirs, their_value) =
                native_coverage_run(other, &evaluator, &s.name, &args, src, true)?;
            // Native code reports scalar results only; the evaluator's
            // outcomes (already cross-checked against native code) decide
            // for floats, aggregates and Unit.
            if ours != theirs || !same_outcome(&our_value, &their_value) {
                return Err(PropError::fail(
                    "divergence",
                    format!(
                        "{name} {} ({}), shadow {} {} ({}) (both native)",
                        native_result_text(&ours),
                        outcome_text(&our_value),
                        s.name,
                        native_result_text(&theirs),
                        outcome_text(&their_value)
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
            let jit = native_outcome(&loaded.compiler, &target, &tail, &args)
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
    let loaded = load(&input, common.fuel)?;
    let contract = target(&loaded.compiler, &name, "contract")?;
    if contract.result != "bool" {
        return Err(format!(
            "contract '{name}' must return bool, not {}",
            contract.result
        ));
    }
    let precondition = match &requires {
        Some(r) => {
            let p = target(&loaded.compiler, r, "precondition")?;
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
    let evaluator = Evaluator::new(&loaded.compiler);
    let constants = literals(&loaded.source);
    let coverage = if native_coverage {
        let compiled = NativeCoverage::compile(&loaded.compiler, &name)?;
        println!(
            "native coverage: {} function(s), {} edge counter(s), {} comparison site(s), fuel {} block(s)",
            compiled.functions, compiled.edges, compiled.comparisons, common.fuel
        );
        Some(compiled)
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
                    Ok(Returned {
                        boolean: Some(holds),
                        ..
                    }) => src.assume(holds)?,
                    Ok(other) => panic!("precondition returned {other}"),
                    Err(e) if is_fuel(&e) => src.assume(false)?,
                    Err(e) => panic!("precondition failed: {e}"),
                }
            }
            Ok(args)
        },
        |args: &Vec<Value>, probe| {
            if let Some(coverage) = &coverage {
                return native_contract(coverage, &evaluator, &name, args, probe);
            }
            let outcome = evaluator.call(&name, args.clone());
            probe.edge(edge_of(&outcome));
            report_literals(probe, &contract.carriers, args, &constants);
            match outcome {
                Ok(Returned {
                    boolean: Some(b), ..
                }) => b,
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
                let tail = signature_tail(&loaded.compiler, &name)?;
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

/// How long a read of the request pipe spins before it sleeps. A campaign
/// answers its next request within microseconds of an answer, and waking a
/// sleeping reader costs tens of microseconds on some hosts, more than the
/// call itself (#2670), so each side polls the pipe briefly first.
#[cfg(unix)]
const SPIN: std::time::Duration = std::time::Duration::from_micros(200);

/// The compiler's answers: a pipe read that spins for [`SPIN`] before
/// blocking in `poll` (`SpinRead` in the root package's
/// `src/tool_facts_fuzz.rs`).
#[cfg(unix)]
struct SpinRead(ChildStdout);

#[cfg(unix)]
impl SpinRead {
    /// Make `fd` non-blocking and read it through the spin.
    fn new(pipe: ChildStdout) -> std::io::Result<Self> {
        let fd = std::os::fd::AsRawFd::as_raw_fd(&pipe);
        // SAFETY: `fd` is an open descriptor this process owns for its
        // lifetime (the child's stdout, held by the result).
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(pipe))
    }
}

#[cfg(unix)]
impl std::io::Read for SpinRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let started = std::time::Instant::now();
        loop {
            // SAFETY: `buf` is valid for `buf.len()` bytes.
            let n = unsafe {
                libc::read(
                    std::os::fd::AsRawFd::as_raw_fd(&self.0),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                )
            };
            if n >= 0 {
                return Ok(n as usize);
            }
            let error = std::io::Error::last_os_error();
            match error.kind() {
                std::io::ErrorKind::Interrupted => {}
                std::io::ErrorKind::WouldBlock if started.elapsed() < SPIN => {
                    std::hint::spin_loop();
                }
                std::io::ErrorKind::WouldBlock => {
                    let mut ready = libc::pollfd {
                        fd: std::os::fd::AsRawFd::as_raw_fd(&self.0),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: one valid pollfd; -1 waits without a timeout.
                    if unsafe { libc::poll(&mut ready, 1, -1) } < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                }
                _ => return Err(error),
            }
        }
    }
}

/// The compiler's answers where pipes cannot be polled: plain reads.
#[cfg(not(unix))]
struct SpinRead(ChildStdout);

#[cfg(not(unix))]
impl SpinRead {
    fn new(pipe: ChildStdout) -> std::io::Result<Self> {
        Ok(Self(pipe))
    }
}

#[cfg(not(unix))]
impl std::io::Read for SpinRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.0, buf)
    }
}
