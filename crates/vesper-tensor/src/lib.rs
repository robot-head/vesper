//! Independent host planning/execution slice of design §28.
//!
//! [`shape`] and [`host`] hold the bounded dimension planner and the small
//! FP32/Q4_0 graph executor. [`kv`] is the paged K/V pool, [`ops`],
//! [`quant`] and [`attention`] the decoder kernels, and [`llama`] end-to-end
//! greedy CPU inference for llama2.c-format decoder-only checkpoints.
//!
//! No compiler integration, egglog, SIMD, GPU/device execution or
//! floating-point reassociation guarantee is implied. Fused kernels are written
//! by hand (see [`ops::swiglu_matvec`] and [`attention::AttentionMode::Online`]).
#![forbid(unsafe_code)]

pub mod attention;
pub mod host;
pub mod kv;
pub mod llama;
pub mod ops;
pub mod quant;
pub mod shape;

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
