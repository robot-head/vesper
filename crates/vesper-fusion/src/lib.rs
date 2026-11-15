//! Host-library operator fusion on `egglog` (design §28.1 item 4, §28.5,
//! §32.1 item 4, §7 Stage 4).
//!
//! Two rule sets run on the same bounded equality-saturation engine
//! ([`engine`]):
//!
//! * [`tensor`]: Tensor dataflow graphs. Elementwise chains collapse into one
//!   per-lane kernel, matrix-vector products become the leaf of their
//!   epilogue (matmul + bias + activation is one pass), quantized weights are
//!   dequantized inside the consuming kernel instead of being materialized,
//!   and an RMSNorm prologue folds into the matvec that reads it.
//! * [`dsp`]: Resonance stereo chains. Gain and pan stages compose into one
//!   per-channel gain, biquad sections join a single cascade run sample by
//!   sample, a uniform gain folds into the first section's feed-forward
//!   coefficients, and parallel gain sends of one source merge.
//!
//! Each rule is named, and `lean/FusionRules.lean` holds one refinement lemma
//! per rule name over a model of the operators (see [`RULE_LEMMAS`]). Rules
//! that do not reassociate are proved over an arbitrary carrier with arbitrary
//! operations, so they also describe IEEE `f32` evaluation order; rules that
//! reassociate are proved over exact rationals only and are checked against
//! the unfused graph within a stated float tolerance by the tests.
//!
//! This is a host library, not the compiler's Stage 4 optimizer (`src/ir_opt`
//! and the rewrite-witness work are separate): nothing here reads Vesper
//! source, and Rust ownership in this crate is not evidence that Vesper's
//! source typing rules are implemented.
#![forbid(unsafe_code)]

pub mod dsp;
pub mod engine;
pub mod tensor;

/// Fusion errors. Malformed, over-budget and non-finite inputs are reported
/// here rather than panicking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A graph or parameter exceeded a fixed budget.
    Limit(&'static str),
    /// A name is empty, too long or not `[A-Za-z0-9_]`.
    InvalidName(String),
    /// A node handle belongs to another graph or is out of range.
    InvalidNode,
    /// Two operands have incompatible lengths or element types.
    Shape(&'static str),
    /// An operation is not defined for the operand element type.
    Unsupported(&'static str),
    /// A required named input or matrix is missing from the environment.
    Missing(String),
    /// An input or a computed value is NaN or infinite.
    NonFinite,
    /// Checked integer arithmetic overflowed.
    Overflow,
    /// A parameter is outside its domain (epsilon, pan position, coefficients).
    Parameter(&'static str),
    /// egglog rejected the program or extraction failed.
    Egglog(String),
    /// The extracted term does not match the rule set's grammar.
    Extraction(String),
    /// An error from `vesper-tensor`.
    Tensor(vesper_tensor::Error),
    /// An error from the Resonance PDC scheduler.
    Schedule(vesper_resonance::pdc::PdcError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

impl From<vesper_tensor::Error> for Error {
    fn from(e: vesper_tensor::Error) -> Self {
        match e {
            vesper_tensor::Error::NonFinite => Error::NonFinite,
            other => Error::Tensor(other),
        }
    }
}

/// Maximum nodes in one input graph.
pub const MAX_NODES: usize = 1024;
/// Maximum elements in one vector, signal channel or matrix.
pub const MAX_ELEMENTS: usize = 1 << 22;
/// Maximum distinct nodes in an extracted term.
pub const MAX_EXTRACTED: usize = 1 << 16;

/// Rule name and the Lean theorem in `lean/FusionRules.lean` that refines it.
pub const RULE_LEMMAS: &[(&str, &str)] = &[
    ("ew-act", "ew_act_refines"),
    ("ew-add", "ew_add_refines"),
    ("ew-mul", "ew_mul_refines"),
    ("ew-scale", "ew_scale_refines"),
    ("ew-inline", "ew_inline_refines"),
    ("matvec-epilogue", "matvec_epilogue_refines"),
    ("dequant-into-matmul", "dequant_into_matmul_refines"),
    ("rmsnorm-prologue", "rmsnorm_prologue_refines"),
    ("gain-stereo", "gain_stereo_refines"),
    ("pan-stereo", "pan_stereo_refines"),
    ("stereo-compose", "stereo_compose_refines"),
    ("biquad-cascade", "biquad_cascade_refines"),
    ("cascade-merge", "cascade_merge_refines"),
    ("cascade-append-nil", "cascade_append_nil_refines"),
    ("cascade-append-cons", "cascade_append_cons_refines"),
    ("gain-into-biquad", "gain_into_biquad_refines"),
    ("gain-before-biquad", "gain_before_biquad_refines"),
    ("mix-sends", "mix_sends_refines"),
];

pub(crate) fn valid_name(name: &str) -> Result<(), Error> {
    if !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        Ok(())
    } else {
        Err(Error::InvalidName(name.chars().take(64).collect()))
    }
}

// Owner tags keep a node handle from one graph from naming a node of another.
pub(crate) fn next_owner() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
