// The `numeric` hook of `vesper::codegen_swap`, from `modules/codegen_numeric`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/codegen_numeric.rs"]
mod codegen_numeric_binding;

use super::{Registry, Swap, lend_codegen_op, on, open};

/// `numeric` requests (vectors, `TiledSeq`) the Vesper module emitted, and
/// declined.
static NUMERIC_CALLS: AtomicU64 = AtomicU64::new(0);
static NUMERIC_DECLINED: AtomicU64 = AtomicU64::new(0);

/// The `numeric` hook of `vesper::codegen_swap` from
/// `modules/codegen_numeric` (#1285): vector and `TiledSeq` operations
/// emitted in Vesper. The graph walk's `INSTRUCTION` requests are declined
/// here, without crossing: the Rust family code then sends the typed
/// requests of `codegen_swap::numeric`, which the Vesper hook answers.
fn vesper_numeric(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    lend_codegen_op(
        lent,
        "numeric",
        codegen_numeric_binding::numeric,
        &NUMERIC_CALLS,
        &NUMERIC_DECLINED,
    )
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("numeric", "0 declined; no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    if on("numeric") {
        r.codegen.numeric = Some(vesper_numeric);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: codegen emitted {} numeric requests from Vesper ({} declined)",
        NUMERIC_CALLS.load(Relaxed),
        NUMERIC_DECLINED.load(Relaxed),
    );
}
