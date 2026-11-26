// `vesper verify SOURCE --format` (SH5 lane 18, #1341), from `verify_run`
// of the driver module (`modules/driver`; once its own module).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::{Registry, Swap, must, on, open};

/// `--format` runs answered by the Vesper verify module.
static VERIFY_CALLS: AtomicU64 = AtomicU64::new(0);

fn run(packed: &[u8]) -> Vec<u8> {
    VERIFY_CALLS.fetch_add(1, Relaxed);
    crate::driver::verify_run(packed)
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open(
    "verify",
    "0 of 29 cases differ (tests/self_host_verify_format.rs, 9 with real Lean), 0.28x; no flip-gate configuration",
)];

pub(super) fn register(_r: &mut Registry) {
    if on("verify") {
        must(crate::commands::install_verify(run));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: verify answered {} calls from Vesper",
        VERIFY_CALLS.load(Relaxed),
    );
}
