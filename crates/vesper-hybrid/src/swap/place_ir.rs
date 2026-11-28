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

// The input of `@vesper/place-ir-lower` (#1252), which the Vesper driver's
// `vesper emit --format ir` lowers (#1894); shared with the root package's
// tests/self_host_ir_lower.rs. It reads the shared encoders through
// `super::`.
#[path = "../../../../tests/common/ir_lower_words.rs"]
mod ir_lower_words;

use super::check_words;
use super::{Registry, Swap, env_off, ir_words, on, open};

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

/// The identity round trip after a Vesper IR pass (`ir_opt`, `inline`),
/// part of the `ir_lower` row: with that row off (the `gated` set), the
/// passes hand their program on as they decoded it (#2207).
pub(super) fn round_trip_after_pass(
    world: Option<&vesper::check::World>,
    program: &mut vesper::ir::Program,
) {
    if on("ir_lower") {
        round_trip(world, program);
    }
}

/// The lowering input of the Vesper driver's `vesper emit --format ir`
/// (`emit_ir_json` in modules/driver/src/ir.ves, #1894): check-data's
/// names, types and world, the lowering section of `ir_lower_words`, then
/// `read_json_extras`' table of the JSON Rust's `serde_json` writes for
/// each closure and handler expression (by payload index) and for each
/// float literal. `None` leaves the pass to Rust: a literal type past 64
/// bits, an expression `serde_json` refuses, or
/// `VESPER_HYBRID_DRIVER_IR=0` (the baseline).
pub(crate) fn driver_ir_lower_input(world: &vesper::check::World) -> Option<Vec<u8>> {
    if env_off("VESPER_HYBRID_DRIVER_IR") {
        return None;
    }
    let mut enc = check_words::Enc::default();
    let mut lw = ir_lower_words::Lw::new(&mut enc, world);
    let mut tail = lw.section(world);
    let pool = std::mem::take(&mut lw.pool);
    let floats = std::mem::take(&mut lw.floats);
    let mut exprs = vec![];
    for (i, item) in pool.items.iter().enumerate() {
        if let ir_words::Payload::Expr(e) = item {
            let json = serde_json::to_string_pretty(e).ok()?;
            exprs.push((i as u64, enc.name(&json)));
        }
    }
    tail.push(exprs.len() as u64);
    for (i, sym) in exprs {
        tail.extend([i, sym]);
    }
    tail.push(floats.len() as u64);
    for bits in floats {
        let json = serde_json::to_string(&f64::from_bits(bits)).ok()?;
        tail.extend([bits, enc.name(&json)]);
    }
    let world_words = enc.world(world);
    if enc.unfit {
        return None;
    }
    Some(enc.finish(&world_words, &tail))
}

/// The flip-gate rows of this file's swaps (`swaps::on`). `ir_lower` is
/// Rust's lowering followed by the identity round trip, so it replaces no
/// Rust and only adds the crossing; it is not gated (#2207). The 0.40x is
/// the Vesper lowering of tests/self_host_ir_lower.rs, which no swap runs.
pub(super) const SWAPS: &[Swap] = &[open(
    "ir_lower",
    "identity round trip after Rust's lowering, replaces no Rust; not a gate candidate",
)];

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
