// The eBPF backend (SH4 lane 16, #1290), from `modules/ebpf_backend`,
// through the `ebpf` hook.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/ebpf_backend.rs"]
mod ebpf_backend_binding;

// The eBPF backend swap's input encoder and decoder (#1290), shared with
// tests/self_host_ebpf_backend.rs.
#[path = "../../../../tests/common/ebpf_words.rs"]
mod ebpf_words;

// The word encoders read the shared ones through `super::`.
use super::{Registry, Swap, check_words, env_off, ir_words, on, open};

/// Programs the Vesper eBPF backend compiled, and left to Rust.
static EBPF_CALLS: AtomicU64 = AtomicU64::new(0);
static EBPF_DECLINED: AtomicU64 = AtomicU64::new(0);

/// The `ebpf` hook of `vesper::codegen_swap` from `modules/ebpf_backend`
/// (#1290): `@vesper/ebpf-backend` compiles the world from Place IR and
/// returns the assembled program or typed error. Worlds whose literals or
/// Place IR ops are not encodable decline to Rust.
fn vesper_ebpf(
    world: &vesper::check::World,
    source: Option<&str>,
    options: &vesper::ebpf_backend::Options,
) -> Option<Result<vesper::ebpf_backend::Compiled, vesper::ebpf_backend::EbpfBackendError>> {
    let Some(bytes) = ebpf_words::input(world, source, options) else {
        EBPF_DECLINED.fetch_add(1, Relaxed);
        return None;
    };
    let words = ebpf_backend_binding::compile_words(&bytes);
    let result = ebpf_words::decode(&words, options);
    EBPF_CALLS.fetch_add(1, Relaxed);
    Some(result)
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("ebpf", "0.93-1.10x; no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    if on("ebpf") && !env_off("VESPER_HYBRID_EBPF") {
        r.codegen.ebpf = Some(vesper_ebpf);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: eBPF backend compiled {} programs from Vesper ({} declined)",
        EBPF_CALLS.load(Relaxed),
        EBPF_DECLINED.load(Relaxed),
    );
}
