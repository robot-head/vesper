//! QUIC variable-length integers (RFC 9000 §16), local to this crate.
//!
//! The wave-five contracts let this crate depend only on
//! `vesper-warp-crypto`, so the packet core's varint codec is not reused.

/// Largest value a QUIC varint can carry, `2^62 - 1`.
pub const VARINT_MAX: u64 = (1 << 62) - 1;

/// Number of bytes the minimal encoding of `v` takes, or `None` if
/// `v > VARINT_MAX`.
pub fn encoded_len(v: u64) -> Option<usize> {
    match v {
        0..=63 => Some(1),
        64..=16_383 => Some(2),
        16_384..=1_073_741_823 => Some(4),
        1_073_741_824..=VARINT_MAX => Some(8),
        _ => None,
    }
}

/// Appends the minimal encoding of `v`. Returns `false` (and writes
/// nothing) if `v > VARINT_MAX`.
pub fn write(out: &mut Vec<u8>, v: u64) -> bool {
    match encoded_len(v) {
        Some(1) => out.push(v as u8),
        Some(2) => out.extend_from_slice(&((v as u16) | 0x4000).to_be_bytes()),
        Some(4) => out.extend_from_slice(&((v as u32) | 0x8000_0000).to_be_bytes()),
        Some(8) => out.extend_from_slice(&(v | 0xc000_0000_0000_0000).to_be_bytes()),
        _ => return false,
    }
    true
}

/// A bounds-checked reader over untrusted bytes.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Reads one varint; any encoding length is accepted (RFC 9000 §16
    /// does not require minimal encodings).
    pub fn varint(&mut self) -> Option<u64> {
        let first = *self.buf.get(self.pos)?;
        let len = 1usize << (first >> 6);
        let bytes = self.bytes(len)?;
        let mut v = u64::from(bytes[0] & 0x3f);
        for &b in &bytes[1..] {
            v = (v << 8) | u64::from(b);
        }
        Some(v)
    }

    /// Takes exactly `n` bytes.
    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let out = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc9000_appendix_a1_examples() {
        for (bytes, v) in [
            (
                &[0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c][..],
                151_288_809_941_952_652,
            ),
            (&[0x9d, 0x7f, 0x3e, 0x7d][..], 494_878_333),
            (&[0x7b, 0xbd][..], 15_293),
            (&[0x25][..], 37),
            (&[0x40, 0x25][..], 37),
        ] {
            let mut r = Reader::new(bytes);
            assert_eq!(r.varint(), Some(v));
            assert!(r.is_empty());
        }
        let mut out = Vec::new();
        assert!(write(&mut out, 15_293));
        assert_eq!(out, [0x7b, 0xbd]);
        assert!(!write(&mut out, VARINT_MAX + 1));
        assert_eq!(Reader::new(&[0x80, 0x01]).varint(), None);
    }
}
