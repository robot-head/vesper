//! The Rust reference implementation of the ported functions of
//! [`crate::varint`], compiled without `vesper-port` (the Vesper build is
//! `vesper/src/varint.ves`). Frozen: bug fixes only (#1066).

use crate::varint::{MAX, VarIntError};

/// Encoded length announced by the two-bit prefix of `first`.
pub const fn prefix_len(first: u8) -> usize {
    1 << (first >> 6)
}

/// Shortest encoding length of `value`.
pub const fn encoded_len(value: u64) -> Result<usize, VarIntError> {
    if value < 1 << 6 {
        Ok(1)
    } else if value < 1 << 14 {
        Ok(2)
    } else if value < 1 << 30 {
        Ok(4)
    } else if value <= MAX {
        Ok(8)
    } else {
        Err(VarIntError::TooLarge(value))
    }
}

/// Decodes one varint from the front of `buf`, returning it and its length.
pub fn decode(buf: &[u8]) -> Result<(u64, usize), VarIntError> {
    let Some(&first) = buf.first() else {
        return Err(VarIntError::Truncated {
            needed: 1,
            available: 0,
        });
    };
    let len = prefix_len(first);
    let Some(bytes) = buf.get(..len) else {
        return Err(VarIntError::Truncated {
            needed: len,
            available: buf.len(),
        });
    };
    let mut value = u64::from(first & 0x3f);
    for byte in &bytes[1..] {
        value = (value << 8) | u64::from(*byte);
    }
    Ok((value, len))
}

/// Encodes `value` in exactly `len` bytes (1, 2, 4 or 8).
pub fn encode_with_len(value: u64, len: usize, out: &mut [u8]) -> Result<usize, VarIntError> {
    let prefix: u8 = match len {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        _ => return Err(VarIntError::BadLength(len)),
    };
    if value > MAX {
        return Err(VarIntError::TooLarge(value));
    }
    if encoded_len(value)? > len {
        return Err(VarIntError::BadLength(len));
    }
    let Some(dst) = out.get_mut(..len) else {
        return Err(VarIntError::BufferTooSmall {
            needed: len,
            available: out.len(),
        });
    };
    let bytes = value.to_be_bytes();
    dst.copy_from_slice(&bytes[8 - len..]);
    dst[0] |= prefix << 6;
    Ok(len)
}

/// Encodes `value` in its shortest form.
pub fn encode(value: u64, out: &mut [u8]) -> Result<usize, VarIntError> {
    encode_with_len(value, encoded_len(value)?, out)
}
