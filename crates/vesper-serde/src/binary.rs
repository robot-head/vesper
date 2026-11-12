//! Common allocation-free error reporting for the binary host codecs.
use core::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Truncated,
    TrailingBytes,
    OutputTooSmall,
    ByteLimit,
    CountLimit,
    DepthLimit,
    StringLimit,
    WorkLimit,
    AllocationLimit,
    AllocationFailed,
    Overflow,
    Magic,
    Version,
    Schema,
    Alignment,
    Offset,
    Layout,
    Utf8,
    Tree,
}

/// Byte offsets refer to encoded input/fields; preflight errors use offset zero.
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
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
pub(crate) fn work(remaining: &mut usize, units: usize) -> Result<()> {
    *remaining = remaining
        .checked_sub(units)
        .ok_or(Error::new(ErrorKind::WorkLimit, 0))?;
    Ok(())
}
