// The `control` hook of `vesper::codegen_swap`, from `modules/codegen_control`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/codegen_control.rs"]
mod codegen_control_binding;

use super::{Registry, Swap, env_off, lend_codegen_op, on, open};

/// `control` requests (closures, handlers, effect calls) the Vesper module emitted, and declined.
static CONTROL_CALLS: AtomicU64 = AtomicU64::new(0);
static CONTROL_DECLINED: AtomicU64 = AtomicU64::new(0);

/// The `control` hook of `vesper::codegen_swap` from `modules/codegen_control` (#1284).
fn vesper_control(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    lend_codegen_op(
        lent,
        "control",
        codegen_control_binding::control,
        &CONTROL_CALLS,
        &CONTROL_DECLINED,
    )
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("control", "0 declined; no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    if on("control") && !env_off("VESPER_HYBRID_CONTROL") {
        r.codegen.control = Some(vesper_control);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: codegen emitted {} control requests from Vesper ({} declined)",
        CONTROL_CALLS.load(Relaxed),
        CONTROL_DECLINED.load(Relaxed),
    );
}
