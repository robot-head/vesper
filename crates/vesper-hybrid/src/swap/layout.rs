// The word arithmetic of the layout pilot (SH0 lane 1, #1123), from
// `modules/layout`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/layout.rs"]
mod layout_binding;

use super::{Registry, Swap, must, on, open};

/// Calls answered by the Vesper word-layout module.
static LAYOUT_CALLS: AtomicU64 = AtomicU64::new(0);

fn count() {
    LAYOUT_CALLS.fetch_add(1, Relaxed);
}

/// `vesper::layout::WordLayout` from `modules/layout`.
const VESPER_WORD_LAYOUT: vesper::layout::WordLayout = vesper::layout::WordLayout {
    first_offset: |is_enum| {
        count();
        layout_binding::first_offset(is_enum)
    },
    next_offset: |offset, size| {
        count();
        let next = layout_binding::next_offset(offset, size);
        (next != layout_binding::no_fit()).then_some(next as i32)
    },
    product_size: |end| {
        count();
        layout_binding::product_size(end)
    },
    enum_size: |size, end| {
        count();
        layout_binding::enum_size(size, end)
    },
    word_offset: |index| {
        count();
        layout_binding::word_offset(index)
    },
};

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("word_layout", "0 diffs; no flip-gate configuration")];

pub(super) fn register(_r: &mut Registry) {
    if on("word_layout") {
        must(vesper::layout::install_word_layout(VESPER_WORD_LAYOUT));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: layout answered {} calls from Vesper",
        LAYOUT_CALLS.load(Relaxed),
    );
}
