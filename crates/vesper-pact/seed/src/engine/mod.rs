//! The Rust implementation of validation, solving, coherence,
//! canonicalization and derivation checking, frozen for the compiler's
//! closure (bug fixes only until self-hosting Stage 2). The package
//! vesper-pact runs the Vesper port instead.

pub(crate) mod canon;
pub(crate) mod coherence;
pub(crate) mod derivation;
pub(crate) mod infer;
pub(crate) mod mode;
pub(crate) mod solve;
pub(crate) mod validate;
