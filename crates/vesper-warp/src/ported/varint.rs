//! [`crate::varint`]'s ported functions, forwarded to `vesper/src/varint.ves`.

use super::binding as b;
use crate::varint::VarIntError;

pub(crate) fn varint_error(e: b::VarIntErr) -> VarIntError {
    match e {
        b::VarIntErr::Truncated { needed, available } => VarIntError::Truncated {
            needed: needed as usize,
            available: available as usize,
        },
        b::VarIntErr::TooLarge { value } => VarIntError::TooLarge(value),
        b::VarIntErr::BufferTooSmall { needed, available } => VarIntError::BufferTooSmall {
            needed: needed as usize,
            available: available as usize,
        },
        b::VarIntErr::BadLength { len } => VarIntError::BadLength(len as usize),
    }
}

/// Encoded length announced by the two-bit prefix of `first`.
pub fn prefix_len(first: u8) -> usize {
    b::varint_prefix_len(first) as usize
}

/// Shortest encoding length of `value`.
pub fn encoded_len(value: u64) -> Result<usize, VarIntError> {
    b::varint_encoded_len(value)
        .map(|n| n as usize)
        .map_err(varint_error)
}

/// Decodes one varint from the front of `buf`, returning it and its length.
pub fn decode(buf: &[u8]) -> Result<(u64, usize), VarIntError> {
    b::varint_decode(buf)
        .map(|(v, n)| (v, n as usize))
        .map_err(varint_error)
}

/// Encodes `value` in exactly `len` bytes (1, 2, 4 or 8).
pub fn encode_with_len(value: u64, len: usize, out: &mut [u8]) -> Result<usize, VarIntError> {
    b::varint_encode_with_len(value, len as u64, out)
        .map(|n| n as usize)
        .map_err(varint_error)
}

/// Encodes `value` in its shortest form.
pub fn encode(value: u64, out: &mut [u8]) -> Result<usize, VarIntError> {
    b::varint_encode(value, out)
        .map(|n| n as usize)
        .map_err(varint_error)
}
