//! `vesper sim-host`: the reference evaluator's external suspension API as a
//! long-lived process, for `crates/vesper-sim-session` (retirement R6 of
//! #1397, claim #2680). The session links no compiler: it starts this
//! command once per checked program and drives one machine per source task
//! over standard input and output, the way an editor drives `vesper lsp`.
//!
//! Every request and every reply is one JSON object on one line. The host
//! answers each request except `drop` with exactly one line:
//!
//! - `{"load":{"source":S}}`: parse and check `S`. Reply `{"loaded":W}`, the
//!   world facts the session validates scenarios against ([`world_facts`]),
//!   or `{"rejected":[D...]}`, every parse diagnostic (or else every check
//!   diagnostic) as `code: message`. A host loads one program.
//! - `{"begin":{"machine":N,"entry":F,"arguments":[V...],"fuel":U,"optimize":B}}`:
//!   start `F` on a fresh `Runtime` with the entry's linear effects at the
//!   host boundary (`Runtime::begin_external`). Reply with an execution.
//! - `{"write":{"machine":N,"index":I,"value":V}}`: write an `inout` argument
//!   of machine `N`'s pending operation. Reply `{"written":null}` or
//!   `{"error":E}`.
//! - `{"resume":{"machine":N,"value":V}}`: resume machine `N`'s pending
//!   operation with `V`. Reply with an execution.
//! - `{"drop":{"machine":N}}`: forget machine `N`. No reply.
//!
//! An execution is `{"complete":V}`, `{"suspended":{"effect":E,
//! "operation":O,"arguments":[V...]}}` or `{"error":E}`. Values use the
//! tagged form of [`value_json`]. End of input ends the host.
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use vesper::{
    ast::{Effects, Mode, Pass, Type},
    check::{self, World},
    parser,
    runtime::{Execution, Runtime, Suspension, Value},
};

/// Tags of the value wire form ([`value_json`]).
const TAG_INT: &str = "i";
const TAG_UINT: &str = "u";
const TAG_SINT: &str = "s";
const TAG_FLOAT: &str = "f";
const TAG_BOOL: &str = "b";
const TAG_STRING: &str = "str";
const TAG_TUPLE: &str = "t";
const TAG_DATA: &str = "d";
/// A value the session cannot rebuild (functions, sequences, wide
/// integers...), carried as its `Display` text for error messages.
const TAG_OPAQUE: &str = "o";

struct Host {
    world: Option<&'static World>,
    /// Each task's machine, held at its pending external operation.
    machines: HashMap<u64, Suspension<'static>>,
}

/// How long the host waits busy for the next request before it sleeps in
/// the kernel (the session's `SPIN` in crates/vesper-sim-session).
const SPIN: std::time::Duration = std::time::Duration::from_micros(200);

/// Serve standard input. A session connects a socket there, which is read
/// busy for up to [`SPIN`] between requests: every operation is one round
/// trip, and a sleeping process wakes on another core with cold caches.
pub fn serve_stdio() -> Result<(), String> {
    let output = std::io::BufWriter::new(std::io::stdout().lock());
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        use std::os::unix::net::UnixStream;
        let socket = std::io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .map(UnixStream::from)
            .ok()
            .filter(|s| s.local_addr().is_ok() && s.set_nonblocking(true).is_ok());
        if let Some(socket) = socket {
            return serve(std::io::BufReader::new(SpinRead(socket)), output);
        }
    }
    serve(std::io::stdin().lock(), output)
}

/// A non-blocking socket read that waits busy for up to [`SPIN`], then
/// blocks.
#[cfg(unix)]
struct SpinRead(std::os::unix::net::UnixStream);

#[cfg(unix)]
impl std::io::Read for SpinRead {
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

/// Serve requests from `input` until it ends.
pub fn serve(input: impl BufRead, mut output: impl Write) -> Result<(), String> {
    let mut host = Host {
        world: None,
        machines: HashMap::new(),
    };
    for line in input.lines() {
        let line = line.map_err(|e| format!("sim-host input: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Json =
            serde_json::from_str(&line).map_err(|e| format!("sim-host request: {e}"))?;
        if let Some(reply) = host.handle(&request) {
            let mut text = reply.to_string();
            text.push('\n');
            output
                .write_all(text.as_bytes())
                .and_then(|()| output.flush())
                .map_err(|e| format!("sim-host output: {e}"))?;
        }
    }
    Ok(())
}

fn error(message: impl Into<String>) -> Json {
    json!({ "error": message.into() })
}

impl Host {
    fn handle(&mut self, request: &Json) -> Option<Json> {
        let Some((kind, body)) = request.as_object().and_then(|m| m.iter().next()) else {
            return Some(error("a request is an object with one member"));
        };
        Some(match kind.as_str() {
            "load" => self.load(body),
            "begin" => self.begin(body),
            "write" => self.write(body),
            "resume" => self.resume(body),
            "drop" => {
                if let Some(id) = body["machine"].as_u64() {
                    self.machines.remove(&id);
                }
                return None;
            }
            other => error(format!("unknown sim-host request {other}")),
        })
    }

    fn load(&mut self, body: &Json) -> Json {
        if self.world.is_some() {
            return error("a sim-host loads one program");
        }
        let Some(source) = body["source"].as_str() else {
            return error("load needs a source string");
        };
        let render =
            |d: &vesper::ast::Diagnostic| Json::String(format!("{}: {}", d.code, d.message));
        let parsed = parser::parse(source);
        if !parsed.diagnostics.is_empty() {
            return json!({ "rejected": parsed.diagnostics.iter().map(render).collect::<Vec<_>>() });
        }
        let checked = check::check(&parsed.module);
        if !checked.diagnostics.is_empty() {
            return json!({ "rejected": checked.diagnostics.iter().map(render).collect::<Vec<_>>() });
        }
        // Live suspensions borrow the world for the rest of the process.
        let world: &'static World = Box::leak(Box::new(checked.world));
        self.world = Some(world);
        json!({ "loaded": world_facts(world) })
    }

    fn begin(&mut self, body: &Json) -> Json {
        let Some(world) = self.world else {
            return error("begin before load");
        };
        let (Some(id), Some(entry), Some(arguments)) = (
            body["machine"].as_u64(),
            body["entry"].as_str(),
            body["arguments"].as_array(),
        ) else {
            return error("begin needs a machine, an entry and arguments");
        };
        let arguments = match arguments
            .iter()
            .map(json_value)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(arguments) => arguments,
            Err(e) => return error(e),
        };
        let mut runtime = Runtime::new(world);
        if let Some(fuel) = body["fuel"].as_u64() {
            runtime.fuel = fuel;
        }
        if let Some(optimize) = body["optimize"].as_bool() {
            runtime.optimize = optimize;
        }
        self.machines.remove(&id);
        let execution = runtime.begin_external(entry, arguments);
        self.execution(id, execution)
    }

    fn write(&mut self, body: &Json) -> Json {
        let Some(suspension) = body["machine"]
            .as_u64()
            .and_then(|id| self.machines.get_mut(&id))
        else {
            return error("no suspended machine to write to");
        };
        let Some(index) = body["index"].as_u64().and_then(|i| usize::try_from(i).ok()) else {
            return error("write needs an argument index");
        };
        match json_value(&body["value"]).and_then(|v| suspension.write_argument(index, v)) {
            Ok(()) => json!({ "written": null }),
            Err(e) => error(e),
        }
    }

    fn resume(&mut self, body: &Json) -> Json {
        let Some(id) = body["machine"].as_u64() else {
            return error("resume needs a machine");
        };
        let value = match json_value(&body["value"]) {
            Ok(value) => value,
            Err(e) => return error(e),
        };
        let Some(suspension) = self.machines.remove(&id) else {
            return error("no suspended machine to resume");
        };
        let execution = suspension.resume(value);
        self.execution(id, execution)
    }

    fn execution(&mut self, id: u64, execution: Result<Execution<'static>, String>) -> Json {
        match execution {
            Err(e) => error(e),
            Ok(Execution::Complete(value)) => json!({ "complete": value_json(&value) }),
            Ok(Execution::Suspended(suspension)) => {
                let request = suspension.request();
                let reply = json!({ "suspended": {
                    "effect": request.effect.name,
                    "operation": request.operation,
                    "arguments": request.arguments.iter().map(value_json).collect::<Vec<_>>(),
                }});
                self.machines.insert(id, suspension);
                reply
            }
        }
    }
}

fn pass_name(pass: Pass) -> &'static str {
    match pass {
        Pass::In => "in",
        Pass::Inout => "inout",
        Pass::Sink => "sink",
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Copy => "copy",
        Mode::Affine => "affine",
        Mode::Linear => "linear",
    }
}

/// A checked type: `{"named":[N,[T...]]}`, `{"tuple":[T...]}`,
/// `{"function":{"params":[[P,T]...],"result":T}}`, or `{"other":D}` with
/// its `Display` text for the rest (literal, row and inferred types).
fn type_json(ty: &Type) -> Json {
    match ty {
        Type::Named(name, args) => {
            json!({ "named": [name, args.iter().map(type_json).collect::<Vec<_>>()] })
        }
        Type::Tuple(items) => json!({ "tuple": items.iter().map(type_json).collect::<Vec<_>>() }),
        Type::Function { params, result, .. } => json!({ "function": {
            "params": params.iter().map(|(p, t)| json!([pass_name(*p), type_json(t)])).collect::<Vec<_>>(),
            "result": type_json(result),
        }}),
        other => json!({ "other": other.to_string() }),
    }
}

fn effects_json(effects: &Effects) -> Json {
    json!({
        "instances": effects.instances.iter().map(|e| json!({
            "label": e.label,
            "name": e.name,
            "args": e.args.iter().map(type_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "tail": effects.tail,
    })
}

/// What a session reads of the checked world: every ADT, every effect
/// declaration, and every function with a body, with its checked result.
pub fn world_facts(world: &World) -> Json {
    let params = |params: &[vesper::ast::Param]| -> Vec<Json> {
        params
            .iter()
            .map(|p| json!({ "name": p.name, "pass": pass_name(p.pass), "ty": type_json(&p.ty) }))
            .collect()
    };
    let generics = |g: &[vesper::ast::Generic]| -> Vec<Json> {
        g.iter().map(|g| Json::String(g.name.clone())).collect()
    };
    let adts: Map<String, Json> = world
        .adts
        .iter()
        .map(|(name, adt)| {
            (
                name.clone(),
                json!({
                    "mode": mode_name(adt.mode),
                    "generics": generics(&adt.generics),
                    "is_enum": adt.is_enum,
                    "variants": adt.variants.iter().map(|v| json!({
                        "name": v.name,
                        "named": v.named,
                        "fields": v.fields.iter().map(|f| json!({ "name": f.name, "ty": type_json(&f.ty) })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                }),
            )
        })
        .collect();
    let effects: Map<String, Json> = world
        .effects
        .iter()
        .map(|(name, effect)| {
            (
                name.clone(),
                json!({
                    "linear": effect.linear,
                    "generics": generics(&effect.generics),
                    "ops": effect.ops.iter().map(|op| json!({
                        "name": op.name,
                        "params": params(&op.params),
                        "result": type_json(&op.result),
                    })).collect::<Vec<_>>(),
                }),
            )
        })
        .collect();
    let functions: Map<String, Json> = world
        .functions
        .iter()
        .filter(|(_, f)| f.body.is_some())
        .map(|(name, f)| {
            let result = world.function_results.get(name).unwrap_or(&f.result);
            (
                name.clone(),
                json!({
                    "generics": generics(&f.generics),
                    "params": params(&f.params),
                    "result": type_json(result),
                    "effects": effects_json(&f.effects),
                }),
            )
        })
        .collect();
    json!({ "adts": adts, "effects": effects, "functions": functions })
}

/// A runtime value in the tagged wire form: `{"i":N}`, `{"u":[N,BITS]}`,
/// `{"s":[N,BITS]}`, `{"f":BITS}` (the `f64` bit pattern), `{"b":B}`,
/// `{"str":S}`, `{"t":[V...]}`, `{"d":[TYPE,CONSTRUCTOR,{FIELD:V...},TRACKED]}`
/// or `{"o":DISPLAY}`.
pub fn value_json(value: &Value) -> Json {
    match value {
        Value::Int(n) => json!({ TAG_INT: n }),
        Value::UInt(n, bits) => json!({ TAG_UINT: [n, bits] }),
        Value::SInt(n, bits) => json!({ TAG_SINT: [n, bits] }),
        Value::Float(f) => json!({ TAG_FLOAT: f.to_bits() }),
        Value::Bool(b) => json!({ TAG_BOOL: b }),
        Value::String(s) => json!({ TAG_STRING: s }),
        Value::Tuple(items) => {
            json!({ TAG_TUPLE: items.iter().map(value_json).collect::<Vec<_>>() })
        }
        Value::Data {
            ty,
            constructor,
            fields,
            resource_identity,
        } => {
            let fields: Map<String, Json> = fields
                .iter()
                .map(|(k, v)| (k.clone(), value_json(v)))
                .collect();
            json!({ TAG_DATA: [ty, constructor, fields, resource_identity.is_some()] })
        }
        other => json!({ TAG_OPAQUE: other.to_string() }),
    }
}

/// A value the session sent, in [`value_json`]'s form. Opaque and tracked
/// values never come back.
fn json_value(json: &Json) -> Result<Value, String> {
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
        TAG_INT => Value::Int(body.as_i64().ok_or_else(bad)?),
        TAG_UINT => {
            let (n, bits) = pair()?;
            Value::UInt(n.as_u64().ok_or_else(bad)?, bits)
        }
        TAG_SINT => {
            let (n, bits) = pair()?;
            Value::SInt(n.as_i64().ok_or_else(bad)?, bits)
        }
        TAG_FLOAT => Value::Float(f64::from_bits(body.as_u64().ok_or_else(bad)?)),
        TAG_BOOL => Value::Bool(body.as_bool().ok_or_else(bad)?),
        TAG_STRING => Value::String(body.as_str().ok_or_else(bad)?.to_owned()),
        TAG_TUPLE => Value::Tuple(
            body.as_array()
                .ok_or_else(bad)?
                .iter()
                .map(json_value)
                .collect::<Result<_, _>>()?,
        ),
        TAG_DATA => {
            let items = body.as_array().filter(|a| a.len() == 4).ok_or_else(bad)?;
            if items[3].as_bool() != Some(false) {
                return Err("a tracked resource cannot cross the sim-host boundary".into());
            }
            let fields = items[2]
                .as_object()
                .ok_or_else(bad)?
                .iter()
                .map(|(k, v)| Ok((k.clone(), json_value(v)?)))
                .collect::<Result<BTreeMap<_, _>, String>>()?;
            Value::Data {
                ty: items[0].as_str().ok_or_else(bad)?.to_owned(),
                constructor: items[1].as_str().ok_or_else(bad)?.to_owned(),
                fields,
                resource_identity: None,
            }
        }
        _ => return Err(bad()),
    })
}

#[cfg(test)]
mod tests {
    use super::serve;
    use serde_json::{Value as Json, json};

    fn replies(requests: &[Json]) -> Vec<Json> {
        let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
        let mut output = vec![];
        serve(input.as_bytes(), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn a_machine_suspends_takes_a_write_and_resumes_to_completion() {
        let source = "linear effect Net { fn fill(inout slot: u64) -> bool; }
fn main() -> u64 eff [Net] { let mut slot: u64 = 1; let ok = Net::fill(inout slot); if ok { slot } else { 0 } }";
        let out = replies(&[
            json!({ "load": { "source": source } }),
            json!({ "begin": { "machine": 3, "entry": "main", "arguments": [], "fuel": 100000, "optimize": true } }),
            json!({ "write": { "machine": 3, "index": 0, "value": { "u": [41, 64] } } }),
            json!({ "resume": { "machine": 3, "value": { "b": true } } }),
            json!({ "drop": { "machine": 3 } }),
            json!({ "resume": { "machine": 3, "value": { "b": true } } }),
        ]);
        assert!(
            out[0]["loaded"]["effects"]["Net"]["linear"]
                .as_bool()
                .unwrap(),
            "{}",
            out[0]
        );
        assert_eq!(
            out[1],
            json!({ "suspended": { "effect": "Net", "operation": "fill", "arguments": [{ "u": [1, 64] }] } })
        );
        assert_eq!(out[2], json!({ "written": null }));
        assert_eq!(out[3], json!({ "complete": { "u": [41, 64] } }));
        // `drop` has no reply, and a finished machine cannot resume.
        assert_eq!(out.len(), 5);
        assert!(out[4]["error"].is_string());
    }

    #[test]
    fn rejected_sources_report_their_diagnostics() {
        let out = replies(&[json!({ "load": { "source": "fn main() -> u64 { nope }" } })]);
        let diagnostics = out[0]["rejected"].as_array().unwrap();
        assert!(
            diagnostics[0].as_str().unwrap().starts_with('E'),
            "{}",
            out[0]
        );
    }
}
