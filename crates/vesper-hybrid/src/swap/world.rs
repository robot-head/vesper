// The checked world's declarations (#2024 slice 2c-2), from
// @vesper/check-collect in the Vesper driver: for a standalone source the
// driver parsed itself, its `decls_words` stream is the `WorldDecls` step's
// answer (driver/world.rs), unless the collector declines the module (one
// the checker elaborates before collecting) and the session encodes it.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::{Registry, Swap, env_off, on, open, stats};

/// Worlds whose declarations the Vesper collector answered.
static WORLD_CALLS: AtomicU64 = AtomicU64::new(0);
/// The world swap's costs (`rust` and `differ`: the session's encoding).
static COST: stats::Cost = stats::Cost::new("world");

pub(super) const SWAPS: &[Swap] = &[open(
    "world",
    "WorldDecls from @vesper/check-collect: 0 differ on 302 checked corpus modules (tests/self_host_check_collect.rs); no flip-gate configuration",
)];

/// The driver's hook, not a registry slot.
pub(super) fn register(_: &mut Registry) {}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: world answered {} declaration streams from Vesper",
        WORLD_CALLS.load(Relaxed),
    );
    COST.report();
}

/// Whether the Vesper driver collects a standalone source's declarations
/// itself (`VESPER_HYBRID_WORLD=0` turns it off).
pub(crate) fn driver_world_begin() -> bool {
    on("world") && !env_off("VESPER_HYBRID_WORLD")
}

/// The Vesper driver's `decls` stream for `world`; at stats level 2 the
/// session's encoding (`rust`) is timed and compared with it.
pub(crate) fn driver_world(decls: &[u8], rust: impl FnOnce() -> Vec<u8>) -> Vec<u8> {
    WORLD_CALLS.fetch_add(1, Relaxed);
    COST.call(false, rust, |r| r.as_slice() == decls);
    decls.to_vec()
}
