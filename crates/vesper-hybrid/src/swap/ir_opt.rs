// The IR optimizer (SH3 lane 9, #1248), from `modules/ir_opt`, through
// `ir_swap`'s `opt` hook.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/ir_opt.rs"]
mod ir_opt_binding;

// The IR optimizer swap's encoder and decoder (#1248), shared with the root
// package's tests/self_host_ir_opt.rs.
#[path = "../../../../tests/common/ir_opt_words.rs"]
mod ir_opt_words;

use super::place_ir::round_trip_after_pass;
// The word encoders read the shared ones through `super::`.
use super::{Registry, Swap, ir_words, on, open};

/// Programs optimized by the Vesper IR optimizer module.
static IR_OPT_CALLS: AtomicU64 = AtomicU64::new(0);

/// `ir_swap`'s `opt` hook from `modules/ir_opt`: the native optimizer
/// policy of `@vesper/ir-opt-policy` (its block screen, #1510) around
/// `@vesper/ir-opt` (memory forwarding and `@fip` reuse), then Rust's
/// token threading and IR check on the decoded program
/// (`ir_opt_words::finish`). A program the Place IR words cannot encode,
/// or a block egglog would refuse (the message is egglog's own), stays in
/// Rust's policy, the default compiler's `opt` path.
fn vesper_optimize(
    world: &vesper::check::World,
    program: &mut vesper::ir::Program,
) -> Result<usize, String> {
    let ours = ir_opt_words::optimize_with(world, program, ir_opt_binding::opt_words);
    match ours {
        Some(result) => {
            IR_OPT_CALLS.fetch_add(1, Relaxed);
            result
        }
        None => vesper::ir_opt_policy::optimize(world, program),
    }
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("ir_opt", "0.0036x; no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    if on("ir_opt") {
        r.ir.opt = Some(|world, program| {
            let n = vesper_optimize(world, program)?;
            round_trip_after_pass(Some(world), program);
            Ok(n)
        });
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: ir-opt answered {} calls from Vesper",
        IR_OPT_CALLS.load(Relaxed),
    );
}
