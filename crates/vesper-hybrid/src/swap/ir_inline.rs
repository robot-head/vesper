// The inliner (SH3 lane 11, #1250), from `modules/ir_inline`, through
// `ir_swap`'s `inline` hook.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/ir_inline.rs"]
mod ir_inline_binding;

// The inliner swap's encoder and decoder (#1250), shared with the root
// package's tests/self_host_ir_inline.rs.
#[path = "../../../../tests/common/inline_words.rs"]
mod inline_words;

use super::place_ir::round_trip_after_pass;
use super::{Registry, Swap, on, open};

/// Programs inlined by the Vesper inliner module.
static IR_INLINE_CALLS: AtomicU64 = AtomicU64::new(0);

/// `ir_swap`'s `inline` hook from `modules/ir_inline`: the inliner of
/// `@vesper/ir-inline`. The package decides and splices; the hook encodes
/// the world's answers and the program, and reads the report and the
/// inlined program back (`inline_words::decode_program`).
fn vesper_inline(
    world: &vesper::check::World,
    program: &mut vesper::ir::Program,
    budget: &vesper::ir_inline::Budget,
) -> vesper::ir_inline::Report {
    let input = inline_words::inline_input(world, program, budget);
    IR_INLINE_CALLS.fetch_add(1, Relaxed);
    let words = ir_inline_binding::inline_words(&input.bytes);
    let (report, inlined) = inline_words::decode_program(&words, &input.tables, program);
    *program = inlined;
    report
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("inline", "0.76x; no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    if on("inline") {
        r.ir.inline = Some(|world, program, budget| {
            let report = vesper_inline(world, program, budget);
            round_trip_after_pass(Some(world), program);
            report
        });
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: ir-inline answered {} calls from Vesper",
        IR_INLINE_CALLS.load(Relaxed),
    );
}
