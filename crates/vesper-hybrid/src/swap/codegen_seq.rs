// The `seq` hook of `vesper::codegen_swap`, from `modules/codegen_seq`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/codegen_seq.rs"]
mod codegen_seq_binding;

use super::{Registry, Swap, env_off, lend_codegen_op, on, open};

/// `seq` requests (`ChunkSeq`, `Slice`) the Vesper module emitted, and declined.
static SEQ_CALLS: AtomicU64 = AtomicU64::new(0);
static SEQ_DECLINED: AtomicU64 = AtomicU64::new(0);

/// The `seq` hook of `vesper::codegen_swap` from `modules/codegen_seq` (#1283).
fn vesper_seq(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    lend_codegen_op(
        lent,
        "seq",
        codegen_seq_binding::seq,
        &SEQ_CALLS,
        &SEQ_DECLINED,
    )
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("seq", "0 diffs; no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    if on("seq") && !env_off("VESPER_HYBRID_SEQ") {
        r.codegen.seq = Some(vesper_seq);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: codegen emitted {} seq requests from Vesper ({} declined)",
        SEQ_CALLS.load(Relaxed),
        SEQ_DECLINED.load(Relaxed),
    );
}
