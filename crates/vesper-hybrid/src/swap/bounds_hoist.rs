// Bounds hoisting (SH3 lane 12, #1251), from `modules/bounds_hoist`,
// through `ir_swap`'s `hoist` hook.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/bounds_hoist.rs"]
mod bounds_hoist_binding;

use super::{Registry, Swap, ir_words, on, pass};

/// Programs `modules/bounds_hoist` hoisted.
static BOUNDS_HOIST_CALLS: AtomicU64 = AtomicU64::new(0);

/// The loops of `program` behind a bounds guard: branches on a
/// `SeqOp::BoundsGuard` result of their own block, one per versioned loop
/// (the count `bounds_hoist::run` returns, which no caller reads).
fn versioned_loops(program: &vesper::ir::Program) -> usize {
    use vesper::ir::{Op, Terminator};
    use vesper::seq::SeqOp;
    program
        .functions
        .values()
        .flat_map(|f| &f.blocks)
        .filter(|b| {
            let Terminator::Branch { condition, .. } = b.terminator else {
                return false;
            };
            b.instructions.iter().any(|i| {
                i.dest == condition
                    && matches!(
                        i.op,
                        Op::Seq {
                            op: SeqOp::BoundsGuard { .. },
                            ..
                        }
                    )
            })
        })
        .count()
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[pass("hoist", "bounds-hoist", "#1251")];

pub(super) fn register(r: &mut Registry) {
    if on("hoist") {
        r.ir.hoist = Some(|program| {
            let Some(p) =
                ir_words::through_vesper(None, program, bounds_hoist_binding::hoist_words)
            else {
                return vesper::bounds_hoist::run(program);
            };
            BOUNDS_HOIST_CALLS.fetch_add(1, Relaxed);
            let n = versioned_loops(&p).saturating_sub(versioned_loops(program));
            *program = p;
            n
        });
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: bounds-hoist hoisted {} programs in Vesper",
        BOUNDS_HOIST_CALLS.load(Relaxed),
    );
}
