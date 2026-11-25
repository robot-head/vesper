// The `drops` hook of `vesper::codegen_swap`, from `modules/codegen_drops`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/codegen_drops.rs"]
mod codegen_drops_binding;

use super::{Registry, Swap, env_off, lend_codegen_op, on, open};

/// `drops` requests (`DROP_BOX`) the Vesper module emitted, and declined.
static DROPS_CALLS: AtomicU64 = AtomicU64::new(0);
static DROPS_DECLINED: AtomicU64 = AtomicU64::new(0);

/// The `drops` hook of `vesper::codegen_swap` from `modules/codegen_drops` (#1282).
fn vesper_drops(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    lend_codegen_op(
        lent,
        "drops",
        codegen_drops_binding::drops,
        &DROPS_CALLS,
        &DROPS_DECLINED,
    )
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("drops", "0 diffs; no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    if on("drops") && !env_off("VESPER_HYBRID_DROPS") {
        r.codegen.drops = Some(vesper_drops);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: codegen emitted {} drops requests from Vesper ({} declined)",
        DROPS_CALLS.load(Relaxed),
        DROPS_DECLINED.load(Relaxed),
    );
}
