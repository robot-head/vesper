//! Evaluator host for the built-in `Trace` effect.
//!
//! [`run_traced`] runs a checked program whose entry function carries
//! `eff [Trace]` on the reference evaluator. Each `Trace` operation that
//! reaches the host boundary is served through `vesper-beacon`: spans start in
//! a fixed-capacity [`SpanPool`], finish into a [`TelemetryRing`] and are
//! drained as columns into [`ExportedSpan`]s. Time is a deterministic logical
//! clock (one tick per host operation), so exported trees are reproducible.
//!
//! The host's current context is the most recently started span that is
//! still open, or the configured parent (for example a parsed `traceparent`
//! header), or the configured trace's root (`span_id` 0). Source handlers
//! that shadow `Trace` (a `with_parent` scope) answer their block's
//! operations themselves and reach the host only by forwarding.
use super::{ACTIVE_SPAN, SPAN_CONTEXT, TRACE};
use crate::check::Checked;
use crate::runtime::{Execution, Runtime, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use vesper_beacon::{
    ErrorCode, FinishError, FinishFailure, PrimaryAttribute, Secret, SpanPool, SpanSchema,
    SpanStatus, StartError, TelemetryRing, W3cSpanContext,
};

/// Spans that may be open at once.
pub const ACTIVE_SPANS: usize = 64;
/// Finished spans buffered before the host drains the ring.
pub const RING_CAPACITY: usize = 256;
/// Spans one run may export.
pub const MAX_EXPORTED: usize = 65_536;

/// How to run a traced program.
#[derive(Clone, Copy, Debug)]
pub struct TraceConfig {
    /// The trace id for spans without an imported parent.
    pub trace_id: u128,
    /// The first span id the pool hands out.
    pub first_span_id: u64,
    pub flags: u8,
    /// An imported parent context (for example from `traceparent`).
    pub parent: Option<W3cSpanContext>,
    /// Evaluator fuel.
    pub fuel: u64,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            trace_id: 0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736,
            first_span_id: 1,
            flags: 1,
            parent: None,
            fuel: 1_000_000,
        }
    }
}

/// One exported span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedSpan {
    pub trace_id: u128,
    pub span_id: u64,
    pub parent_span_id: u64,
    pub flags: u8,
    pub name: String,
    pub schema: SpanSchema,
    pub status: SpanStatus,
    /// The `emit_err` message.
    pub message: Option<String>,
    pub attribute: PrimaryAttribute,
    pub start_unix_ns: u64,
    pub duration_ns: u64,
}

/// A completed traced run.
#[derive(Debug)]
pub struct TraceRun {
    pub value: Value,
    /// Exported spans, in finish order.
    pub spans: Vec<ExportedSpan>,
}

impl TraceRun {
    /// The span forest rendered one span per line, children indented two
    /// spaces under their parent and ordered by span id (start order).
    pub fn tree(&self) -> String {
        render_tree(&self.spans)
    }
}

/// Typed failures of a traced run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceRunError {
    /// The program did not check.
    NotChecked(usize),
    /// The World does not use the built-in Beacon prelude.
    NotBeacon,
    /// The entry is missing or its row holds another effect.
    Entry(String),
    /// The evaluator failed.
    Runtime(String),
    /// An effect other than `Trace` reached the host.
    UnsupportedEffect(String),
    /// A schema id outside the Beacon vocabulary (0 request, 1 storage
    /// append, 2 background).
    UnknownSchema(u64),
    /// An error code outside the Beacon vocabulary (0 invalid input,
    /// 1 storage, 2 timeout, 3 cancelled).
    UnknownErrorCode(u64),
    /// A context that is neither a valid W3C context nor this trace's root.
    InvalidContext,
    /// A span value that the host did not start, or already finished.
    UnknownSpan(u64),
    PoolFull,
    IdExhausted,
    EndBeforeStart,
    /// More than [`MAX_EXPORTED`] spans.
    ExportLimit,
    /// The run completed with spans still open (the host cancels them).
    UnclosedSpans(usize),
    /// An argument does not have the declared shape.
    Malformed(&'static str),
}

impl fmt::Display for TraceRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotChecked(n) => write!(f, "program has {n} diagnostic(s)"),
            Self::NotBeacon => write!(f, "program does not use the built-in Trace effect"),
            Self::Entry(e) => write!(f, "invalid traced entry: {e}"),
            Self::Runtime(e) => write!(f, "runtime error: {e}"),
            Self::UnsupportedEffect(e) => write!(f, "host does not provide effect {e}"),
            Self::UnknownSchema(s) => write!(f, "unknown span schema id {s}"),
            Self::UnknownErrorCode(c) => write!(f, "unknown span error code {c}"),
            Self::InvalidContext => write!(f, "invalid trace context"),
            Self::UnknownSpan(id) => write!(f, "span {id} is not open in this host"),
            Self::PoolFull => write!(f, "too many open spans"),
            Self::IdExhausted => write!(f, "span ids exhausted"),
            Self::EndBeforeStart => write!(f, "span finished before it started"),
            Self::ExportLimit => write!(f, "too many exported spans"),
            Self::UnclosedSpans(n) => write!(f, "{n} span(s) left open"),
            Self::Malformed(what) => write!(f, "malformed {what}"),
        }
    }
}

impl std::error::Error for TraceRunError {}

/// Runs `entry` (which must carry exactly `eff [Trace]`) with the Beacon host.
pub fn run_traced(
    checked: &Checked,
    entry: &str,
    args: Vec<Value>,
    config: TraceConfig,
) -> Result<TraceRun, TraceRunError> {
    if !checked.diagnostics.is_empty() {
        return Err(TraceRunError::NotChecked(checked.diagnostics.len()));
    }
    let world = &checked.world;
    if !super::active(world) {
        return Err(TraceRunError::NotBeacon);
    }
    let function = world
        .functions
        .get(entry)
        .ok_or_else(|| TraceRunError::Entry(format!("no function '{entry}'")))?;
    if function.effects.tail.is_some()
        || function.effects.instances.len() != 1
        || function.effects.instances[0].name != TRACE
    {
        return Err(TraceRunError::Entry(format!(
            "'{entry}' must carry exactly eff [Trace]"
        )));
    }
    let first = NonZeroU64::new(config.first_span_id).ok_or(TraceRunError::InvalidContext)?;
    let pool = SpanPool::<ACTIVE_SPANS>::new(config.trace_id, first, config.flags)
        .map_err(|_| TraceRunError::InvalidContext)?;
    let mut host = Host {
        config,
        pool: &pool,
        ring: TelemetryRing::default(),
        open: BTreeMap::new(),
        stack: vec![],
        names: BTreeMap::new(),
        messages: BTreeMap::new(),
        exported: vec![],
        clock: 0,
    };
    let result = host.drive(world, entry, args);
    let unclosed = host.cancel_open();
    let value = result?;
    if unclosed > 0 {
        return Err(TraceRunError::UnclosedSpans(unclosed));
    }
    host.drain()?;
    Ok(TraceRun {
        value,
        spans: host.exported,
    })
}

struct Host<'p> {
    config: TraceConfig,
    pool: &'p SpanPool<ACTIVE_SPANS>,
    ring: TelemetryRing<RING_CAPACITY>,
    open: BTreeMap<u64, vesper_beacon::ActiveSpan<'p, ACTIVE_SPANS>>,
    /// Open spans in start order; the last one is the current context.
    stack: Vec<W3cSpanContext>,
    names: BTreeMap<u64, String>,
    messages: BTreeMap<u64, String>,
    exported: Vec<ExportedSpan>,
    clock: u64,
}

impl Host<'_> {
    fn drive(
        &mut self,
        world: &crate::check::World,
        entry: &str,
        args: Vec<Value>,
    ) -> Result<Value, TraceRunError> {
        let mut runtime = Runtime::new(world);
        runtime.fuel = self.config.fuel;
        let mut state = runtime
            .begin_external(entry, args)
            .map_err(TraceRunError::Runtime)?;
        loop {
            match state {
                Execution::Complete(value) => return Ok(value),
                Execution::Suspended(suspension) => {
                    let request = suspension.request();
                    if request.effect.name != TRACE {
                        return Err(TraceRunError::UnsupportedEffect(
                            request.effect.name.clone(),
                        ));
                    }
                    let reply = self.operation(&request.operation, &request.arguments)?;
                    state = suspension.resume(reply).map_err(TraceRunError::Runtime)?;
                }
            }
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn current(&self) -> Value {
        match self.stack.last().copied().or(self.config.parent) {
            Some(ctx) => context_value(ctx),
            None => root_value(self.config.trace_id, self.config.flags),
        }
    }

    fn operation(&mut self, op: &str, args: &[Value]) -> Result<Value, TraceRunError> {
        let now = self.tick();
        match (op, args) {
            ("current_ctx", []) => Ok(self.current()),
            ("start_span", [schema, name]) => {
                let parent = self.current();
                self.start(&parent, schema, name, now)
            }
            ("start_child", [parent, schema, name]) => self.start(parent, schema, name, now),
            ("emit_ok", [span, attr]) => {
                let attr = PrimaryAttribute::ItemCount(uint(attr, "attribute")?);
                self.finish(span, SpanStatus::Ok, attr, None, now)
            }
            ("emit_err", [span, code, message]) => {
                let code = match uint(code, "error code")? {
                    0 => ErrorCode::InvalidInput,
                    1 => ErrorCode::Storage,
                    2 => ErrorCode::Timeout,
                    3 => ErrorCode::Cancelled,
                    other => return Err(TraceRunError::UnknownErrorCode(other)),
                };
                let Value::String(message) = message else {
                    return Err(TraceRunError::Malformed("error message"));
                };
                self.finish(
                    span,
                    SpanStatus::Error(code),
                    PrimaryAttribute::None,
                    Some(message.clone()),
                    now,
                )
            }
            ("emit_attr", [span, attr]) => {
                let attr = telemetry_attr(attr)?;
                self.finish(span, SpanStatus::Ok, attr, None, now)
            }
            _ => Err(TraceRunError::Malformed("Trace operation")),
        }
    }

    fn start(
        &mut self,
        parent: &Value,
        schema: &Value,
        name: &Value,
        now: u64,
    ) -> Result<Value, TraceRunError> {
        let schema_id = uint(schema, "schema id")?;
        let schema = match schema_id {
            0 => SpanSchema::Request,
            1 => SpanSchema::StorageAppend,
            2 => SpanSchema::Background,
            other => return Err(TraceRunError::UnknownSchema(other)),
        };
        let Value::String(name) = name else {
            return Err(TraceRunError::Malformed("span name"));
        };
        let parent = self.parent_context(parent)?;
        let span = self
            .pool
            .start_span(parent, schema, now, now)
            .map_err(|e| match e {
                StartError::PoolFull => TraceRunError::PoolFull,
                StartError::IdExhausted => TraceRunError::IdExhausted,
            })?;
        let ctx = span.context();
        self.names.insert(ctx.span_id(), name.clone());
        self.stack.push(ctx);
        self.open.insert(ctx.span_id(), span);
        let mut fields = BTreeMap::new();
        fields.insert("ctx".into(), context_value(ctx));
        fields.insert(
            "parent_span_id".into(),
            Value::UInt(parent.map_or(0, W3cSpanContext::span_id), 64),
        );
        fields.insert("start_mono_ns".into(), Value::UInt(now, 64));
        fields.insert("schema_id".into(), Value::UInt(schema_id, 32));
        Ok(data(ACTIVE_SPAN, fields))
    }

    /// A source context: a valid W3C context, or this trace's root marker.
    fn parent_context(&self, v: &Value) -> Result<Option<W3cSpanContext>, TraceRunError> {
        let (trace_id, span_id, flags) = context_fields(v)?;
        if span_id == 0 {
            return if trace_id == self.config.trace_id {
                Ok(None)
            } else {
                Err(TraceRunError::InvalidContext)
            };
        }
        W3cSpanContext::new(trace_id, span_id, flags)
            .map(Some)
            .map_err(|_| TraceRunError::InvalidContext)
    }

    fn finish(
        &mut self,
        span: &Value,
        status: SpanStatus,
        attribute: PrimaryAttribute,
        message: Option<String>,
        now: u64,
    ) -> Result<Value, TraceRunError> {
        let Value::Data { ty, fields, .. } = span else {
            return Err(TraceRunError::Malformed("span"));
        };
        if ty != ACTIVE_SPAN {
            return Err(TraceRunError::Malformed("span"));
        }
        let (trace_id, span_id, _) =
            context_fields(fields.get("ctx").ok_or(TraceRunError::Malformed("span"))?)?;
        let owner = self
            .open
            .remove(&span_id)
            .ok_or(TraceRunError::UnknownSpan(span_id))?;
        if owner.context().trace_id() != trace_id {
            self.open.insert(span_id, owner);
            return Err(TraceRunError::UnknownSpan(span_id));
        }
        let mut owner = owner;
        loop {
            let result = match status {
                SpanStatus::Ok => owner.finish_ok(&mut self.ring, now, attribute),
                SpanStatus::Error(code) => owner.finish_err(&mut self.ring, now, code, attribute),
            };
            match result {
                Ok(()) => break,
                Err(FinishFailure {
                    error: FinishError::QueueFull,
                    span,
                }) => {
                    owner = span;
                    if let Err(e) = self.drain() {
                        self.open.insert(span_id, owner);
                        return Err(e);
                    }
                }
                Err(FinishFailure {
                    error: FinishError::EndBeforeStart,
                    span,
                }) => {
                    self.open.insert(span_id, span);
                    return Err(TraceRunError::EndBeforeStart);
                }
            }
        }
        self.stack.retain(|c| c.span_id() != span_id);
        if let Some(message) = message {
            self.messages.insert(span_id, message);
        }
        Ok(Value::Tuple(vec![]))
    }

    fn drain(&mut self) -> Result<(), TraceRunError> {
        while !self.ring.is_empty() {
            let batch = self.ring.drain::<RING_CAPACITY>();
            for i in 0..batch.len() {
                if self.exported.len() == MAX_EXPORTED {
                    return Err(TraceRunError::ExportLimit);
                }
                let span_id = batch.span_ids()[i];
                self.exported.push(ExportedSpan {
                    trace_id: batch.trace_ids()[i],
                    span_id,
                    parent_span_id: batch.parent_span_ids()[i],
                    flags: batch.flags()[i],
                    name: self.names.get(&span_id).cloned().unwrap_or_default(),
                    schema: batch.schemas()[i],
                    status: batch.statuses()[i],
                    message: self.messages.remove(&span_id),
                    attribute: batch.attributes()[i],
                    start_unix_ns: batch.start_unix_ns()[i],
                    duration_ns: batch.duration_ns()[i],
                });
            }
        }
        Ok(())
    }

    /// Cancels spans a failed run left open, so that no Beacon owner is
    /// dropped silently. Returns how many there were.
    fn cancel_open(&mut self) -> usize {
        let open = std::mem::take(&mut self.open);
        let count = open.len();
        let end = self.clock.saturating_add(1);
        for (_, mut span) in open {
            loop {
                match span.cancel(&mut self.ring, end) {
                    Ok(()) => break,
                    Err(FinishFailure {
                        error: FinishError::QueueFull,
                        span: s,
                    }) => {
                        // Make room; cancelled spans are not exported.
                        let _ = self.ring.drain::<RING_CAPACITY>();
                        span = s;
                    }
                    // Unreachable (end is after every start); the owner then
                    // stays visible as an occupied pool slot.
                    Err(FinishFailure {
                        error: FinishError::EndBeforeStart,
                        ..
                    }) => break,
                }
            }
        }
        if count > 0 {
            // Cancelled records are diagnostics of a failed run, not exports.
            let _ = self.ring.drain::<RING_CAPACITY>();
            self.ring = TelemetryRing::default();
        }
        count
    }
}

fn data(ty: &str, fields: BTreeMap<String, Value>) -> Value {
    Value::Data {
        ty: ty.into(),
        constructor: ty.into(),
        fields,
        resource_identity: None,
    }
}

fn context_value(ctx: W3cSpanContext) -> Value {
    let id = ctx.trace_id();
    context_parts(
        (id >> 64) as u64,
        id as u64,
        ctx.span_id(),
        ctx.trace_flags(),
    )
}

fn root_value(trace_id: u128, flags: u8) -> Value {
    context_parts((trace_id >> 64) as u64, trace_id as u64, 0, flags)
}

fn context_parts(hi: u64, lo: u64, span_id: u64, flags: u8) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("trace_id_hi".into(), Value::UInt(hi, 64));
    fields.insert("trace_id_lo".into(), Value::UInt(lo, 64));
    fields.insert("span_id".into(), Value::UInt(span_id, 64));
    fields.insert("flags".into(), Value::UInt(u64::from(flags), 8));
    data(SPAN_CONTEXT, fields)
}

fn context_fields(v: &Value) -> Result<(u128, u64, u8), TraceRunError> {
    let Value::Data { ty, fields, .. } = v else {
        return Err(TraceRunError::Malformed("context"));
    };
    if ty != SPAN_CONTEXT {
        return Err(TraceRunError::Malformed("context"));
    }
    let field = |name: &'static str| {
        fields
            .get(name)
            .ok_or(TraceRunError::Malformed("context"))
            .and_then(|v| uint(v, "context"))
    };
    let hi = field("trace_id_hi")?;
    let lo = field("trace_id_lo")?;
    let flags = u8::try_from(field("flags")?).map_err(|_| TraceRunError::Malformed("context"))?;
    Ok((
        (u128::from(hi) << 64) | u128::from(lo),
        field("span_id")?,
        flags,
    ))
}

fn uint(v: &Value, what: &'static str) -> Result<u64, TraceRunError> {
    match v {
        Value::UInt(n, _) => Ok(*n),
        Value::Int(n) => u64::try_from(*n).map_err(|_| TraceRunError::Malformed(what)),
        _ => Err(TraceRunError::Malformed(what)),
    }
}

fn telemetry_attr(v: &Value) -> Result<PrimaryAttribute, TraceRunError> {
    let Value::Data {
        ty,
        constructor,
        fields,
        ..
    } = v
    else {
        return Err(TraceRunError::Malformed("telemetry attribute"));
    };
    if ty != super::TELEMETRY_ATTR {
        return Err(TraceRunError::Malformed("telemetry attribute"));
    }
    let payload = || {
        fields
            .values()
            .next()
            .ok_or(TraceRunError::Malformed("telemetry attribute"))
            .and_then(|v| uint(v, "telemetry attribute"))
    };
    match constructor.rsplit("::").next().unwrap_or(constructor) {
        "NoAttr" => Ok(PrimaryAttribute::None),
        "Count" => Ok(PrimaryAttribute::ItemCount(payload()?)),
        "Bytes" => Ok(PrimaryAttribute::ByteCount(payload()?)),
        // The marker is content-independent; nothing about the source value
        // reaches the record.
        "Redacted" => Ok(PrimaryAttribute::Redacted(Secret::new(()).redact())),
        _ => Err(TraceRunError::Malformed("telemetry attribute")),
    }
}

/// Renders spans as an indented forest (see [`TraceRun::tree`]).
pub fn render_tree(spans: &[ExportedSpan]) -> String {
    // Spans are keyed by (trace id, span id): a parent in another trace is
    // external even when its span id coincides with an exported span.
    let key = |s: &ExportedSpan| (s.trace_id, s.span_id);
    let ids: std::collections::BTreeSet<(u128, u64)> = spans.iter().map(key).collect();
    let mut children: BTreeMap<(u128, u64), Vec<&ExportedSpan>> = BTreeMap::new();
    let mut roots = vec![];
    for s in spans {
        let parent = (s.trace_id, s.parent_span_id);
        if ids.contains(&parent) && s.parent_span_id != s.span_id {
            children.entry(parent).or_default().push(s);
        } else {
            roots.push(s);
        }
    }
    roots.sort_by_key(|s| (s.span_id, s.trace_id));
    children
        .values_mut()
        .for_each(|c| c.sort_by_key(|s| s.span_id));
    let mut out = String::new();
    let mut stack: Vec<(&ExportedSpan, usize)> = roots.into_iter().rev().map(|s| (s, 0)).collect();
    while let Some((s, depth)) = stack.pop() {
        out.push_str(&"  ".repeat(depth));
        out.push_str(&line(s));
        out.push('\n');
        if let Some(c) = children.get(&key(s)) {
            stack.extend(c.iter().rev().map(|c| (*c, depth + 1)));
        }
    }
    out
}

fn line(s: &ExportedSpan) -> String {
    let schema = match s.schema {
        SpanSchema::Request => "request",
        SpanSchema::StorageAppend => "storage_append",
        SpanSchema::Background => "background",
    };
    let status = match s.status {
        SpanStatus::Ok => "ok".to_string(),
        SpanStatus::Error(code) => {
            let code = match code {
                ErrorCode::InvalidInput => "invalid_input",
                ErrorCode::Storage => "storage",
                ErrorCode::Timeout => "timeout",
                ErrorCode::Cancelled => "cancelled",
            };
            match &s.message {
                Some(m) => format!("err({code}) {m:?}"),
                None => format!("err({code})"),
            }
        }
    };
    let attribute = match s.attribute {
        PrimaryAttribute::None => String::new(),
        PrimaryAttribute::ItemCount(n) => format!(" count={n}"),
        PrimaryAttribute::ByteCount(n) => format!(" bytes={n}"),
        PrimaryAttribute::Redacted(r) => {
            format!(" {}", String::from_utf8_lossy(r.as_bytes()))
        }
    };
    format!("{} [{schema}] {status}{attribute}", s.name)
}
