//! Which swaps the hybrid installs (SH6 lane 2, #1397): every swap, or only
//! the swaps whose flip gate passed.
//!
//! `VESPER_HYBRID_SWAPS` picks the set:
//!
//! - `all` (the default): every swap below, as the hybrid has always run.
//! - `gated`: only the swaps whose `scripts/ci/flip_gate.py` verdict is
//!   PASS ([`Gate::Pass`]). Every other module stays on its Rust path until
//!   its own gate passes. This is the set the default `vesper` switches to
//!   (docs/maintenance/rust-exit-plan.md, "The retirement checklist").
//!   `VESPER_HYBRID_GATED=1` means the same.
//! - `none`: no swap; the hybrid then runs the Rust compiler behind the
//!   Vesper driver.
//!
//! The per-swap opt-outs (`VESPER_HYBRID_SEQ=0` and the rest) still turn a
//! swap off inside any set. The Vesper driver and its command layers
//! (`driver.rs`, `commands.rs`) are the hybrid's entry, not a module swap,
//! and run in every set.
//!
//! A swap is promoted by changing its row to [`Gate::Pass`] in the PR that
//! adds its gate configuration and scoreboard row; `tests/swaps.rs` checks
//! that both exist.

/// The flip gate's verdict on one swap.
pub(crate) enum Gate {
    /// `scripts/ci/flip_gate.py` passed: the configuration's name under
    /// `scripts/ci/flip-gate/` (also the scoreboard row's name) and the PR.
    Pass {
        config: &'static str,
        pr: &'static str,
    },
    /// Not gated yet: what is measured, or what is missing.
    Open(&'static str),
}

/// One swap: its name (the hook or slot it fills) and its verdict.
pub(crate) struct Swap {
    pub(crate) name: &'static str,
    pub(crate) gate: Gate,
}

const fn open(name: &'static str, why: &'static str) -> Swap {
    Swap {
        name,
        gate: Gate::Open(why),
    }
}

const fn pass(name: &'static str, config: &'static str, pr: &'static str) -> Swap {
    Swap {
        name,
        gate: Gate::Pass { config, pr },
    }
}

/// Every swap `install` can install, in install order.
pub(crate) const SWAPS: &[Swap] = &[
    open("word_layout", "0 diffs; no flip-gate configuration"),
    open("parse", "0 diffs; no flip-gate configuration"),
    open("layout", "0 diffs; no flip-gate configuration"),
    open("ir_check", "0 diffs, 0.53x; no flip-gate configuration"),
    open("stack_bound", "2.54x, above the bar"),
    open("tensor_check", "0.28-0.49x; no flip-gate configuration"),
    open("effects", "0.28x; no flip-gate configuration"),
    open("fip", "0.92x; no flip-gate configuration"),
    open("aggregate", "0 diffs; no flip-gate configuration"),
    open("call", "0 diffs; no flip-gate configuration"),
    open("wasm", "0 diffs; no flip-gate configuration"),
    open("ebpf", "0.93-1.10x; no flip-gate configuration"),
    open("freestanding", "no flip-gate configuration"),
    open("verifiers", "no flip-gate configuration"),
    open("register_locals", "0 diffs; no flip-gate configuration"),
    pass("component", "wit-component", "#1334"),
    open("numeric", "0 declined; no flip-gate configuration"),
    open("seq", "0 diffs; no flip-gate configuration"),
    open("control", "0 declined; no flip-gate configuration"),
    open("ffi", "0 diffs; no flip-gate configuration"),
    open("scalar", "0 diffs; no flip-gate configuration"),
    open("export", "no flip-gate configuration"),
    open("plan", "no flip-gate configuration"),
    open("interface_json", "no flip-gate configuration"),
    open("test_discovery", "no flip-gate configuration"),
    open(
        "doctest_extract",
        "0 diffs, 1.12x; no flip-gate configuration",
    ),
    open("ir_lower", "0.40x; no flip-gate configuration"),
    open("ir_opt", "0.0036x; no flip-gate configuration"),
    open("inline", "0.76x; no flip-gate configuration"),
    pass("hoist", "bounds-hoist", "#1251"),
    open(
        "doc",
        "0 byte diffs, 0.76-0.87x; no flip-gate configuration",
    ),
    pass("optimize", "optimize", "#1501"),
    open("declaration_pass", "no flip-gate configuration"),
    open("pact_session", "no flip-gate configuration"),
    open("builtin_wanted", "no flip-gate configuration"),
    open("deps_pass", "no flip-gate configuration"),
];

/// The set `VESPER_HYBRID_SWAPS` names.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Set {
    All,
    Gated,
    None,
}

impl Set {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Set::All => "all",
            Set::Gated => "gated",
            Set::None => "none",
        }
    }
}

/// Reads the set from the environment, or explains a value it does not
/// know.
fn read() -> Result<Set, String> {
    match std::env::var("VESPER_HYBRID_SWAPS") {
        Ok(v) => match v.as_str() {
            "" | "all" => Ok(Set::All),
            "gated" => Ok(Set::Gated),
            "none" => Ok(Set::None),
            _ => Err(format!(
                "VESPER_HYBRID_SWAPS={v} is not one of all, gated, none"
            )),
        },
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("VESPER_HYBRID_SWAPS is not one of all, gated, none".into())
        }
        Err(std::env::VarError::NotPresent) => {
            if std::env::var_os("VESPER_HYBRID_GATED").is_some_and(|v| v == "1") {
                Ok(Set::Gated)
            } else {
                Ok(Set::All)
            }
        }
    }
}

static SET: std::sync::OnceLock<Set> = std::sync::OnceLock::new();

/// The set this process runs, read once. An unknown value stops the
/// process before `main`: a typo must not silently run every swap.
pub(crate) fn set() -> Set {
    *SET.get_or_init(|| {
        read().unwrap_or_else(|e| {
            eprintln!("vesper-hybrid: {e}");
            std::process::exit(2)
        })
    })
}

/// Whether the swap `name` is in the set. `name` must be a row of
/// [`SWAPS`].
pub(crate) fn on(name: &str) -> bool {
    let Some(swap) = SWAPS.iter().find(|s| s.name == name) else {
        eprintln!("vesper-hybrid: `{name}` is not a row of swaps::SWAPS");
        std::process::abort();
    };
    match set() {
        Set::All => true,
        Set::Gated => matches!(swap.gate, Gate::Pass { .. }),
        Set::None => false,
    }
}

/// The `VESPER_HYBRID_STATS` report of the set: one line for the set and
/// one per swap (`tests/swaps.rs` reads them).
pub(crate) fn report() {
    let set = set();
    let n = SWAPS.iter().filter(|s| on(s.name)).count();
    eprintln!(
        "vesper-hybrid: swaps ({}): {n} of {} on",
        set.name(),
        SWAPS.len()
    );
    for s in SWAPS {
        let state = if on(s.name) { "on" } else { "off" };
        match s.gate {
            Gate::Pass { config, pr } => eprintln!(
                "vesper-hybrid: swap {}: {state}, flip gate PASS (scripts/ci/flip-gate/{config}.toml, {pr})",
                s.name
            ),
            Gate::Open(why) => {
                eprintln!("vesper-hybrid: swap {}: {state}, not gated ({why})", s.name)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        for (i, s) in SWAPS.iter().enumerate() {
            assert!(
                SWAPS[..i].iter().all(|t| t.name != s.name),
                "{} is listed twice",
                s.name
            );
        }
    }
}
