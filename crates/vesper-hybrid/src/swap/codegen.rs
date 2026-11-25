// The census lowering family (SH4 lane 1, #1275), from `modules/codegen`,
// through the `aggregate` hook.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/codegen.rs"]
mod codegen_binding;

use super::{Registry, Swap, on, open};

/// `aggregate` requests the Vesper codegen module emitted, and declined.
static CODEGEN_CALLS: AtomicU64 = AtomicU64::new(0);
static CODEGEN_DECLINED: AtomicU64 = AtomicU64::new(0);

/// The `aggregate` hook of `vesper::codegen_swap` from `modules/codegen`:
/// the census lowering family, emitted in Vesper through the lent builder
/// and module (`vesper_cranelift_sys::lend`).
fn vesper_aggregate(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    let kind = if lent.jit {
        vesper_cranelift_sys::ModuleKind::Jit
    } else {
        vesper_cranelift_sys::ModuleKind::Object
    };
    let (answer, crossing) = vesper_cranelift_sys::lend_as(
        kind,
        lent.builder,
        &mut *lent.module,
        lent.op,
        lent.args,
        codegen_binding::aggregate,
    );
    if let Some(e) = crossing.error {
        return Err(format!("codegen (aggregate op {}): {e}", lent.op));
    }
    if crossing.unreturned {
        return Err(format!(
            "codegen (aggregate op {}) kept a lent handle",
            lent.op
        ));
    }
    if answer == 1 {
        CODEGEN_CALLS.fetch_add(1, Relaxed);
        lent.results.extend(crossing.results);
        Ok(true)
    } else {
        CODEGEN_DECLINED.fetch_add(1, Relaxed);
        Ok(false)
    }
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("aggregate", "0 diffs; no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    if on("aggregate") {
        r.codegen.aggregate = Some(vesper_aggregate);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: codegen emitted {} aggregate requests from Vesper ({} declined)",
        CODEGEN_CALLS.load(Relaxed),
        CODEGEN_DECLINED.load(Relaxed),
    );
}
