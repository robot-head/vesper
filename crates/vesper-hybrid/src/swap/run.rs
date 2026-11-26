// The `vesper run` evaluator (Stage 2 retirement, #1397; claim #2053), from
// `modules/run`, behind the Vesper driver's `Run` step
// (`driver/native.rs`). A program the Vesper machine declines (it runs
// integers, data, calls, patterns, `?` and handlers that do not resume,
// not closures, resumed continuations, sequences or floats) runs on Rust's `Runtime`, the default compiler's
// path.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/run.rs"]
mod run_binding;

// The evaluator's encoder and decoder (#2053), shared with the root
// package's tests/self_host_run.rs.
#[path = "../../../../tests/common/run_words.rs"]
mod run_words;

// The word encoders read the shared ones through `super::`.
use super::{Registry, Swap, env_off, ir_words, on, open};

/// Programs the Vesper machine evaluated.
static RUN_CALLS: AtomicU64 = AtomicU64::new(0);
/// Programs it declined, which Rust's evaluator ran.
static RUN_DECLINED: AtomicU64 = AtomicU64::new(0);

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open(
    "run",
    "0.146x over 144 programs (tests/self_host_run.rs); no flip-gate configuration",
)];

/// The driver's hook, not a registry slot.
pub(super) fn register(_: &mut Registry) {}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: run evaluated {} programs from Vesper ({} declined)",
        RUN_CALLS.load(Relaxed),
        RUN_DECLINED.load(Relaxed),
    );
}

/// `Runtime::run(entry, [])` with `fuel` and `optimize`, from Vesper: the
/// value's `Display` text or the error, or `None` when the swap is off
/// (`VESPER_HYBRID_RUN=0`) or the Vesper machine declines the program,
/// and the caller runs Rust's evaluator.
pub(crate) fn driver_run(
    world: &vesper::check::World,
    entry: &str,
    fuel: u64,
    optimize: bool,
) -> Option<Result<String, String>> {
    if !on("run") || env_off("VESPER_HYBRID_RUN") {
        return None;
    }
    let declined = || {
        RUN_DECLINED.fetch_add(1, Relaxed);
        None
    };
    if run_words::world_declines(world).is_some() {
        return declined();
    }
    // An optimizer error is Rust's to report.
    let Ok(program) = run_words::program(world, optimize) else {
        return declined();
    };
    let Some(input) = run_words::run_input(world, &program, entry, fuel) else {
        return declined();
    };
    match run_words::decode(&run_binding::eval_words(&input)) {
        run_words::Answer::Value(text) => {
            RUN_CALLS.fetch_add(1, Relaxed);
            Some(Ok(text))
        }
        run_words::Answer::Error(message) => {
            RUN_CALLS.fetch_add(1, Relaxed);
            Some(Err(message))
        }
        run_words::Answer::Declined(_) => declined(),
    }
}
