//! RFC 9000 Appendix A.2 packet-number encoding and A.3 decoding.
//!
//! [`decode_quic_packet_number_rfc9000`] is the §35.5 decoder. Its branch
//! structure is modelled in `lean/WarpPacketNumber.lean`, where congruence,
//! the 2^62 bound and window recovery are proved over `Nat`; that is a proof
//! about the model, and `tests/lean_model.rs` compares this Rust code with the
//! executable model on concrete inputs only.

#[cfg(test)]
use crate::PN_LIMIT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PnError {
    /// `pn_nbits` is not 8, 16, 24 or 32.
    InvalidBits(u32),
    /// The truncated value does not fit in `pn_nbits`.
    TruncatedTooWide { truncated: u64, nbits: u32 },
    /// A full packet number (or the largest seen) is not below 2^62.
    OutOfRange(u64),
    /// Reconstruction yields a packet number of 2^62 or more.
    Exhausted,
    /// Encoding a packet number not above the largest acknowledged.
    NotAboveLargestAcked { full_pn: u64, largest_acked: u64 },
    /// More than 2^31 unacknowledged packet numbers: no 4-byte encoding is
    /// unambiguous.
    TooManyUnacked(u64),
    /// Output buffer shorter than the encoded length.
    BufferTooSmall,
}

/// The longest truncated packet number, in bytes.
pub const PN_MAX_LEN: usize = 4;

/// A truncated packet-number length, 1 to [`PN_MAX_LEN`] bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PnLen(pub(crate) u8);

impl PnLen {
    pub const fn new(bytes: u8) -> Option<Self> {
        if bytes >= 1 && bytes as usize <= PN_MAX_LEN {
            Some(Self(bytes))
        } else {
            None
        }
    }
    /// From the low two bits of an unprotected first byte.
    pub const fn from_first_byte(first: u8) -> Self {
        Self((first & 0x03) + 1)
    }
    pub const fn bytes(self) -> usize {
        self.0 as usize
    }
    pub const fn bits(self) -> u32 {
        self.0 as u32 * 8
    }
    /// The two-bit Packet Number Length field value.
    pub const fn first_byte_bits(self) -> u8 {
        self.0 - 1
    }
}

/// A packet number as it appears on the wire after header-protection removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncatedPn {
    pub len: PnLen,
    pub value: u32,
}

pub use crate::imp::packet_number::{
    decode_quic_packet_number_rfc9000, decode_with_largest, encode, encoded_len, write_truncated,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_a3_example() {
        assert_eq!(
            decode_quic_packet_number_rfc9000(0xa82f_30ea, 0x9b32, 16),
            Ok(0xa82f_9b32)
        );
    }

    #[test]
    fn rfc_a2_examples() {
        // RFC 9000 §17.1 / A.2: 0xac5c02 with 0xabe8b3 acked needs 16 bits;
        // 0xace8fe needs 24 bits.
        assert_eq!(encoded_len(0xac_5c02, Some(0xab_e8b3)), Ok(PnLen(2)));
        assert_eq!(encoded_len(0xac_e8fe, Some(0xab_e8b3)), Ok(PnLen(3)));
        let mut out = [0u8; 4];
        let len = encode(0xac_5c02, Some(0xab_e8b3), &mut out).unwrap();
        assert_eq!(&out[..len.bytes()], &[0x5c, 0x02]);
        assert_eq!(
            TruncatedPn::read(len, &out)
                .unwrap()
                .decode(Some(0xab_e8b3)),
            Ok(0xac_5c02)
        );
    }

    #[test]
    fn rejects_bad_inputs() {
        assert_eq!(
            decode_with_largest(None, 0, 7),
            Err(PnError::InvalidBits(7))
        );
        assert_eq!(
            decode_with_largest(None, 0, 64),
            Err(PnError::InvalidBits(64))
        );
        assert_eq!(
            decode_with_largest(None, 256, 8),
            Err(PnError::TruncatedTooWide {
                truncated: 256,
                nbits: 8
            })
        );
        assert_eq!(
            decode_with_largest(Some(PN_LIMIT), 0, 8),
            Err(PnError::OutOfRange(PN_LIMIT))
        );
        // Largest = 2^62 - 1: expected is 2^62; a small truncated value would
        // reconstruct at or above 2^62.
        assert_eq!(
            decode_with_largest(Some(PN_LIMIT - 1), 0, 8),
            Err(PnError::Exhausted)
        );
        assert_eq!(
            decode_with_largest(Some(PN_LIMIT - 1), 0xff, 8),
            Ok(PN_LIMIT - 1)
        );
        assert_eq!(
            encoded_len(5, Some(5)),
            Err(PnError::NotAboveLargestAcked {
                full_pn: 5,
                largest_acked: 5
            })
        );
        assert_eq!(
            encoded_len(1 << 31, None),
            Err(PnError::TooManyUnacked((1 << 31) + 1))
        );
        assert_eq!(encoded_len((1 << 31) - 1, None), Ok(PnLen(4)));
        assert_eq!(
            encoded_len(PN_LIMIT, None),
            Err(PnError::OutOfRange(PN_LIMIT))
        );
        assert!(PnLen::new(0).is_none() && PnLen::new(5).is_none());
    }

    #[test]
    fn first_packets_decode() {
        assert_eq!(decode_with_largest(None, 0, 8), Ok(0));
        assert_eq!(decode_with_largest(None, 127, 8), Ok(127));
        // With expected 0, 128..=255 are "behind" but cannot go negative.
        assert_eq!(decode_with_largest(None, 200, 8), Ok(200));
    }
}
