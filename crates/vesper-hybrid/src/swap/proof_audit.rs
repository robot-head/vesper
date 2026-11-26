// `vesper proof-audit` (SH5 lane 18, #1341), from `proof_audit_run` of
// the driver module (`modules/driver`; its own module until #2091).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::{Registry, Swap, must, on, pass};

/// Audits run by the Vesper proof-audit module.
static PROOF_AUDIT_CALLS: AtomicU64 = AtomicU64::new(0);

fn run(packed: &[u8]) -> Vec<u8> {
    PROOF_AUDIT_CALLS.fetch_add(1, Relaxed);
    crate::driver::proof_audit_run(packed)
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[pass("proof_audit", "proof-audit", "#2207")];

pub(super) fn register(_r: &mut Registry) {
    if on("proof_audit") {
        must(crate::commands::install_proof_audit(run));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: proof_audit answered {} calls from Vesper",
        PROOF_AUDIT_CALLS.load(Relaxed),
    );
}
