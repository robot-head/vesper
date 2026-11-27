// The capability effect checker (SH2 lane 8, #1180), from `modules/effects`.


#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/effects.rs"]
mod effects_binding;

// The capability effect checker's encoder and diagnostic decoder (#1180).
#[path = "../../../../tests/common/effects_words.rs"]
mod effects_words;

// The word encoders read the shared ones through `super::`.
use super::{Registry, Swap, check_words, on, pass};

/// `capability_effects::check_world` from `modules/effects` (SH2 lane 8, `#1180`).
const VESPER_EFFECTS_CHECK: vesper::capability_effects::EffectsCheckTable =
    vesper::capability_effects::EffectsCheckTable {
        check_world: |world| {
            let bytes = effects_words::encode_world(world);
            let words = effects_binding::effects_diags(&bytes);
            effects_words::decode_diagnostics(&words)
        },
        capability_policies: |world| {
            let bytes = effects_words::encode_world(world);
            let words = effects_binding::effects_policies(&bytes);
            effects_words::decode_policies(&words)
        },
    };

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[pass("effects", "effects", "#2207")];

pub(super) fn register(_r: &mut Registry) {
    if on("effects") {
        vesper::capability_effects::install_effects_check(VESPER_EFFECTS_CHECK);
    }
}

pub(super) fn report() {}
