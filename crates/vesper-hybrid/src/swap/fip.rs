// The fip checker (SH2 lane 12, #1184), from `modules/fip`.


#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/fip.rs"]
mod fip_binding;

// The fip checker's encoder and diagnostic decoder (#1184).
#[path = "../../../../tests/common/fip_words.rs"]
mod fip_words;

// The word encoders read the shared ones through `super::`.
use super::{Registry, Swap, check_words, on, open};

/// `fip::check_world` from `modules/fip` (SH2 lane 12, `#1184`).
const VESPER_FIP_CHECK: vesper::fip::FipCheckTable = vesper::fip::FipCheckTable {
    check_world: |world| {
        let bytes = fip_words::encode_world(world);
        let words = fip_binding::fip_diags(&bytes);
        fip_words::decode_diagnostics(&words)
    },
};

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open(
    "fip",
    "flip gate FAIL 1.80x / 1.67x with encoding (scripts/ci/flip-gate/fip.json, #2289)",
)];

pub(super) fn register(_r: &mut Registry) {
    if on("fip") {
        vesper::fip::install_fip_check(VESPER_FIP_CHECK);
    }
}

pub(super) fn report() {}
