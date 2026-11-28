//! The reference evaluator, driven over a process boundary.
//!
//! A [`CheckedProgram`](crate::CheckedProgram) starts one `vesper sim-host`
//! (`src/sim_host.rs` in the root package) and keeps it for its lifetime.
//! Each source task is one machine there: [`Runtime::begin_external`] starts
//! it, and the live continuation stays in the host behind the
//! [`Suspension`] token until it is resumed or dropped. Values cross in the
//! host's tagged wire form ([`Value`]).
use serde_json::{Map, Value as Json, json};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

/// How long a read waits for a reply busy before it sleeps in the kernel.
/// Every operation is one round trip, and a sleeping process wakes on
/// another core with cold caches; most replies come well within this.
const SPIN: std::time::Duration = std::time::Duration::from_micros(200);

/// The `vesper` executable to drive, overriding the search.
pub const COMPILER_ENV: &str = "VESPER_COMPILER";
/// The hidden command that serves sessions.
const HOST_COMMAND: &str = "sim-host";
/// The evaluator's default step limit (`Runtime::new`).
pub const DEFAULT_FUEL: u64 = 10_000_000;
/// The compiler the port harness built with this crate (build.rs), the last
/// place searched.
const BUILT_COMPILER: Option<&str> = option_env!("VESPER_SIM_BUILT_COMPILER");

/// Tags of the host's value wire form (`src/sim_host.rs`).
const TAG_INT: &str = "i";
const TAG_UINT: &str = "u";
const TAG_SINT: &str = "s";
const TAG_FLOAT: &str = "f";
const TAG_BOOL: &str = "b";
const TAG_STRING: &str = "str";
const TAG_TUPLE: &str = "t";
const TAG_DATA: &str = "d";
const TAG_OPAQUE: &str = "o";

/// An evaluator value. Values the session cannot build (functions,
/// sequences, wide integers, ...) arrive as [`Value::Opaque`] with their
/// evaluator `Display` text.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    UInt(u64, u32),
    /// A narrow signed integer (`i8`, `i16`, `i32`) and its width.
    SInt(i64, u32),
    Float(f64),
    Bool(bool),
    String(String),
    Tuple(Vec<Value>),
    Data {
        ty: String,
        constructor: String,
        fields: BTreeMap<String, Value>,
        /// The value carries an observed resource identity, which no JSON
        /// boundary may erase.
        tracked: bool,
    },
    Opaque(String),
}

fn join(items: impl Iterator<Item = String>) -> String {
    items.collect::<Vec<_>>().join(", ")
}

/// The evaluator's `Display` (`runtime::Value`).
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(v) | Self::SInt(v, _) => write!(f, "{v}"),
            Self::UInt(v, _) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::String(v) => write!(f, "{v:?}"),
            Self::Tuple(v) => write!(f, "({})", join(v.iter().map(ToString::to_string))),
            Self::Data {
                constructor,
                fields,
                ..
            } => {
                if fields.is_empty() {
                    write!(f, "{constructor}")
                } else if fields.keys().all(|k| k.parse::<usize>().is_ok()) {
                    let mut pairs = fields.iter().collect::<Vec<_>>();
                    pairs.sort_by_key(|(k, _)| k.parse::<usize>().unwrap_or(usize::MAX));
                    write!(
                        f,
                        "{constructor}({})",
                        join(pairs.iter().map(|(_, v)| v.to_string()))
                    )
                } else {
                    write!(
                        f,
                        "{constructor} {{ {} }}",
                        join(fields.iter().map(|(k, v)| format!("{k}: {v}")))
                    )
                }
            }
            Self::Opaque(text) => f.write_str(text),
        }
    }
}

impl Value {
    fn to_wire(&self) -> Json {
        match self {
            Self::Int(n) => json!({ TAG_INT: n }),
            Self::UInt(n, bits) => json!({ TAG_UINT: [n, bits] }),
            Self::SInt(n, bits) => json!({ TAG_SINT: [n, bits] }),
            Self::Float(v) => json!({ TAG_FLOAT: v.to_bits() }),
            Self::Bool(b) => json!({ TAG_BOOL: b }),
            Self::String(s) => json!({ TAG_STRING: s }),
            Self::Tuple(items) => {
                json!({ TAG_TUPLE: items.iter().map(Value::to_wire).collect::<Vec<_>>() })
            }
            Self::Data {
                ty,
                constructor,
                fields,
                tracked,
            } => {
                let fields: Map<String, Json> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_wire()))
                    .collect();
                json!({ TAG_DATA: [ty, constructor, fields, tracked] })
            }
            Self::Opaque(text) => json!({ TAG_OPAQUE: text }),
        }
    }

    fn from_wire(json: &Json) -> Result<Self, String> {
        let bad = || format!("malformed sim-host value {json}");
        let (tag, body) = json
            .as_object()
            .filter(|m| m.len() == 1)
            .and_then(|m| m.iter().next())
            .ok_or_else(bad)?;
        let pair = || -> Result<(&Json, u32), String> {
            let items = body.as_array().filter(|a| a.len() == 2).ok_or_else(bad)?;
            let bits = items[1]
                .as_u64()
                .and_then(|b| u32::try_from(b).ok())
                .ok_or_else(bad)?;
            Ok((&items[0], bits))
        };
        Ok(match tag.as_str() {
            TAG_INT => Self::Int(body.as_i64().ok_or_else(bad)?),
            TAG_UINT => {
                let (n, bits) = pair()?;
                Self::UInt(n.as_u64().ok_or_else(bad)?, bits)
            }
            TAG_SINT => {
                let (n, bits) = pair()?;
                Self::SInt(n.as_i64().ok_or_else(bad)?, bits)
            }
            TAG_FLOAT => Self::Float(f64::from_bits(body.as_u64().ok_or_else(bad)?)),
            TAG_BOOL => Self::Bool(body.as_bool().ok_or_else(bad)?),
            TAG_STRING => Self::String(body.as_str().ok_or_else(bad)?.to_owned()),
            TAG_TUPLE => Self::Tuple(
                body.as_array()
                    .ok_or_else(bad)?
                    .iter()
                    .map(Self::from_wire)
                    .collect::<Result<_, _>>()?,
            ),
            TAG_DATA => {
                let items = body.as_array().filter(|a| a.len() == 4).ok_or_else(bad)?;
                Self::Data {
                    ty: items[0].as_str().ok_or_else(bad)?.to_owned(),
                    constructor: items[1].as_str().ok_or_else(bad)?.to_owned(),
                    fields: items[2]
                        .as_object()
                        .ok_or_else(bad)?
                        .iter()
                        .map(|(k, v)| Ok((k.clone(), Self::from_wire(v)?)))
                        .collect::<Result<_, String>>()?,
                    tracked: items[3].as_bool().ok_or_else(bad)?,
                }
            }
            TAG_OPAQUE => Self::Opaque(body.as_str().ok_or_else(bad)?.to_owned()),
            _ => return Err(bad()),
        })
    }
}

/// One `vesper sim-host` process.
pub(crate) struct Host {
    child: Child,
    /// `None` once the host is told to stop.
    input: Option<BufWriter<Box<dyn Write + Send>>>,
    output: BufReader<Box<dyn Read + Send>>,
    line: String,
    /// The host stopped answering; every later call fails.
    broken: bool,
}

impl Host {
    pub(crate) fn start() -> Result<Self, String> {
        let compiler = compiler()?;
        let start =
            |e: std::io::Error| format!("cannot start {} {HOST_COMMAND}: {e}", compiler.display());
        let mut command = Command::new(compiler);
        command.arg(HOST_COMMAND).stderr(Stdio::inherit());
        let (child, input, output) = streams(&mut command).map_err(start)?;
        Ok(Self {
            child,
            input: Some(BufWriter::new(input)),
            output: BufReader::new(output),
            line: String::new(),
            broken: false,
        })
    }

    fn send(&mut self, request: &Json) -> Result<(), String> {
        let input = match &mut self.input {
            Some(input) if !self.broken => input,
            _ => return Err(format!("the {HOST_COMMAND} process has stopped")),
        };
        let mut text = request.to_string();
        text.push('\n');
        let sent = input
            .write_all(text.as_bytes())
            .and_then(|()| input.flush());
        sent.map_err(|e| {
            self.broken = true;
            format!("{HOST_COMMAND} request: {e}")
        })
    }

    /// Send `request` and read its one-line reply.
    pub(crate) fn call(&mut self, request: &Json) -> Result<Json, String> {
        self.send(request)?;
        self.line.clear();
        match self.output.read_line(&mut self.line) {
            Ok(0) | Err(_) => {
                self.broken = true;
                Err(format!(
                    "the {HOST_COMMAND} process stopped without a reply"
                ))
            }
            Ok(_) => serde_json::from_str(&self.line).map_err(|e| {
                self.broken = true;
                format!("{HOST_COMMAND} reply: {e}")
            }),
        }
    }
}

type Streams = (Child, Box<dyn Write + Send>, Box<dyn Read + Send>);

/// Spawn the host on two socket pairs, one each way, so that the reply end
/// can be read busy ([`SpinRead`]) while the request end stays blocking.
#[cfg(unix)]
fn streams(command: &mut Command) -> std::io::Result<Streams> {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    let (requests, host_input) = UnixStream::pair()?;
    let (replies, host_output) = UnixStream::pair()?;
    replies.set_nonblocking(true)?;
    let child = command
        .stdin(Stdio::from(OwnedFd::from(host_input)))
        .stdout(Stdio::from(OwnedFd::from(host_output)))
        .spawn()?;
    Ok((child, Box::new(requests), Box::new(SpinRead(replies))))
}

#[cfg(not(unix))]
fn streams(command: &mut Command) -> std::io::Result<Streams> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(std::io::Error::other("no standard streams"));
    };
    Ok((child, Box::new(input), Box::new(output)))
}

/// A non-blocking socket read that waits busy for up to [`SPIN`], then
/// blocks.
#[cfg(unix)]
struct SpinRead(std::os::unix::net::UnixStream);

#[cfg(unix)]
impl Read for SpinRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let started = std::time::Instant::now();
        loop {
            match self.0.read(buf) {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() < SPIN {
                        std::hint::spin_loop();
                        continue;
                    }
                    self.0.set_nonblocking(false)?;
                    let read = self.0.read(buf);
                    self.0.set_nonblocking(true)?;
                    return read;
                }
                read => return read,
            }
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // End of input ends the host.
        drop(self.input.take());
        let _ = self.child.wait();
    }
}

/// The machines of one program's host, with fresh ids.
pub(crate) struct Connection {
    /// Sessions on several threads may share a program: each call holds
    /// the host for its request and reply.
    host: Mutex<Host>,
    next: AtomicU64,
}

impl Connection {
    pub(crate) fn new(host: Host) -> Self {
        Self {
            host: Mutex::new(host),
            next: AtomicU64::new(0),
        }
    }
    pub(crate) fn call(&self, request: &Json) -> Result<Json, String> {
        self.host().call(request)
    }
    fn host(&self) -> MutexGuard<'_, Host> {
        // A call that panicked left the host marked broken or consistent.
        self.host.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn machine(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
    fn forget(&self, machine: u64) {
        // A broken host has nothing left to forget.
        let _ = self.host().send(&json!({ "drop": { "machine": machine } }));
    }
}

/// An unstarted machine.
pub struct Runtime<'w> {
    connection: &'w Connection,
    pub fuel: u64,
    pub optimize: bool,
}

/// An external operation a suspended machine is waiting on.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalRequest {
    pub effect: String,
    pub operation: String,
    pub arguments: Vec<Value>,
}

pub enum Execution<'w> {
    Complete(Value),
    Suspended(Suspension<'w>),
}

/// The unique token of a machine suspended at an external operation. Its
/// continuation lives in the host; resuming consumes the token, and
/// dropping it drops the continuation.
pub struct Suspension<'w> {
    connection: &'w Connection,
    machine: u64,
    request: ExternalRequest,
    live: bool,
}

impl<'w> Runtime<'w> {
    pub(crate) fn new(connection: &'w Connection) -> Self {
        Self {
            connection,
            fuel: DEFAULT_FUEL,
            optimize: true,
        }
    }

    /// Run `entry` with its linear effects at the host boundary until it
    /// completes or performs an external operation.
    pub fn begin_external(self, entry: &str, args: Vec<Value>) -> Result<Execution<'w>, String> {
        let machine = self.connection.machine();
        let reply = self.connection.call(&json!({ "begin": {
            "machine": machine,
            "entry": entry,
            "arguments": args.iter().map(Value::to_wire).collect::<Vec<_>>(),
            "fuel": self.fuel,
            "optimize": self.optimize,
        }}))?;
        execution(self.connection, machine, reply)
    }
}

fn execution<'w>(
    connection: &'w Connection,
    machine: u64,
    reply: Json,
) -> Result<Execution<'w>, String> {
    let bad = || format!("malformed {HOST_COMMAND} execution {reply}");
    let (kind, body) = reply
        .as_object()
        .filter(|m| m.len() == 1)
        .and_then(|m| m.iter().next())
        .ok_or_else(bad)?;
    match kind.as_str() {
        "error" => Err(body.as_str().ok_or_else(bad)?.to_owned()),
        "complete" => Ok(Execution::Complete(Value::from_wire(body)?)),
        "suspended" => {
            let (Some(effect), Some(operation), Some(arguments)) = (
                body["effect"].as_str(),
                body["operation"].as_str(),
                body["arguments"].as_array(),
            ) else {
                return Err(bad());
            };
            Ok(Execution::Suspended(Suspension {
                connection,
                machine,
                request: ExternalRequest {
                    effect: effect.to_owned(),
                    operation: operation.to_owned(),
                    arguments: arguments
                        .iter()
                        .map(Value::from_wire)
                        .collect::<Result<_, _>>()?,
                },
                live: true,
            }))
        }
        _ => Err(bad()),
    }
}

impl<'w> Suspension<'w> {
    pub fn request(&self) -> &ExternalRequest {
        &self.request
    }

    pub fn resume(mut self, value: Value) -> Result<Execution<'w>, String> {
        // The host consumes the continuation whatever the outcome.
        self.live = false;
        let reply = self.connection.call(&json!({ "resume": {
            "machine": self.machine,
            "value": value.to_wire(),
        }}))?;
        execution(self.connection, self.machine, reply)
    }

    /// The host implementation may update an inout argument before resuming.
    pub fn write_argument(&mut self, index: usize, value: Value) -> Result<(), String> {
        let reply = self.connection.call(&json!({ "write": {
            "machine": self.machine,
            "index": index,
            "value": value.to_wire(),
        }}))?;
        match reply.get("error").and_then(Json::as_str) {
            Some(message) => Err(message.to_owned()),
            None if reply.get("written").is_some() => Ok(()),
            None => Err(format!("malformed {HOST_COMMAND} reply {reply}")),
        }
    }
}

impl Drop for Suspension<'_> {
    fn drop(&mut self) {
        if self.live {
            self.connection.forget(self.machine);
        }
    }
}

fn executable(path: &Path) -> bool {
    path.is_file()
}

/// The `vesper` executable sessions drive: [`COMPILER_ENV`], else `vesper`
/// beside this executable, else the one the port harness built with this
/// crate, else `vesper` on `PATH`.
pub fn compiler() -> Result<&'static Path, String> {
    static FOUND: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let name = format!("vesper{}", std::env::consts::EXE_SUFFIX);
            if let Some(path) = std::env::var_os(COMPILER_ENV) {
                let path = PathBuf::from(path);
                return if executable(&path) {
                    Ok(path)
                } else {
                    Err(format!("{COMPILER_ENV}={} is not a file", path.display()))
                };
            }
            let beside = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join(&name)));
            let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).map(|d| d.join(&name)).collect())
                .unwrap_or_default();
            beside
                .into_iter()
                .chain(BUILT_COMPILER.map(PathBuf::from))
                .chain(path_dirs)
                .find(|p| executable(p))
                .ok_or_else(|| {
                    format!(
                        "cannot find the vesper compiler: set {COMPILER_ENV}, or put vesper beside this executable or on PATH"
                    )
                })
        })
        .as_ref()
        .map(PathBuf::as_path)
        .map_err(Clone::clone)
}
