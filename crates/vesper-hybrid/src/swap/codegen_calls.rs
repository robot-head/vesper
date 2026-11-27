// The call instructions of native lowering (SH4 lane 7, #1281), from
// `modules/codegen_calls`, through the `call` hook.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/codegen_calls.rs"]
mod codegen_calls_binding;

use super::{Registry, Swap, on, pass};

/// `call` requests the Vesper codegen_calls module emitted, and declined.
static CALLS_EMITTED: AtomicU64 = AtomicU64::new(0);
static CALLS_DECLINED: AtomicU64 = AtomicU64::new(0);

/// The `call` hook of `vesper::codegen_swap` from `modules/codegen_calls`:
/// the call instructions of native lowering, emitted in Vesper through the
/// lent builder (#1281).
fn vesper_call(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    let (answer, crossing) = vesper_cranelift_sys::lend(
        lent.builder,
        &mut *lent.module,
        lent.op,
        lent.args,
        codegen_calls_binding::call,
    );
    if let Some(e) = crossing.error {
        return Err(format!("codegen_calls (call op {}): {e}", lent.op));
    }
    if crossing.unreturned {
        return Err(format!(
            "codegen_calls (call op {}) kept a lent handle",
            lent.op
        ));
    }
    if answer == 1 {
        CALLS_EMITTED.fetch_add(1, Relaxed);
        lent.results.extend(crossing.results);
        Ok(true)
    } else {
        CALLS_DECLINED.fetch_add(1, Relaxed);
        Ok(false)
    }
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[pass("call", "codegen-calls", "#2572")];

pub(super) fn register(r: &mut Registry) {
    if on("call") {
        r.codegen.call = Some(vesper_call);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: codegen_calls emitted {} calls from Vesper ({} declined)",
        CALLS_EMITTED.load(Relaxed),
        CALLS_DECLINED.load(Relaxed),
    );
}
