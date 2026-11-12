//! What to execute: one macro component invocation with declared inputs,
//! deterministic settings and explicit budgets.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vesper_loom_sandbox::{Determinism, OUTPUT_GUEST_PATH, ResourceLimits, StepKind};

use crate::record::RecordLimits;

/// The two macro step kinds. Build scripts are deliberately not
/// representable: they belong to `vesper-loom-build`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MacroKind {
    ProcMacro,
    DeriveMacro,
}

impl MacroKind {
    pub fn step(self) -> StepKind {
        match self {
            MacroKind::ProcMacro => StepKind::ProcMacro,
            MacroKind::DeriveMacro => StepKind::DeriveMacro,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MacroKind::ProcMacro => "proc-macro",
            MacroKind::DeriveMacro => "derive-macro",
        }
    }
}

/// A read-only host directory snapshotted into the CAS and mounted at
/// `guest_path`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSpec {
    pub host_path: PathBuf,
    pub guest_path: String,
}

/// One macro execution request. The token input comes from the hygiene
/// session at expansion time; everything else is here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroRequest {
    /// Exact `wasm32-wasip2` component bytes.
    pub component: Vec<u8>,
    pub kind: MacroKind,
    /// Exported entry point name.
    pub entry: String,
    pub args: Vec<String>,
    pub inputs: Vec<InputSpec>,
    pub limits: ResourceLimits,
    pub determinism: Determinism,
}

impl MacroRequest {
    /// A request with conservative default budgets (see [`RunnerLimits`]).
    pub fn new(component: Vec<u8>, kind: MacroKind, entry: impl Into<String>) -> Self {
        let caps = RunnerLimits::default();
        let limits = ResourceLimits {
            transport: caps.transport,
            max_memory_bytes: caps.max_memory_bytes,
            fuel: 2_000_000_000,
            max_output_entries: 1,
            max_output_bytes: 0,
            max_stdio_bytes: caps.max_stdio_bytes,
            ..ResourceLimits::default()
        };
        Self {
            component,
            kind,
            entry: entry.into(),
            args: Vec::new(),
            inputs: Vec::new(),
            limits,
            determinism: Determinism::default(),
        }
    }

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_input(
        mut self,
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
    ) -> Self {
        self.inputs.push(InputSpec {
            host_path: host_path.into(),
            guest_path: guest_path.into(),
        });
        self
    }

    /// Bounds a stored record of this request must satisfy.
    pub(crate) fn record_limits(&self, caps: &RunnerLimits) -> RecordLimits {
        RecordLimits {
            transport: self.limits.transport,
            max_stdio_bytes: self.limits.max_stdio_bytes,
            max_record_bytes: caps.max_record_bytes,
        }
    }

    /// Checks the request against the runner's caps. Mount layout and zero
    /// limits are additionally checked by the sandbox's own `StepConfig`
    /// validation, which the runner performs before any snapshot or cache
    /// lookup.
    pub fn validate(&self, caps: &RunnerLimits) -> Result<(), RequestError> {
        if self.entry.is_empty()
            || self.entry.len() > 256
            || self.entry.chars().any(char::is_control)
        {
            return Err(RequestError::BadEntry);
        }
        // Mirrors the sandbox's own `StepConfig` layout rules so that a request
        // that could never run is rejected before a cache lookup, too.
        let out = guest_components(OUTPUT_GUEST_PATH).expect("constant is valid");
        let mut seen: Vec<(&str, Vec<&str>)> = Vec::new();
        for i in &self.inputs {
            let bad = |reason: &str| RequestError::Mount {
                guest_path: i.guest_path.clone(),
                reason: reason.to_owned(),
            };
            let parts = guest_components(&i.guest_path).map_err(&bad)?;
            if is_prefix(&parts, &out) || is_prefix(&out, &parts) {
                return Err(bad("overlaps the output mount"));
            }
            for (other, other_parts) in &seen {
                if is_prefix(other_parts, &parts) || is_prefix(&parts, other_parts) {
                    return Err(bad(&format!("nests with {other}")));
                }
            }
            seen.push((&i.guest_path, parts));
        }
        let l = &self.limits;
        for (field, zero) in [
            ("fuel", l.fuel == 0),
            ("max_memory_bytes", l.max_memory_bytes == 0),
            ("max_instances", l.max_instances == 0),
            ("wall_clock_backstop", l.wall_clock_backstop.is_zero()),
        ] {
            if zero {
                return Err(RequestError::ZeroLimit { field });
            }
        }
        let t = &l.transport;
        let c = &caps.transport;
        for (field, value, cap) in [
            ("transport.max_tokens", t.max_tokens, c.max_tokens),
            (
                "transport.max_text_bytes",
                t.max_text_bytes,
                c.max_text_bytes,
            ),
            (
                "transport.max_scopes_per_token",
                t.max_scopes_per_token,
                c.max_scopes_per_token,
            ),
            (
                "transport.max_total_scopes",
                t.max_total_scopes,
                c.max_total_scopes,
            ),
            (
                "transport.max_diagnostics",
                t.max_diagnostics,
                c.max_diagnostics,
            ),
            (
                "max_memory_bytes",
                l.max_memory_bytes,
                caps.max_memory_bytes,
            ),
            ("max_stdio_bytes", l.max_stdio_bytes, caps.max_stdio_bytes),
        ] {
            if value > cap {
                return Err(RequestError::LimitAboveCap { field, value, cap });
            }
        }
        // Even at the caps, a maximal record must stay storable.
        let worst = worst_case_record(&self.record_limits(caps));
        if worst.is_none_or(|w| w > caps.max_record_bytes) {
            return Err(RequestError::RecordBoundTooLarge {
                worst_case: worst,
                cap: caps.max_record_bytes,
            });
        }
        Ok(())
    }
}

fn guest_components(path: &str) -> Result<Vec<&str>, &'static str> {
    let rest = path.strip_prefix('/').ok_or("not absolute")?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts == [""] {
        return Err("is the root");
    }
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains('\\') || p.contains('\0'))
    {
        return Err("not normalized");
    }
    Ok(parts)
}

fn is_prefix(a: &[&str], b: &[&str]) -> bool {
    a.len() <= b.len() && a.iter().zip(b).all(|(x, y)| x == y)
}

fn worst_case_record(l: &RecordLimits) -> Option<usize> {
    let t = &l.transport;
    (4 + 4 + 32 + 8 + 8 + 24usize)
        .checked_add(t.max_tokens.checked_mul(31)?)?
        .checked_add(t.max_diagnostics.checked_mul(18)?)?
        .checked_add(t.max_total_scopes.checked_mul(4)?)?
        .checked_add(t.max_text_bytes.checked_mul(2)?)?
        .checked_add(l.max_stdio_bytes.checked_mul(2)?)
}

/// Upper bounds a runner accepts for any request. They bound host
/// allocation: Wasmtime's canonical-ABI lifting of the packed result is
/// bounded by guest linear memory (`max_memory_bytes`), the sandbox then
/// checks the transport limits before allocating any per-token value, and
/// stored records are bounded by `max_record_bytes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerLimits {
    pub transport: vesper_loom_sandbox::TransportLimits,
    pub max_memory_bytes: usize,
    pub max_stdio_bytes: usize,
    pub max_record_bytes: usize,
}

impl Default for RunnerLimits {
    fn default() -> Self {
        Self {
            transport: vesper_loom_sandbox::TransportLimits {
                max_tokens: 1 << 16,
                max_text_bytes: 1 << 20,
                max_scopes_per_token: 64,
                max_total_scopes: 1 << 16,
                max_diagnostics: 256,
            },
            max_memory_bytes: 64 << 20,
            max_stdio_bytes: 64 << 10,
            max_record_bytes: 8 << 20,
        }
    }
}

/// A request the runner refuses before touching inputs or the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestError {
    BadEntry,
    ZeroLimit {
        field: &'static str,
    },
    Mount {
        guest_path: String,
        reason: String,
    },
    LimitAboveCap {
        field: &'static str,
        value: usize,
        cap: usize,
    },
    RecordBoundTooLarge {
        worst_case: Option<usize>,
        cap: usize,
    },
    /// An input overlaps the store or scratch area.
    LayoutOverlap {
        input: PathBuf,
        area: PathBuf,
    },
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
