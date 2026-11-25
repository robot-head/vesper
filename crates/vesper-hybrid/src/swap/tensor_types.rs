// The tensor type checker (SH2 lane 14, #1186), from `modules/tensor_types`.


#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/tensor_types.rs"]
mod tensor_types_binding;

// The tensor type checker's AST encoder and diagnostic decoder (#1186).
#[path = "../../../../tests/common/tensor_words.rs"]
mod tensor_words;

use super::{Registry, Swap, must, on, open};

/// `tensor_types::check` from `modules/tensor_types` (SH2 lane 14, `#1186`).
const VESPER_TENSOR_CHECK: vesper::tensor_types::TensorCheckTable =
    vesper::tensor_types::TensorCheckTable {
        diagnostics: |module| {
            let (names, bytes) = tensor_words::encode_tensor_module(module);
            let words = tensor_types_binding::tensor_diags(&names, &bytes);
            Some(tensor_words::decode_tensor_diags(&words))
        },
    };

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open(
    "tensor_check",
    "0.28-0.49x; no flip-gate configuration",
)];

pub(super) fn register(_r: &mut Registry) {
    if on("tensor_check") {
        must(vesper::tensor_types::install_tensor_check(
            VESPER_TENSOR_CHECK,
        ));
    }
}

pub(super) fn report() {}
