// The tensor type checker (SH2 lane 14, #1186), from `modules/tensor_types`.


#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/tensor_types.rs"]
mod tensor_types_binding;

// The tensor type checker's AST encoder and diagnostic decoder (#1186).
#[path = "../../../../tests/common/tensor_words.rs"]
mod tensor_words;

use super::{Registry, Swap, must, on, pass};

/// `tensor_types::check` from `modules/tensor_types` (SH2 lane 14, `#1186`).
const VESPER_TENSOR_CHECK: vesper::tensor_types::TensorCheckTable =
    vesper::tensor_types::TensorCheckTable {
        diagnostics: |module| {
            // A module that names no tensor type, effect or wire is
            // inactive (`tensor_types::active`, a walk over its names):
            // screened before the encoding, which costs more than Rust's
            // whole answer for such a module (flip gate, #2289). Encoded
            // modules are screened again by the Vesper side.
            if !vesper::tensor_types::active(module) {
                return Some(None);
            }
            let (names, bytes) = tensor_words::encode_tensor_module(module);
            let words = tensor_types_binding::tensor_diags(&names, &bytes);
            Some(tensor_words::decode_tensor_diags(&words))
        },
    };

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[pass("tensor_check", "tensor-check", "#2289")];

pub(super) fn register(_r: &mut Registry) {
    if on("tensor_check") {
        must(vesper::tensor_types::install_tensor_check(
            VESPER_TENSOR_CHECK,
        ));
    }
}

pub(super) fn report() {}
