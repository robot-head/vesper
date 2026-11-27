// Freestanding stubs, linker, trap table and scripts (SH4 lane 14, #1288),
// from `modules/freestanding`, through the `freestanding` hook.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/freestanding.rs"]
mod freestanding_binding;

// The freestanding swap's encoder and decoder (#1288), shared with
// tests/self_host_freestanding.rs.
#[path = "../../../../tests/common/freestanding_words.rs"]
mod freestanding_words;

use super::{Registry, Swap, env_off, on, pass};

/// Freestanding images whose stub, linker, trap table and script ran in Vesper.
static FREESTANDING_CALLS: AtomicU64 = AtomicU64::new(0);

/// The `freestanding` hook of `vesper::codegen_swap` from `modules/freestanding`
/// (#1288): `@vesper/codegen-freestanding` runs `compare_code`, `stub`, `link`
/// (with trap resolution and trap-table patching) and `script`.
fn vesper_freestanding(
    world: &vesper::check::World,
    entry: &str,
    options: &vesper::native::freestanding::ImageOptions,
) -> Option<
    Result<
        vesper::native::freestanding::FreestandingImage,
        vesper::native::freestanding::FreestandingError,
    >,
> {
    use vesper::native::freestanding::{FreestandingOps, LinkedImage, build_image_with};
    let ops = FreestandingOps {
        compare_code: &|object, symbol, code| {
            let req = freestanding_words::compare_code_request(object, symbol, code);
            let out = freestanding_binding::compare_code_words(&req.0);
            freestanding_words::decode_compare_code(&out)
        },
        stub: &|target, board, e_flags| {
            let req = freestanding_words::stub_request(target, board, e_flags);
            let out = freestanding_binding::stub_words(&req.0);
            freestanding_words::decode_stub(&out)
        },
        link: &|objects, link_options, traps| {
            let req = freestanding_words::link_request(objects, link_options, traps);
            let out = freestanding_binding::link_words(&req.0);
            let linked = freestanding_words::decode_link(&out)?;
            Ok(LinkedImage {
                entry: linked.entry,
                layout: linked.layout,
                symbols: linked.symbols,
                pcs: linked.pcs,
                elf: linked.elf,
            })
        },
        script: &|target, board, stack_bytes| {
            let req = freestanding_words::script_request(target, board, stack_bytes);
            let out = freestanding_binding::script_words(&req.0);
            freestanding_words::decode_script(&out)
        },
    };
    FREESTANDING_CALLS.fetch_add(1, Relaxed);
    Some(build_image_with(world, entry, options, &ops))
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[pass("freestanding", "freestanding", "#2517")];

pub(super) fn register(r: &mut Registry) {
    if on("freestanding") && !env_off("VESPER_HYBRID_FREESTANDING") {
        r.codegen.freestanding = Some(vesper_freestanding);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: freestanding built {} images from Vesper",
        FREESTANDING_CALLS.load(Relaxed),
    );
}
