//! Independent host planning/execution slice of design §28.
//!
//! [`shape`] and [`host`] hold the bounded dimension planner and the small
//! FP32/Q4_0 graph executor. [`kv`] is the paged K/V pool, [`ops`],
//! [`quant`] and [`attention`] the decoder kernels, and [`llama`] end-to-end
//! greedy CPU inference for llama2.c-format decoder-only checkpoints.
//!
//! [`formats`] adds Q4_K, MXFP4 and FP8; [`gguf`] and [`safetensors`] read
//! (and, for tests and conversion, write) those containers, optionally from a
//! read-only memory mapping ([`mmap`]). [`tokenizer`] is llama2.c-compatible
//! BPE prompt encoding, [`sampling`] temperature and top-p sampling, and
//! [`simd`] the `std::arch` dot-product and AXPY kernels with their scalar
//! references. [`llama::Model::forward_batch`] runs batched prefill and
//! several sequences in one forward call.
//!
//! No compiler integration, egglog or GPU/device execution is implied. Fused
//! kernels are written by hand (see [`ops::swiglu_matvec`] and
//! [`attention::AttentionMode::Online`]).
//!
//! `unsafe` is denied crate-wide and allowed only in [`simd`] (`std::arch`
//! intrinsics, each with a scalar reference) and [`mmap`] (the one
//! `memmap2::Mmap::map` call).
#![deny(unsafe_code)]

pub mod attention;
pub mod formats;
pub mod gguf;
pub mod host;
mod json;
pub mod kv;
pub mod llama;
pub mod mmap;
pub mod ops;
pub mod quant;
pub mod safetensors;
pub mod sampling;
pub mod shape;
pub mod simd;
pub mod tokenizer;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Limit(&'static str),
    Overflow,
    InvalidId,
    InvalidName,
    Shape(&'static str),
    Unresolved,
    SchemaMismatch,
    MissingTensor(String),
    NonFinite,
    InvalidEpsilon,
    /// Malformed checkpoint or tokenizer bytes.
    Malformed(&'static str),
    /// A K/V pool operation failed.
    Kv(kv::KvError),
    /// A row length is not a whole number of the format's blocks.
    QuantBlockAlignment {
        cols: usize,
        block: usize,
    },
    /// A container, tensor type or model layout this crate does not support.
    Unsupported(String),
    /// A file could not be opened, read or mapped.
    Io(&'static str),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

// Owner tags prevent a node handle from one arena silently naming another node.
pub(crate) fn next_owner() -> Result<u64, Error> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut current = NEXT.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .ok_or(Error::Limit("arena identities"))?;
        match NEXT.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(current),
            Err(observed) => current = observed,
        }
    }
}
