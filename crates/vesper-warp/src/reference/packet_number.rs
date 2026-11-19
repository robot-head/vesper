//! The Rust reference implementation of the ported functions of
//! [`crate::packet_number`], compiled without `vesper-port` (the Vesper
//! build is `vesper/src/pn.ves`). Frozen: bug fixes only (#1066).

use crate::PN_LIMIT;
use crate::packet_number::{PnError, PnLen, TruncatedPn};

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
