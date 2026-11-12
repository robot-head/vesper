//! RFC 9000 §16 variable-length integers.
//!
//! Decoding accepts non-minimal encodings, as the RFC requires for values in
//! general (frame *types* are checked for minimality separately in
//! [`crate::frame`]). Encoding always chooses the shortest form unless a
//! length is forced with [`encode_with_len`].

/// Largest encodable value, 2^62 − 1.
pub const MAX: u64 = (1 << 62) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarIntError {
    /// The buffer ended before the length the prefix announced.
    Truncated { needed: usize, available: usize },
    /// The value is above 2^62 − 1.
    TooLarge(u64),
    /// The output buffer is too small.
    BufferTooSmall { needed: usize, available: usize },
    /// A forced length is not 1, 2, 4 or 8, or cannot hold the value.
    BadLength(usize),
}

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

/// A forward-only reader over a borrowed byte slice.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub const fn position(&self) -> usize {
        self.pos
    }
    /// The whole underlying buffer, including consumed bytes.
    pub const fn full(&self) -> &'a [u8] {
        self.buf
    }
    pub fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }
    pub fn varint(&mut self) -> Result<u64, VarIntError> {
        let (value, len) = decode(self.remaining())?;
        self.pos += len;
        Ok(value)
    }
    pub fn u8(&mut self) -> Option<u8> {
        let byte = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(byte)
    }
    /// Borrows the next `len` bytes, or `None` if fewer remain.
    pub fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        let bytes = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(bytes)
    }
}

/// A forward-only writer into a borrowed output buffer.
#[derive(Debug)]
pub struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Writer<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub const fn position(&self) -> usize {
        self.pos
    }
    pub fn varint(&mut self, value: u64) -> Result<(), VarIntError> {
        self.pos += encode(value, &mut self.buf[self.pos..])?;
        Ok(())
    }
    pub fn varint_with_len(&mut self, value: u64, len: usize) -> Result<(), VarIntError> {
        self.pos += encode_with_len(value, len, &mut self.buf[self.pos..])?;
        Ok(())
    }
    pub fn bytes(&mut self, bytes: &[u8]) -> Result<(), VarIntError> {
        let available = self.buf.len() - self.pos;
        let Some(dst) = self.buf.get_mut(self.pos..self.pos + bytes.len()) else {
            return Err(VarIntError::BufferTooSmall {
                needed: bytes.len(),
                available,
            });
        };
        dst.copy_from_slice(bytes);
        self.pos += bytes.len();
        Ok(())
    }
    pub fn u8(&mut self, byte: u8) -> Result<(), VarIntError> {
        self.bytes(&[byte])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 9000 §A.1 / §16 examples.
    #[test]
    fn rfc_examples() {
        let cases: [(&[u8], u64); 5] = [
            (
                &[0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c],
                151_288_809_941_952_652,
            ),
            (&[0x9d, 0x7f, 0x3e, 0x7d], 494_878_333),
            (&[0x7b, 0xbd], 15_293),
            (&[0x25], 37),
            (&[0x40, 0x25], 37),
        ];
        for (bytes, value) in cases {
            assert_eq!(decode(bytes), Ok((value, bytes.len())));
            let mut out = [0u8; 8];
            let len = encode_with_len(value, bytes.len(), &mut out).unwrap();
            assert_eq!(&out[..len], bytes);
        }
        let mut out = [0u8; 8];
        assert_eq!(encode(37, &mut out), Ok(1));
        assert_eq!(out[0], 0x25);
    }

    #[test]
    fn bounds_and_errors() {
        let mut out = [0u8; 8];
        assert_eq!(encode(MAX, &mut out), Ok(8));
        assert_eq!(decode(&out), Ok((MAX, 8)));
        assert_eq!(
            encode(MAX + 1, &mut out),
            Err(VarIntError::TooLarge(MAX + 1))
        );
        assert_eq!(
            encode_with_len(64, 1, &mut out),
            Err(VarIntError::BadLength(1))
        );
        assert_eq!(
            encode_with_len(1, 3, &mut out),
            Err(VarIntError::BadLength(3))
        );
        assert_eq!(
            encode(1 << 20, &mut out[..2]),
            Err(VarIntError::BufferTooSmall {
                needed: 4,
                available: 2
            })
        );
        assert_eq!(
            decode(&[]),
            Err(VarIntError::Truncated {
                needed: 1,
                available: 0
            })
        );
        for boundary in [0, 63, 64, 16_383, 16_384, (1 << 30) - 1, 1 << 30] {
            let len = encode(boundary, &mut out).unwrap();
            assert_eq!(len, encoded_len(boundary).unwrap());
            assert_eq!(decode(&out[..len]), Ok((boundary, len)));
            for cut in 0..len {
                assert!(matches!(
                    decode(&out[..cut]),
                    Err(VarIntError::Truncated { .. })
                ));
            }
        }
    }
}
