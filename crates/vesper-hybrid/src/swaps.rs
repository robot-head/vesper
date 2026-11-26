//! Which swaps the hybrid installs (SH6 lane 2, #1397): every swap, or only
//! the swaps whose flip gate passed.
//!
//! `VESPER_HYBRID_SWAPS` picks the set:
//!
//! - `all` (the default): every swap below, as the hybrid has always run.
//!   A hybrid started as `vesper` (Bazel's `//:vesper_shipped`)
//!   or built with the `gated-default` feature defaults to `gated`
//!   instead: that build is the one the default `vesper` switches to, and
//!   it needs no environment variable (docs/maintenance/rust-exit-plan.md,
//!   "The default switch"). The variables below still override it.
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
//! Each swap's row is in its own file under `src/swap/` (its `SWAPS`). A
//! swap is promoted by changing its row to [`Gate::Pass`] in the PR that
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

pub(crate) const fn open(name: &'static str, why: &'static str) -> Swap {
    Swap {
        name,
        gate: Gate::Open(why),
    }
}

pub(crate) const fn pass(name: &'static str, config: &'static str, pr: &'static str) -> Swap {
    Swap {
        name,
        gate: Gate::Pass { config, pr },
    }
}

/// Every swap `install` can install: each `src/swap/` file's `SWAPS`
/// rows, in file-name order.
pub(crate) fn all() -> impl Iterator<Item = &'static Swap> {
    super::SWAP_FILES.iter().flat_map(|f| f.swaps)
}

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

/// The file name (without `.exe`) the hybrid is shipped under as the
/// default compiler (docs/maintenance/rust-exit-plan.md, "The default
/// switch"); the port lanes run it as `vesper-hybrid`.
const SHIPPED_NAME: &str = "vesper";

/// The set when neither variable names one: `gated` for the build the
/// default `vesper` switches to, that is a hybrid started as `vesper`
/// (Bazel's `//:vesper_shipped`) or built with the
/// `gated-default` feature; else `all`.
fn default_set() -> Set {
    let shipped = std::env::args_os().next().is_some_and(|arg0| {
        std::path::Path::new(&arg0)
            .file_stem()
            .is_some_and(|stem| stem == SHIPPED_NAME)
    });
    if shipped || cfg!(feature = "gated-default") {
        Set::Gated
    } else {
        Set::All
    }
}

/// Reads the set from the environment, or explains a value it does not
/// know.
fn read() -> Result<Set, String> {
    match std::env::var("VESPER_HYBRID_SWAPS") {
        Ok(v) => match v.as_str() {
            "" => Ok(default_set()),
            "all" => Ok(Set::All),
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
                Ok(default_set())
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
/// [`all`].
pub(crate) fn on(name: &str) -> bool {
    let Some(swap) = all().find(|s| s.name == name) else {
        eprintln!("vesper-hybrid: `{name}` is not a row of any src/swap/ file's SWAPS");
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
    let n = all().filter(|s| on(s.name)).count();
    eprintln!(
        "vesper-hybrid: swaps ({}): {n} of {} on",
        set.name(),
        all().count()
    );
    for s in all() {
        let state = if on(s.name) { "on" } else { "off" };
        match s.gate {
            Gate::Pass { config, pr } => eprintln!(
                "vesper-hybrid: swap {}: {state}, flip gate PASS (scripts/ci/flip-gate/{config}.json, {pr})",
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
        let swaps: Vec<_> = all().collect();
        for (i, s) in swaps.iter().enumerate() {
            assert!(
                swaps[..i].iter().all(|t| t.name != s.name),
                "{} is listed twice",
                s.name
            );
        }
    }
}
