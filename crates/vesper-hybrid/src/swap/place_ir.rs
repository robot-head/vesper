// The IR swap point's identity round trip (SH3 lane 1, #1240), from
// `modules/place_ir`: each native IR pass in Rust, then the program
// through `@vesper/place-ir` tables and back, proving the round trip
// inside `vesper check`/`build`. The IR swap files (`ir_opt`, `ir_inline`)
// run their pass and then this round trip; this file fills `lower`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/place_ir.rs"]
mod place_ir_binding;

use super::{Registry, Swap, ir_words, on, open};

/// Programs the IR swap point's identity hooks sent through Vesper.
static PLACE_IR_CALLS: AtomicU64 = AtomicU64::new(0);

/// `program` read into `@vesper/place-ir` tables and written back, in
/// place: the identity Vesper pass. A program the format cannot encode
/// stays as Rust made it.
pub(super) fn round_trip(world: Option<&vesper::check::World>, program: &mut vesper::ir::Program) {
    if let Some(p) = ir_words::through_vesper(world, program, place_ir_binding::round_trip_words) {
        PLACE_IR_CALLS.fetch_add(1, Relaxed);
        *program = p;
    }
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("ir_lower", "0.40x; no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    if on("ir_lower") {
        r.ir.lower = Some(|world| {
            let mut program = vesper::ir::lower_world(world);
            round_trip(Some(world), &mut program);
            program
        });
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: place-ir round-tripped {} programs through Vesper",
        PLACE_IR_CALLS.load(Relaxed),
    );
}
