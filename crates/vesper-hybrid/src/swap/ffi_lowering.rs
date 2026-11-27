// The `ffi` hook of `vesper::codegen_swap`, from `modules/ffi_lowering`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/ffi_lowering.rs"]
mod ffi_lowering_binding;

use super::{Registry, Swap, env_off, lend_codegen_op, on, pass};

/// `ffi` requests (`.vsys` foreign calls) the Vesper module emitted, and declined.
static FFI_CALLS: AtomicU64 = AtomicU64::new(0);
static FFI_DECLINED: AtomicU64 = AtomicU64::new(0);

/// The `ffi` hook of `vesper::codegen_swap` from `modules/ffi_lowering` (#1287).
fn vesper_ffi(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    lend_codegen_op(
        lent,
        "ffi",
        ffi_lowering_binding::ffi,
        &FFI_CALLS,
        &FFI_DECLINED,
    )
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[pass("ffi", "ffi-lowering", "#2617")];

pub(super) fn register(r: &mut Registry) {
    if on("ffi") && !env_off("VESPER_HYBRID_FFI") {
        r.codegen.ffi = Some(vesper_ffi);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: codegen emitted {} ffi requests from Vesper ({} declined)",
        FFI_CALLS.load(Relaxed),
        FFI_DECLINED.load(Relaxed),
    );
}
