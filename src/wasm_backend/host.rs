//! Host capabilities: effects the entry leaves unhandled that map to WASI
//! 0.2 or Beacon WIT imports.
//!
//! A capability is *declared*: the entry's effect row names it, and the
//! program declares the effect with exactly the recognised signature. Only
//! then does the component import the interface; an entry with any
//! other unhandled effect is rejected. The recognised capabilities are
//!
//! * `linear effect Stdout { fn write(s: String); }`: `wasi:cli/stdout`
//!   and `wasi:io/streams` (`blocking-write-and-flush`, in chunks of at
//!   most 4096 bytes, as the interface requires); a stream error traps.
//! * `linear effect Clock { fn now_ns() -> u64; }`:
//!   `wasi:clocks/monotonic-clock.now`, in nanoseconds.
//! * The built-in Beacon `Trace` effect (`crate::trace_effects::PRELUDE`):
//!   `vesper:beacon/trace@0.1.0`.
//!
//! An operation on one of them dispatches through a host frame installed by
//! the export wrapper, exactly like a handler frame, so a capability may be
//! performed anywhere below the entry (through calls, closures and handler
//! bodies) and handled locally by a source handler that shadows it.
use super::WasmError;
use crate::{ast::*, check::World};

pub(crate) const IO_WIT: &str = "package wasi:io@0.2.0;\n\ninterface error {\n  resource error;\n}\n\ninterface streams {\n  use error.{error};\n  variant stream-error {\n    last-operation-failed(error),\n    closed,\n  }\n  resource output-stream {\n    blocking-write-and-flush: func(contents: list<u8>) -> result<_, stream-error>;\n  }\n}\n";
pub(crate) const CLI_WIT: &str = "package wasi:cli@0.2.0;\n\ninterface run {\n  run: func() -> result;\n}\n\ninterface stdout {\n  use wasi:io/streams@0.2.0.{output-stream};\n  get-stdout: func() -> output-stream;\n}\n";
pub(crate) const CLOCKS_WIT: &str = "package wasi:clocks@0.2.0;\n\ninterface monotonic-clock {\n  type instant = u64;\n  now: func() -> instant;\n}\n";
pub(crate) const BEACON_WIT: &str = "package vesper:beacon@0.1.0;

interface trace {
  record span-context {
    trace-id-hi: u64,
    trace-id-lo: u64,
    span-id: u64,
    %flags: u8,
  }

  record active-span {
    ctx: span-context,
    parent-span-id: u64,
    start-mono-ns: u64,
    schema-id: u32,
  }

  variant telemetry-attr {
    no-attr,
    count(u64),
    bytes(u64),
    redacted,
  }

  current-ctx: func() -> span-context;
  start-span: func(schema-id: u32, name: string) -> active-span;
  start-child: func(parent: span-context, schema-id: u32, name: string) -> active-span;
  emit-ok: func(span: active-span, attr-u64: u64);
  emit-err: func(span: active-span, err-code: u32, msg: string);
  emit-attr: func(span: active-span, attr: telemetry-attr);
}
";

pub const WASI_STDOUT: &str = "wasi:cli/stdout@0.2.0";
pub const WASI_STREAMS: &str = "wasi:io/streams@0.2.0";
pub const WASI_MONOTONIC_CLOCK: &str = "wasi:clocks/monotonic-clock@0.2.0";
pub const BEACON_TRACE: &str = "vesper:beacon/trace@0.1.0";

/// A recognised host capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostCapability {
    /// `Stdout::write(String)`.
    Stdout,
    /// `Clock::now_ns() -> u64`.
    Clock,
    /// Built-in Beacon `Trace` effect (§22 / §31.2).
    Trace,
}

impl HostCapability {
    pub fn effect(self) -> &'static str {
        match self {
            Self::Stdout => "Stdout",
            Self::Clock => "Clock",
            Self::Trace => crate::trace_effects::TRACE,
        }
    }
    /// The WIT interface the component imports for it.
    pub fn interface(self) -> &'static str {
        match self {
            Self::Stdout => WASI_STDOUT,
            Self::Clock => WASI_MONOTONIC_CLOCK,
            Self::Trace => BEACON_TRACE,
        }
    }
    fn signature_matches(self, world: &World, decl: &EffectDecl) -> bool {
        let named = |t: &Type, n: &str| crate::numeric::default_type(t).name() == n;
        let is_unit = |t: &Type| named(t, "Unit") || *t == Type::unit();
        if !decl.generics.is_empty() {
            return false;
        }
        match self {
            Self::Stdout => {
                if decl.ops.len() != 1 {
                    return false;
                }
                let op = &decl.ops[0];
                op.generics.is_empty()
                    && op.effects.instances.is_empty()
                    && op.name == "write"
                    && op.params.len() == 1
                    && op.params[0].pass != Pass::Inout
                    && named(&op.params[0].ty, "String")
                    && is_unit(&op.result)
            }
            Self::Clock => {
                if decl.ops.len() != 1 {
                    return false;
                }
                let op = &decl.ops[0];
                op.generics.is_empty()
                    && op.effects.instances.is_empty()
                    && op.name == "now_ns"
                    && op.params.is_empty()
                    && named(&op.result, "u64")
            }
            Self::Trace => {
                if !crate::trace_effects::active(world)
                    || decl.ops.len() != 6
                    || decl
                        .ops
                        .iter()
                        .any(|op| !op.generics.is_empty() || !op.effects.instances.is_empty())
                {
                    return false;
                }
                let ops = &decl.ops;
                ops[0].name == "current_ctx"
                    && ops[0].params.is_empty()
                    && named(&ops[0].result, crate::trace_effects::SPAN_CONTEXT)
                    && ops[1].name == "start_span"
                    && ops[1].params.len() == 2
                    && ops[1].params[0].pass != Pass::Inout
                    && ops[1].params[1].pass != Pass::Inout
                    && named(&ops[1].params[0].ty, "u32")
                    && named(&ops[1].params[1].ty, "String")
                    && named(&ops[1].result, crate::trace_effects::ACTIVE_SPAN)
                    && ops[2].name == "start_child"
                    && ops[2].params.len() == 3
                    && ops[2].params.iter().all(|p| p.pass != Pass::Inout)
                    && named(&ops[2].params[0].ty, crate::trace_effects::SPAN_CONTEXT)
                    && named(&ops[2].params[1].ty, "u32")
                    && named(&ops[2].params[2].ty, "String")
                    && named(&ops[2].result, crate::trace_effects::ACTIVE_SPAN)
                    && ops[3].name == "emit_ok"
                    && ops[3].params.len() == 2
                    && ops[3].params.iter().all(|p| p.pass != Pass::Inout)
                    && named(&ops[3].params[0].ty, crate::trace_effects::ACTIVE_SPAN)
                    && named(&ops[3].params[1].ty, "u64")
                    && is_unit(&ops[3].result)
                    && ops[4].name == "emit_err"
                    && ops[4].params.len() == 3
                    && ops[4].params.iter().all(|p| p.pass != Pass::Inout)
                    && named(&ops[4].params[0].ty, crate::trace_effects::ACTIVE_SPAN)
                    && named(&ops[4].params[1].ty, "u32")
                    && named(&ops[4].params[2].ty, "String")
                    && is_unit(&ops[4].result)
                    && ops[5].name == "emit_attr"
                    && ops[5].params.len() == 2
                    && ops[5].params.iter().all(|p| p.pass != Pass::Inout)
                    && named(&ops[5].params[0].ty, crate::trace_effects::ACTIVE_SPAN)
                    && named(&ops[5].params[1].ty, crate::trace_effects::TELEMETRY_ATTR)
                    && is_unit(&ops[5].result)
            }
        }
    }
}

/// The host capabilities the entry declares, or why its row cannot be
/// served by the host.
pub(crate) fn capabilities(world: &World, entry: &str) -> Result<Vec<HostCapability>, WasmError> {
    let Some(f) = world.functions.get(entry) else {
        return Err(WasmError::Entry(format!(
            "unknown entry function '{entry}'"
        )));
    };
    if f.effects.tail.is_some() {
        return Err(WasmError::Entry(
            "the entry must have a closed effect row".into(),
        ));
    }
    let mut out = vec![];
    for e in &f.effects.instances {
        if crate::capability_effects::is_capability_effect(&e.name) {
            continue;
        }
        let cap = [
            HostCapability::Stdout,
            HostCapability::Clock,
            HostCapability::Trace,
        ]
        .into_iter()
        .find(|c| c.effect() == e.name);
        let decl = world.effects.get(&e.name);
        match (cap, decl) {
            (Some(c), Some(d))
                if d.linear
                    && e.args.is_empty()
                    && e.label.is_none()
                    && c.signature_matches(world, d) =>
            {
                if !out.contains(&c) {
                    out.push(c);
                }
            }
            (Some(c), _) => {
                return Err(WasmError::Entry(format!(
                    "host capability {} must be declared as `linear effect {}` with {} and used without a label",
                    c.effect(),
                    c.effect(),
                    match c {
                        HostCapability::Stdout => "`fn write(s: String);`",
                        HostCapability::Clock => "`fn now_ns() -> u64;`",
                        HostCapability::Trace => "the built-in Beacon `Trace` prelude",
                    }
                )));
            }
            (None, _) => {
                return Err(WasmError::Entry(format!(
                    "the entry must handle effect {}: the host provides only Stdout, Clock and Trace",
                    e.name
                )));
            }
        }
    }
    out.sort();
    Ok(out)
}
