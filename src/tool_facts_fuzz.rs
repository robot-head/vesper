//! `vesper tool-facts fuzz-serve --source FILE --fuel STEPS`: the compiler
//! half of `vesper fuzz` and `vesper verify --refute` (#2670, part of #1397).
//!
//! A campaign runs its target thousands of times, and each next input
//! depends on the coverage of the last, so the tool keeps one compiler
//! process for the whole campaign instead of one per input. The compiler
//! checks the module once, then answers one JSON request per stdin line with
//! one JSON response per stdout line until stdin closes:
//!
//! - `target {name, role}`: the parameter carriers, result type and
//!   whether the function declares effects, or why it cannot be a target.
//! - `tail {name}`: the signature text after the parameter list.
//! - `eval {name, args}`: one evaluator call, fuel reset first.
//! - `native {name, tail, args}`: one call through the native batch backend
//!   (a zero-argument wrapper, compiled for this input).
//! - `coverage {name}` and `coverage_run {id, args, shadow}`: compile a
//!   target with native coverage instrumentation once, then run it and
//!   report what its feed would give Crucible's probe.
//! - `check {text}`: whether a module checks, and its function names (for
//!   the regression file).
//!
//! The tool side is crates/vesper-tools/src/fuzz.rs. Arguments travel as
//! `{carrier, value}` machine values; results as their display text plus,
//! for integers and booleans, their machine value. The evaluator writes
//! nothing to stdout, so the stream is the protocol's alone.
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::{
    borrow::Cow,
    io::{BufRead, Write},
};
use vesper::{
    ast::{Function, Pass},
    check::{self, World},
    native::batch::{BatchLimits, NativeBatch},
    native_coverage::{FeedEvent, NativeCoverage, NativeOutcome},
    parser,
    runtime::{Runtime, Value},
};

/// The kind `vesper fuzz` and `vesper verify --refute` start.
pub const KIND: &str = "fuzz-serve";

/// The parameter types a target may take, by name.
const CARRIERS: [&str; 5] = ["i64", "u64", "u32", "u8", "bool"];

/// The wrapper the `native` request compiles around one call.
const CASE: &str = "__vesper_fuzz_case";

/// The bits of each unsigned carrier.
const U64_BITS: u32 = 64;
const U32_BITS: u32 = 32;
const U8_BITS: u32 = 8;

pub fn serve(args: Vec<String>) -> Result<(), String> {
    let mut it = args.into_iter();
    let mut source_path = None;
    let mut fuel = None;
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} requires a value"));
        match arg.as_str() {
            "--source" => source_path = Some(std::path::PathBuf::from(value()?)),
            "--fuel" => {
                fuel = Some(
                    value()?
                        .parse::<u64>()
                        .map_err(|e| format!("--fuel: {e}"))?,
                )
            }
            _ => return Err(format!("unknown {KIND} option '{arg}'")),
        }
    }
    let source_path = source_path.ok_or_else(|| format!("{KIND} requires --source <path>"))?;
    let fuel = fuel.ok_or_else(|| format!("{KIND} requires --fuel <steps>"))?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let loaded = std::fs::read_to_string(&source_path)
        .map_err(|e| e.to_string())
        .and_then(|source| check_source(&source).map(|world| (source, world)));
    let (source, world) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => return respond(&mut out, &json!({ "error": error })),
    };
    respond(&mut out, &json!({ "ready": true }))?;
    let mut session = Session {
        source,
        world: &world,
        runtime: Runtime::new(&world),
        fuel,
        coverage: Vec::new(),
    };
    #[cfg(unix)]
    let requests = std::io::BufReader::new(
        SpinRead::new(std::os::fd::AsRawFd::as_raw_fd(&std::io::stdin()))
            .map_err(|e| e.to_string())?,
    );
    #[cfg(not(unix))]
    let requests = std::io::stdin().lock();
    for line in requests.lines() {
        let line = line.map_err(|e| e.to_string())?;
        let request: Request<'_> =
            serde_json::from_str(&line).map_err(|e| format!("{KIND}: {e}"))?;
        let response =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.answer(&request)))
                .unwrap_or_else(|panic| {
                    let message = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                        .unwrap_or_default();
                    Ok(Answer::Json(json!({ "panic": message })))
                })
                .unwrap_or_else(|error| Answer::Json(json!({ "error": error })));
        respond(&mut out, &response)?;
    }
    Ok(())
}

fn respond(out: &mut impl Write, response: &impl Serialize) -> Result<(), String> {
    serde_json::to_writer(&mut *out, response).map_err(|e| e.to_string())?;
    out.write_all(b"\n").map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

/// `{count} syntax error(s); first: {code} {message}`, as the tools report
/// a module that does not check.
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

struct Session<'a> {
    source: String,
    world: &'a World,
    runtime: Runtime<'a>,
    fuel: u64,
    coverage: Vec<NativeCoverage>,
}

/// One request line. Every request names its `op`; the other fields are the
/// ones that op reads. Parsed straight into this shape (not a JSON tree), as
/// a campaign sends thousands.
#[derive(Deserialize)]
struct Request<'a> {
    #[serde(borrow)]
    op: Cow<'a, str>,
    #[serde(borrow, default)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    role: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    tail: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    text: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    args: Vec<Arg<'a>>,
    #[serde(default)]
    id: Option<usize>,
    #[serde(default)]
    shadow: bool,
}

/// An argument: `{carrier, value}` for the evaluator and the native batch,
/// a bare machine value for a coverage run.
#[derive(Deserialize)]
#[serde(untagged)]
enum Arg<'a> {
    Machine(u64),
    Wire {
        #[serde(borrow)]
        carrier: Cow<'a, str>,
        value: u64,
    },
}

impl Request<'_> {
    fn field<'r>(&'r self, value: &'r Option<Cow<'_, str>>, name: &str) -> Result<&'r str, String> {
        value
            .as_deref()
            .ok_or_else(|| format!("{KIND} request without '{name}'"))
    }
    fn name(&self) -> Result<&str, String> {
        self.field(&self.name, "name")
    }
    /// The evaluator values of `{carrier, value}` arguments.
    fn values(&self) -> Result<Vec<Value>, String> {
        self.args
            .iter()
            .map(|arg| {
                let Arg::Wire { carrier, value } = arg else {
                    return Err(format!("{KIND}: argument without a carrier"));
                };
                Ok(match carrier.as_ref() {
                    "i64" => Value::Int(*value as i64),
                    "u64" => Value::UInt(*value, U64_BITS),
                    "u32" => Value::UInt(*value, U32_BITS),
                    "u8" => Value::UInt(*value, U8_BITS),
                    "bool" => Value::Bool(*value != 0),
                    other => return Err(format!("{KIND}: unknown carrier '{other}'")),
                })
            })
            .collect()
    }
    /// Bare machine-value arguments.
    fn machine(&self) -> Result<Vec<u64>, String> {
        self.args
            .iter()
            .map(|arg| match arg {
                Arg::Machine(value) => Ok(*value),
                Arg::Wire { .. } => Err(format!("{KIND}: coverage arguments are machine values")),
            })
            .collect()
    }
}

/// Source literal of an argument.
fn literal(value: &Value) -> String {
    match value {
        Value::Int(n) => n.to_string(),
        Value::UInt(n, _) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// A call's outcome: `{ok, machine?, bool?}` or `{err}`.
#[derive(Serialize, Default)]
struct Call {
    #[serde(skip_serializing_if = "Option::is_none")]
    ok: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    machine: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bool: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    err: Option<String>,
}

fn outcome(result: Result<Value, String>) -> Call {
    match result {
        Ok(value) => {
            let (machine, boolean) = match value {
                Value::Int(n) => (Some(n as u64), None),
                Value::UInt(n, _) => (Some(n), None),
                Value::Bool(b) => (Some(u64::from(b)), Some(b)),
                _ => (None, None),
            };
            Call {
                ok: Some(value.to_string()),
                machine,
                bool: boolean,
                err: None,
            }
        }
        Err(error) => Call {
            err: Some(error),
            ..Call::default()
        },
    }
}

/// One response line: a call's outcome, or any other JSON answer.
#[derive(Serialize)]
#[serde(untagged)]
enum Answer {
    Call(Call),
    Json(Json),
}

fn feed_json(events: &[FeedEvent]) -> Json {
    events
        .iter()
        .map(|event| match *event {
            FeedEvent::Edge { slot, hits } => json!([slot, hits]),
            FeedEvent::Cmp {
                site,
                kind,
                lhs,
                rhs,
            } => json!([site, format!("{kind:?}"), lhs, rhs]),
        })
        .collect()
}

impl Session<'_> {
    fn answer(&mut self, request: &Request<'_>) -> Result<Answer, String> {
        let json = match request.op.as_ref() {
            "eval" => {
                let args = request.values()?;
                self.runtime.fuel = self.fuel;
                return Ok(Answer::Call(outcome(
                    self.runtime.run(request.name()?, args),
                )));
            }
            "native" => {
                let args = request.values()?;
                let tail = request.field(&request.tail, "tail")?;
                return Ok(match self.native(request.name()?, tail, &args) {
                    Ok(result) => Answer::Call(outcome(result)),
                    Err(error) => Answer::Json(json!({ "compile_error": error })),
                });
            }
            "target" => self
                .target(request.name()?, request.field(&request.role, "role")?)
                .unwrap_or_else(|error| json!({ "error": error })),
            "tail" => match self.tail(request.name()?) {
                Ok(tail) => json!({ "tail": tail }),
                Err(error) => json!({ "error": error }),
            },
            "coverage" => match self.compile_coverage(request.name()?) {
                Ok(answer) => answer,
                Err(error) => json!({ "error": error }),
            },
            "coverage_run" => self.coverage_run(request)?,
            "check" => match check_source(request.field(&request.text, "text")?) {
                Ok(world) => json!({ "functions": world.functions.keys().collect::<Vec<_>>() }),
                Err(error) => json!({ "error": error }),
            },
            other => return Err(format!("{KIND}: unknown request '{other}'")),
        };
        Ok(Answer::Json(json))
    }

    /// A function's signature reduced to drawable carriers.
    fn target(&self, name: &str, role: &str) -> Result<Json, String> {
        let f: &Function = self
            .world
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
            if p.pass != Pass::In {
                return Err(format!(
                    "{role} '{name}' parameter '{}' is not passed by value",
                    p.name
                ));
            }
            let carrier = p.ty.name();
            if !CARRIERS.contains(&carrier) {
                return Err(format!(
                    "{role} '{name}' parameter '{}' has type {}; supported: i64, u64, u32, u8, bool",
                    p.name, carrier
                ));
            }
            carriers.push(carrier.to_owned());
        }
        if f.body.is_none() {
            return Err(format!("{role} '{name}' has no body"));
        }
        Ok(json!({
            "carriers": carriers,
            "result": f.result.name(),
            "effectful": !f.effects.instances.is_empty() || f.effects.tail.is_some(),
        }))
    }

    /// The signature text after the parameter list (`-> T eff [...]`), taken
    /// from the source so regression cases carry the target's effect row.
    fn tail(&self, name: &str) -> Result<String, String> {
        let f = self
            .world
            .functions
            .get(name)
            .ok_or("target is not a function of the module")?;
        let body = f.body.as_ref().ok_or("target has no body")?;
        let header = self
            .source
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

    /// Native JIT outcome of `name(args)` through a synthesized zero-argument
    /// wrapper compiled by the native batch backend. `tail` is the target's
    /// signature after its parameter list.
    fn native(
        &self,
        name: &str,
        tail: &str,
        args: &[Value],
    ) -> Result<Result<Value, String>, String> {
        let call = args.iter().map(literal).collect::<Vec<_>>().join(", ");
        let wrapped = format!(
            "{}\nfn {CASE}() {tail} {{\n    {name}({call})\n}}\n",
            self.source
        );
        let world = check_source(&wrapped)?;
        let mut batch = NativeBatch::compile(&world, &[CASE], BatchLimits::default())?;
        Ok(batch.run(CASE))
    }

    fn compile_coverage(&mut self, name: &str) -> Result<Json, String> {
        let mut compiled = NativeCoverage::compile(&self.source, self.world, name)?;
        compiled.set_fuel(self.fuel);
        let stats = compiled.stats();
        self.coverage.push(compiled);
        Ok(json!({
            "id": self.coverage.len() - 1,
            "functions": stats.functions,
            "edges": stats.edges,
            "comparisons": stats.comparisons,
        }))
    }

    fn coverage_run(&mut self, request: &Request<'_>) -> Result<Json, String> {
        let id = request
            .id
            .ok_or_else(|| format!("{KIND} request without 'id'"))?;
        let shadow = request.shadow;
        let machine = request.machine()?;
        let coverage = self
            .coverage
            .get_mut(id)
            .ok_or_else(|| format!("{KIND}: no coverage target {id}"))?;
        let outcome = match coverage.run(&machine) {
            Ok(outcome) => outcome,
            Err(error) => return Ok(json!({ "run_error": error })),
        };
        let mut answer = match outcome {
            NativeOutcome::Returned => json!({ "outcome": "returned" }),
            NativeOutcome::Failed { effect } => json!({ "outcome": "failed", "effect": effect }),
            NativeOutcome::Exhausted { stack } => json!({ "outcome": "exhausted", "stack": stack }),
        };
        if answer["outcome"] == "returned"
            && let Some(result) = coverage.last_result()
        {
            answer["result"] = json!(result);
        }
        answer["feed"] = feed_json(&coverage.feed_events(shadow));
        Ok(answer)
    }
}

/// How long a read of the request pipe spins before it sleeps. A campaign
/// sends its next request within microseconds of an answer, and waking a
/// sleeping reader costs tens of microseconds on some hosts, more than the
/// call itself (#2670), so each side polls the pipe briefly first.
#[cfg(unix)]
const SPIN: std::time::Duration = std::time::Duration::from_micros(200);

/// A pipe read that spins for [`SPIN`] before blocking in `poll`.
#[cfg(unix)]
struct SpinRead(std::os::fd::RawFd);

#[cfg(unix)]
impl SpinRead {
    /// Make `fd` non-blocking and read it through the spin.
    fn new(fd: std::os::fd::RawFd) -> std::io::Result<Self> {
        // SAFETY: `fd` is an open descriptor this process owns for its
        // lifetime (standard input).
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(fd))
    }
}

#[cfg(unix)]
impl std::io::Read for SpinRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let started = std::time::Instant::now();
        loop {
            // SAFETY: `buf` is valid for `buf.len()` bytes.
            let n = unsafe { libc::read(self.0, buf.as_mut_ptr().cast(), buf.len()) };
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
                        fd: self.0,
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
