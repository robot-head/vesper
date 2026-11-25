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
/// `vesper build --target bpf` runs the Vesper driver encoded for building
/// in Vesper (#1895), and how many of those fell back to the Rust pass.
pub(crate) static DRIVER_EBPF_INPUTS: AtomicU64 = AtomicU64::new(0);
pub(crate) static DRIVER_EBPF_FALLBACKS: AtomicU64 = AtomicU64::new(0);

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
/// The `ebpf` swap's input for `vesper build --target bpf`, which the Vesper
/// driver compiles, verifies and writes itself (#1895): the source parsed
/// and checked as `ebpf_backend::compile_source` does. `None` when the swap
/// is off, the source has errors or the world does not encode (the Rust
/// step builds it and reports them). `VESPER_HYBRID_DRIVER_EBPF=0` keeps
/// the build on the Rust step.
pub(crate) fn ebpf_build_input(input: &str, source: &str, entry: &str) -> Option<Vec<u8>> {
    vesper::codegen_swap::installed().ebpf?;
    if std::env::var_os("VESPER_HYBRID_DRIVER_EBPF").is_some_and(|v| v == "0") {
        return None;
    }
    let options = vesper::ebpf_backend::Options {
        entry: entry.to_owned(),
        file: std::path::Path::new(input)
            .file_name()
            .map_or_else(|| input.to_owned(), |f| f.to_string_lossy().into_owned()),
        policy: Default::default(),
    };
    let (source, parsed) = vesper::forge_frontend::parse_compiler_source(source, false).ok()?;
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let checked = vesper::check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return None;
    }
    ebpf_words::input(&checked.world, Some(&source), &options)
}

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
    eprintln!(
        "vesper-hybrid: driver built {} eBPF objects in Vesper ({} fell back to Rust)",
        DRIVER_EBPF_INPUTS.load(Relaxed) - DRIVER_EBPF_FALLBACKS.load(Relaxed),
        DRIVER_EBPF_FALLBACKS.load(Relaxed)
    );
}
