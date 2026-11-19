//! [`crate::packet_number`]'s ported functions, forwarded to
//! `vesper/src/pn.ves`.

use super::binding as b;
use crate::packet_number::{PnError, PnLen, TruncatedPn};

pub(crate) fn pn_error(e: b::PnErr) -> PnError {
    match e {
        b::PnErr::InvalidBits { bits } => PnError::InvalidBits(bits),
        b::PnErr::TruncatedTooWide { truncated, nbits } => {
            PnError::TruncatedTooWide { truncated, nbits }
        }
        b::PnErr::OutOfRange { pn } => PnError::OutOfRange(pn),
        b::PnErr::Exhausted => PnError::Exhausted,
        b::PnErr::NotAboveLargestAcked {
            full_pn,
            largest_acked,
        } => PnError::NotAboveLargestAcked {
            full_pn,
            largest_acked,
        },
        b::PnErr::TooManyUnacked { unacked } => PnError::TooManyUnacked(unacked),
        b::PnErr::BufferTooSmall => PnError::BufferTooSmall,
    }
}

impl TruncatedPn {
    /// Reads `len` big-endian bytes; `None` if `bytes` is shorter.
    pub fn read(len: PnLen, bytes: &[u8]) -> Option<Self> {
        let value = b::pn_read(len.bytes() as u64, bytes)?;
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
    b::pn_decode(largest_pn, truncated_pn, pn_nbits).map_err(pn_error)
}

/// A.2: the shortest truncation length that the peer can decode, given the
/// largest packet number it has acknowledged in this space.
pub fn encoded_len(full_pn: u64, largest_acked: Option<u64>) -> Result<PnLen, PnError> {
    b::pn_len_for(full_pn, largest_acked)
        .map(|n| PnLen(n as u8))
        .map_err(pn_error)
}

/// Encodes the truncated packet number into `out`, returning its length.
pub fn encode(full_pn: u64, largest_acked: Option<u64>, out: &mut [u8]) -> Result<PnLen, PnError> {
    let len = encoded_len(full_pn, largest_acked)?;
    write_truncated(full_pn, len, out)?;
    Ok(len)
}

/// Writes the low `len` bytes of `full_pn` big-endian.
pub fn write_truncated(full_pn: u64, len: PnLen, out: &mut [u8]) -> Result<(), PnError> {
    b::pn_write(full_pn, len.bytes() as u64, out).map_err(pn_error)
}
