//! Executable scalar host subset of design §38. No source macros, general PEG,
//! Protobuf/DER, IPC envelopes, SIMD, constant-time parsing or proof claims.
#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

pub mod arrow;
pub mod framing;
pub mod grammar;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    UnexpectedEof,
    NonCanonical,
    Overflow,
    TrailingBytes,
    OutputTooSmall,
    InputLimit,
    OutputLimit,
    PayloadLimit,
    WorkLimit,
    DepthLimit,
    CountLimit,
    AllocationLimit,
    AllocationFailed,
    Descriptor,
    Alignment,
    Range,
    Bitmap,
    NullCount,
    Aliasing,
    Rule,
    RecursiveRule,
    DuplicateBranch,
    AmbiguousChoice,
    NullableChoice,
    Mismatch,
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
pub type Result<T> = core::result::Result<T, Error>;
pub(crate) fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(Error::new(ErrorKind::Overflow, 0))
}
pub(crate) fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or(Error::new(ErrorKind::Overflow, 0))
}
pub(crate) fn bound(n: usize, max: usize, kind: ErrorKind) -> Result<()> {
    if n > max {
        Err(Error::new(kind, 0))
    } else {
        Ok(())
    }
}
pub(crate) fn charge(remaining: &mut usize, units: usize) -> Result<()> {
    *remaining = remaining
        .checked_sub(units)
        .ok_or(Error::new(ErrorKind::WorkLimit, 0))?;
    Ok(())
}
