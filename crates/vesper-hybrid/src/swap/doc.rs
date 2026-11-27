// The doc renderers (SH5 lane 15, #1338), from `modules/doc`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/doc.rs"]
mod doc_binding;

use super::{Registry, Swap, must, on, pass};

/// JSON texts and pages rendered by the Vesper doc module.
static DOC_CALLS: AtomicU64 = AtomicU64::new(0);

/// `vesper_doc::swap::RenderTable` from `modules/doc`: the JSON writer and
/// the static and linked pages of `@vesper/doc`.
const VESPER_DOC: vesper_doc::swap::RenderTable = vesper_doc::swap::RenderTable {
    pretty_json: |request| {
        DOC_CALLS.fetch_add(1, Relaxed);
        doc_binding::doc_pretty_json(request)
    },
    static_page: |request| {
        DOC_CALLS.fetch_add(1, Relaxed);
        doc_binding::doc_static_page(request)
    },
    linked_page: |request| {
        DOC_CALLS.fetch_add(1, Relaxed);
        doc_binding::doc_linked_page(request)
    },
};

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[pass("doc", "doc", "#2207")];

pub(super) fn register(_r: &mut Registry) {
    // The request layout the Rust encoder writes must be the module's.
    if on("doc") {
        if doc_binding::doc_format() != vesper_doc::swap::FORMAT {
            std::process::abort();
        }
        must(vesper_doc::swap::install_render(VESPER_DOC));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: doc answered {} calls from Vesper",
        DOC_CALLS.load(Relaxed),
    );
}
