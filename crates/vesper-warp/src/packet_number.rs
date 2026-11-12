//! RFC 9000 Appendix A.2 packet-number encoding and A.3 decoding.
//!
//! [`decode_quic_packet_number_rfc9000`] is the §35.5 decoder. Its branch
//! structure is modelled in `lean/WarpPacketNumber.lean`, where congruence,
//! the 2^62 bound and window recovery are proved over `Nat`; that is a proof
//! about the model, and `tests/lean_model.rs` compares this Rust code with the
//! executable model on concrete inputs only.

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

/// A truncated packet-number length, 1 to 4 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PnLen(u8);

impl PnLen {
    pub const fn new(bytes: u8) -> Option<Self> {
        if bytes >= 1 && bytes <= 4 {
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

impl TruncatedPn {
    /// Reads `len` big-endian bytes; `None` if `bytes` is shorter.
    pub fn read(len: PnLen, bytes: &[u8]) -> Option<Self> {
        let field = bytes.get(..len.bytes())?;
        let value = field.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b));
        Some(Self { len, value })
    }
    /// Reconstructs the full packet number (A.3).
    pub fn decode(self, largest_pn: Option<u64>) -> Result<u64, PnError> {
        decode_with_largest(largest_pn, u64::from(self.value), self.len.bits())
    }
}

/// §35.5 / RFC 9000 A.3: reconstruct the full packet number from the largest
/// packet number successfully processed in the space and a truncated value of
/// `pn_nbits` bits. `largest_pn` is the largest processed packet number.
pub fn decode_quic_packet_number_rfc9000(
    largest_pn: u64,
    truncated_pn: u64,
    pn_nbits: u32,
) -> Result<u64, PnError> {
    decode_with_largest(Some(largest_pn), truncated_pn, pn_nbits)
}

/// As [`decode_quic_packet_number_rfc9000`], with `None` meaning no packet
/// has been processed yet in the space (expected packet number 0).
pub fn decode_with_largest(
    largest_pn: Option<u64>,
    truncated_pn: u64,
    pn_nbits: u32,
) -> Result<u64, PnError> {
    if !matches!(pn_nbits, 8 | 16 | 24 | 32) {
        return Err(PnError::InvalidBits(pn_nbits));
    }
    let expected = match largest_pn {
        None => 0,
        Some(largest) if largest < PN_LIMIT => largest + 1,
        Some(largest) => return Err(PnError::OutOfRange(largest)),
    };
    let win = 1u64 << pn_nbits;
    if truncated_pn >= win {
        return Err(PnError::TruncatedTooWide {
            truncated: truncated_pn,
            nbits: pn_nbits,
        });
    }
    let hwin = win / 2;
    let mask = win - 1;
    // expected ≤ 2^62 and win ≤ 2^32, so none of the additions below can
    // overflow u64; the comparisons are the RFC's signed ones rewritten
    // without subtraction (see the Lean model).
    let candidate = (expected & !mask) | truncated_pn;
    let result = if candidate + hwin <= expected && candidate + win < PN_LIMIT {
        candidate + win
    } else if candidate > expected + hwin && candidate >= win {
        candidate - win
    } else {
        candidate
    };
    if result < PN_LIMIT {
        Ok(result)
    } else {
        Err(PnError::Exhausted)
    }
}

/// A.2: the shortest truncation length that the peer can decode, given the
/// largest packet number it has acknowledged in this space.
pub fn encoded_len(full_pn: u64, largest_acked: Option<u64>) -> Result<PnLen, PnError> {
    if full_pn >= PN_LIMIT {
        return Err(PnError::OutOfRange(full_pn));
    }
    let num_unacked = match largest_acked {
        None => full_pn + 1,
        Some(acked) if acked >= full_pn => {
            return Err(PnError::NotAboveLargestAcked {
                full_pn,
                largest_acked: acked,
            });
        }
        Some(acked) => full_pn - acked,
    };
    // Need 2 * num_unacked <= 2^bits, i.e. num_unacked <= 2^(bits - 1).
    for bytes in 1..=4u8 {
        if num_unacked <= 1u64 << (u32::from(bytes) * 8 - 1) {
            return Ok(PnLen(bytes));
        }
    }
    Err(PnError::TooManyUnacked(num_unacked))
}

/// Encodes the truncated packet number into `out`, returning its length.
pub fn encode(full_pn: u64, largest_acked: Option<u64>, out: &mut [u8]) -> Result<PnLen, PnError> {
    let len = encoded_len(full_pn, largest_acked)?;
    write_truncated(full_pn, len, out)?;
    Ok(len)
}

/// Writes the low `len` bytes of `full_pn` big-endian.
pub fn write_truncated(full_pn: u64, len: PnLen, out: &mut [u8]) -> Result<(), PnError> {
    let dst = out.get_mut(..len.bytes()).ok_or(PnError::BufferTooSmall)?;
    dst.copy_from_slice(&full_pn.to_be_bytes()[8 - len.bytes()..]);
    Ok(())
}

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
