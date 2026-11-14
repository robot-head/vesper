//! Host capabilities: effects the entry leaves unhandled that map to WASI
//! 0.2 imports.
//!
//! A capability is *declared*: the entry's effect row names it, and the
//! program declares the effect with exactly the recognised signature. Only
//! then does the component import the WASI interface; an entry with any
//! other unhandled effect is rejected. The recognised capabilities are
//!
//! * `linear effect Stdout { fn write(s: String); }`: `wasi:cli/stdout`
//!   and `wasi:io/streams` (`blocking-write-and-flush`, in chunks of at
//!   most 4096 bytes, as the interface requires); a stream error traps.
//! * `linear effect Clock { fn now_ns() -> u64; }`:
//!   `wasi:clocks/monotonic-clock.now`, in nanoseconds.
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

pub const WASI_STDOUT: &str = "wasi:cli/stdout@0.2.0";
pub const WASI_STREAMS: &str = "wasi:io/streams@0.2.0";
pub const WASI_MONOTONIC_CLOCK: &str = "wasi:clocks/monotonic-clock@0.2.0";

/// A recognised host capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostCapability {
    /// `Stdout::write(String)`.
    Stdout,
    /// `Clock::now_ns() -> u64`.
    Clock,
}

impl HostCapability {
    pub fn effect(self) -> &'static str {
        match self {
            Self::Stdout => "Stdout",
            Self::Clock => "Clock",
        }
    }
    /// The WIT interface the component imports for it.
    pub fn interface(self) -> &'static str {
        match self {
            Self::Stdout => WASI_STDOUT,
            Self::Clock => WASI_MONOTONIC_CLOCK,
        }
    }
    fn signature_matches(self, decl: &EffectDecl) -> bool {
        let named = |t: &Type, n: &str| crate::numeric::default_type(t).name() == n;
        if !decl.generics.is_empty() || decl.ops.len() != 1 {
            return false;
        }
        let op = &decl.ops[0];
        if !op.generics.is_empty() || !op.effects.instances.is_empty() {
            return false;
        }
        match self {
            Self::Stdout => {
                op.name == "write"
                    && op.params.len() == 1
                    && op.params[0].pass != Pass::Inout
                    && named(&op.params[0].ty, "String")
                    && (named(&op.result, "Unit") || op.result == Type::unit())
            }
            Self::Clock => op.name == "now_ns" && op.params.is_empty() && named(&op.result, "u64"),
        }
    }
}

/// The host capabilities the entry declares, or why its row cannot be
/// served by the host.
pub(crate) fn capabilities(world: &World, entry: &str) -> Result<Vec<HostCapability>, WasmError> {
    let Some(f) = world.functions.get(entry) else {
        return Err(WasmError::Entry(format!("unknown entry function '{entry}'")));
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
        let cap = [HostCapability::Stdout, HostCapability::Clock]
            .into_iter()
            .find(|c| c.effect() == e.name);
        let decl = world.effects.get(&e.name);
        match (cap, decl) {
            (Some(c), Some(d))
                if d.linear && e.args.is_empty() && e.label.is_none() && c.signature_matches(d) =>
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
                    }
                )));
            }
            (None, _) => {
                return Err(WasmError::Entry(format!(
                    "the entry must handle effect {}: the host provides only Stdout and Clock",
                    e.name
                )));
            }
        }
    }
    out.sort();
    Ok(out)
}
