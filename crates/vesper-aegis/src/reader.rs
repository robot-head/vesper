//! Bounds-checked little-endian reader over untrusted bytes.

use crate::error::{Format, ParseError};

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    format: Format,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8], format: Format) -> Self {
        Reader {
            bytes,
            pos: 0,
            format,
        }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    pub(crate) fn take(&mut self, n: usize, field: &'static str) -> Result<&'a [u8], ParseError> {
        let have = self.remaining();
        if n > have {
            return Err(ParseError::Truncated {
                format: self.format,
                field,
                need: n,
                have,
            });
        }
        let out = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub(crate) fn array<const N: usize>(
        &mut self,
        field: &'static str,
    ) -> Result<[u8; N], ParseError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N, field)?);
        Ok(out)
    }

    pub(crate) fn u16(&mut self, field: &'static str) -> Result<u16, ParseError> {
        Ok(u16::from_le_bytes(self.array(field)?))
    }

    pub(crate) fn u32(&mut self, field: &'static str) -> Result<u32, ParseError> {
        Ok(u32::from_le_bytes(self.array(field)?))
    }

    pub(crate) fn u64(&mut self, field: &'static str) -> Result<u64, ParseError> {
        Ok(u64::from_le_bytes(self.array(field)?))
    }

    /// Consume a reserved field that must be all zero.
    pub(crate) fn reserved(&mut self, n: usize, field: &'static str) -> Result<(), ParseError> {
        if self.take(n, field)?.iter().any(|&b| b != 0) {
            return Err(ParseError::NonZeroReserved {
                format: self.format,
                field,
            });
        }
        Ok(())
    }

    pub(crate) fn finish(&self) -> Result<(), ParseError> {
        match self.remaining() {
            0 => Ok(()),
            extra => Err(ParseError::TrailingBytes {
                format: self.format,
                extra,
            }),
        }
    }
}
