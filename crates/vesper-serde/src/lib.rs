//! Bounded host codec foundations for design section 20.
//!
//! An [`IndexedJson`] validates the *entire* immutable input before exposing raw
//! views or constant-time subtree skips. Duplicate keys are rejected by decoded
//! Unicode scalar equality, including escaped spellings, in every object.
//! Unescaped strings borrow input; escaped strings use explicit caller scratch.
//! It is built either by scalar recursive descent or by the two-stage §20.3
//! path: a SIMD structural index ([`simd`]) then a tape builder over it
//! ([`IndexedJson::parse_simd`], [`SimdParser`]). There are no source-language,
//! arena, or linear-resource claims.
// `unsafe` is denied everywhere except the gated `simd::kernels` module.
#![deny(unsafe_code)]
#![doc = include_str!("../README.md")]

pub mod archive;
pub mod binary;
pub mod event_columns;
pub mod ndjson;
/// Correctly rounded decimal to binary64 conversion (Eisel–Lemire).
pub mod number {
    pub use crate::lemire::{DecodePath, decode_f64};
}
pub mod simd;
pub mod streamvbyte;
pub mod tiles;

mod encode;
mod float;
mod lemire;
mod pow5;
mod helpers;
mod index;
mod stage2;

pub use encode::{EncodeLimits, Value, encode_into, encoded_size};
pub use float::FloatLimits;
pub use helpers::{JsonSizeBound, SwarFieldMatcher, compute_escaped_mask_64};
pub use index::{Cursor, IndexedJson, JsonStr, JsonString, Kind, Limits, Node, ObjectFields};
pub use stage2::SimdParser;

/// A stable, allocation-free error category. Offsets are input byte positions
/// for parsing, and zero for encoder/helper errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Syntax,
    Utf8,
    Escape,
    Number,
    DuplicateKey,
    InputLimit,
    DepthLimit,
    TokenLimit,
    StringLimit,
    WorkLimit,
    AllocationLimit,
    AllocationFailed,
    ScratchTooSmall,
    OutputTooSmall,
    OutputLimit,
    Overflow,
    Type,
    End,
    /// The requested stage 1 instruction-set path is not available here.
    IsaUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub offset: usize,
}
impl Error {
    pub(crate) const fn new(kind: ErrorKind, offset: usize) -> Self {
        Self { kind, offset }
    }
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?} at byte {}", self.kind, self.offset)
    }
}
impl std::error::Error for Error {}

pub(crate) fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b).ok_or(Error::new(ErrorKind::Overflow, 0))
}
