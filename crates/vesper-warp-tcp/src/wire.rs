//! TCP header and option wire format (RFC 9293 §3.1, RFC 7323, RFC 2018) and
//! the Internet checksum over an IPv4/IPv6 pseudo-header (RFC 1071, RFC 9293
//! §3.1, RFC 8200 §8.1).
//!
//! Parsing borrows the caller's bytes and never allocates. Every malformed
//! input is a typed [`WireError`]; nothing here panics on attacker-controlled
//! bytes.

use crate::seq::Seq;

/// Fixed TCP header length without options.
pub const HEADER_LEN: usize = 20;
/// Maximum TCP header length (data offset 15).
pub const MAX_HEADER_LEN: usize = 60;
/// Maximum option bytes.
pub const MAX_OPTIONS_LEN: usize = MAX_HEADER_LEN - HEADER_LEN;
/// IP protocol number for TCP.
pub const IPPROTO_TCP: u8 = 6;
/// Maximum SACK blocks representable in one option (RFC 2018 §3).
pub const MAX_SACK_BLOCKS: usize = 4;
/// Largest permitted window shift (RFC 7323 §2.3).
pub const MAX_WINDOW_SCALE: u8 = 14;

/// Byte offsets of the fixed header fields, cross-checked against a
/// `vesper-ffi` wire layout in the tests.
pub mod offsets {
    /// Source port, u16 big-endian.
    pub const SRC_PORT: usize = 0;
    /// Destination port, u16 big-endian.
    pub const DST_PORT: usize = 2;
    /// Sequence number, u32 big-endian.
    pub const SEQ: usize = 4;
    /// Acknowledgment number, u32 big-endian.
    pub const ACK: usize = 8;
    /// Data offset (high nibble) and reserved bits.
    pub const DATA_OFFSET: usize = 12;
    /// Control bits.
    pub const FLAGS: usize = 13;
    /// Window, u16 big-endian.
    pub const WINDOW: usize = 14;
    /// Checksum, u16 big-endian.
    pub const CHECKSUM: usize = 16;
    /// Urgent pointer, u16 big-endian.
    pub const URGENT: usize = 18;
}

/// TCP control bits (RFC 9293 §3.1, RFC 3168 for CWR/ECE).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Flags(pub u8);

impl Flags {
    /// No more data from sender.
    pub const FIN: Flags = Flags(0x01);
    /// Synchronize sequence numbers.
    pub const SYN: Flags = Flags(0x02);
    /// Reset the connection.
    pub const RST: Flags = Flags(0x04);
    /// Push function.
    pub const PSH: Flags = Flags(0x08);
    /// Acknowledgment field is significant.
    pub const ACK: Flags = Flags(0x10);
    /// Urgent pointer field is significant.
    pub const URG: Flags = Flags(0x20);
    /// ECN-Echo (recognized, not negotiated by this endpoint).
    pub const ECE: Flags = Flags(0x40);
    /// Congestion Window Reduced (recognized, not negotiated).
    pub const CWR: Flags = Flags(0x80);

    /// Whether every bit of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Union of two flag sets.
    #[must_use]
    pub const fn with(self, other: Flags) -> Flags {
        Flags(self.0 | other.0)
    }
}

impl core::ops::BitOr for Flags {
    type Output = Flags;
    fn bitor(self, rhs: Flags) -> Flags {
        self.with(rhs)
    }
}

/// A SACK block `[left, right)` in sequence space (RFC 2018 §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SackBlock {
    /// First sequence number of the block.
    pub left: Seq,
    /// Sequence number immediately following the block.
    pub right: Seq,
}

/// RFC 7323 timestamps option values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Timestamps {
    /// Timestamp value of the sender.
    pub val: u32,
    /// Echoed timestamp of the peer.
    pub ecr: u32,
}

/// Options recognized by this endpoint. Unknown options with a valid length
/// are skipped as RFC 9293 §3.1 requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TcpOptions {
    /// Maximum segment size (kind 2), SYN only.
    pub mss: Option<u16>,
    /// Window shift count (kind 3), SYN only; values above 14 are clamped by
    /// the connection, as RFC 7323 §2.3 requires, but reported as received.
    pub window_scale: Option<u8>,
    /// SACK-permitted (kind 4), SYN only.
    pub sack_permitted: bool,
    /// SACK blocks (kind 5).
    pub sack_blocks: [SackBlock; MAX_SACK_BLOCKS],
    /// Number of valid entries in `sack_blocks`.
    pub sack_len: u8,
    /// Timestamps (kind 8).
    pub timestamps: Option<Timestamps>,
}

impl TcpOptions {
    /// The SACK blocks present.
    #[must_use]
    pub fn sack(&self) -> &[SackBlock] {
        &self.sack_blocks[..usize::from(self.sack_len).min(MAX_SACK_BLOCKS)]
    }

    /// Encoded length, including NOP padding to a 4-byte boundary.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        let mut n = 0;
        if self.mss.is_some() {
            n += 4;
        }
        if self.window_scale.is_some() {
            n += 4; // NOP + 3
        }
        if self.sack_permitted {
            n += 4; // NOP NOP + 2 (or packed with timestamps below)
        }
        if self.timestamps.is_some() {
            n += 12; // NOP NOP + 10
        }
        if self.sack_len > 0 {
            n += 4 + 8 * usize::from(self.sack_len).min(MAX_SACK_BLOCKS);
        }
        n
    }

    /// Serialize into `out`, returning the number of bytes written (a
    /// multiple of 4).
    pub fn write(&self, out: &mut [u8]) -> Result<usize, WireError> {
        let len = self.encoded_len();
        if len > MAX_OPTIONS_LEN {
            return Err(WireError::OptionsTooLong);
        }
        if out.len() < len {
            return Err(WireError::BufferTooSmall);
        }
        let mut i = 0;
        if let Some(mss) = self.mss {
            out[i..i + 4].copy_from_slice(&[2, 4, (mss >> 8) as u8, mss as u8]);
            i += 4;
        }
        if let Some(ws) = self.window_scale {
            out[i..i + 4].copy_from_slice(&[1, 3, 3, ws]);
            i += 4;
        }
        if self.sack_permitted {
            out[i..i + 4].copy_from_slice(&[1, 1, 4, 2]);
            i += 4;
        }
        if let Some(ts) = self.timestamps {
            out[i..i + 2].copy_from_slice(&[1, 1]);
            out[i + 2] = 8;
            out[i + 3] = 10;
            out[i + 4..i + 8].copy_from_slice(&ts.val.to_be_bytes());
            out[i + 8..i + 12].copy_from_slice(&ts.ecr.to_be_bytes());
            i += 12;
        }
        if self.sack_len > 0 {
            let blocks = self.sack();
            out[i] = 1;
            out[i + 1] = 1;
            out[i + 2] = 5;
            out[i + 3] = (2 + 8 * blocks.len()) as u8;
            i += 4;
            for b in blocks {
                out[i..i + 4].copy_from_slice(&b.left.0.to_be_bytes());
                out[i + 4..i + 8].copy_from_slice(&b.right.0.to_be_bytes());
                i += 8;
            }
        }
        debug_assert_eq!(i, len);
        Ok(len)
    }

    /// Parse an option list (the bytes between the fixed header and the data
    /// offset).
    pub fn parse(bytes: &[u8]) -> Result<TcpOptions, WireError> {
        let mut opts = TcpOptions::default();
        let mut i = 0usize;
        while i < bytes.len() {
            let kind = bytes[i];
            match kind {
                0 => break, // End of option list; the rest is padding.
                1 => {
                    i += 1;
                    continue;
                }
                _ => {}
            }
            let Some(&len) = bytes.get(i + 1) else {
                return Err(WireError::MalformedOption { kind });
            };
            let len = usize::from(len);
            if len < 2 || i + len > bytes.len() {
                return Err(WireError::MalformedOption { kind });
            }
            let body = &bytes[i + 2..i + len];
            match kind {
                2 => {
                    if len != 4 || opts.mss.is_some() {
                        return Err(WireError::MalformedOption { kind });
                    }
                    opts.mss = Some(u16::from_be_bytes([body[0], body[1]]));
                }
                3 => {
                    if len != 3 || opts.window_scale.is_some() {
                        return Err(WireError::MalformedOption { kind });
                    }
                    opts.window_scale = Some(body[0]);
                }
                4 => {
                    if len != 2 || opts.sack_permitted {
                        return Err(WireError::MalformedOption { kind });
                    }
                    opts.sack_permitted = true;
                }
                5 => {
                    let n = (len - 2) / 8;
                    if (len - 2) % 8 != 0 || n == 0 || n > MAX_SACK_BLOCKS || opts.sack_len != 0 {
                        return Err(WireError::MalformedOption { kind });
                    }
                    for (k, chunk) in body.chunks_exact(8).enumerate() {
                        opts.sack_blocks[k] = SackBlock {
                            left: Seq(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])),
                            right: Seq(u32::from_be_bytes([
                                chunk[4], chunk[5], chunk[6], chunk[7],
                            ])),
                        };
                    }
                    opts.sack_len = n as u8;
                }
                8 => {
                    if len != 10 || opts.timestamps.is_some() {
                        return Err(WireError::MalformedOption { kind });
                    }
                    opts.timestamps = Some(Timestamps {
                        val: u32::from_be_bytes([body[0], body[1], body[2], body[3]]),
                        ecr: u32::from_be_bytes([body[4], body[5], body[6], body[7]]),
                    });
                }
                _ => {} // Unknown option with a valid length: ignore.
            }
            i += len;
        }
        Ok(opts)
    }
}

/// Typed wire-format errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// Fewer than 20 bytes.
    Truncated,
    /// Data offset below 5 or beyond the segment.
    BadDataOffset,
    /// An option with an invalid length, a duplicated singleton option or an
    /// option running past the header.
    MalformedOption {
        /// The offending option kind.
        kind: u8,
    },
    /// Options would exceed 40 bytes.
    OptionsTooLong,
    /// Output buffer too small.
    BufferTooSmall,
    /// Segment longer than the 16-bit (IPv4) or 32-bit (IPv6) pseudo-header
    /// length field can describe.
    TooLong,
    /// The checksum does not verify.
    BadChecksum,
}

impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WireError::Truncated => f.write_str("segment shorter than the TCP header"),
            WireError::BadDataOffset => f.write_str("invalid TCP data offset"),
            WireError::MalformedOption { kind } => write!(f, "malformed TCP option kind {kind}"),
            WireError::OptionsTooLong => f.write_str("TCP options exceed 40 bytes"),
            WireError::BufferTooSmall => f.write_str("output buffer too small"),
            WireError::TooLong => f.write_str("segment too long for the pseudo-header"),
            WireError::BadChecksum => f.write_str("TCP checksum mismatch"),
        }
    }
}

impl std::error::Error for WireError {}

/// A parsed segment borrowing the caller's buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment<'a> {
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// Sequence number.
    pub seq: Seq,
    /// Acknowledgment number (meaningful when ACK is set).
    pub ack: Seq,
    /// Control bits.
    pub flags: Flags,
    /// Raw (unscaled) window field.
    pub window: u16,
    /// Checksum field as received.
    pub checksum: u16,
    /// Urgent pointer.
    pub urgent: u16,
    /// Header length in bytes (data offset × 4).
    pub header_len: usize,
    /// Parsed options.
    pub options: TcpOptions,
    /// Payload, borrowed from the input.
    pub payload: &'a [u8],
}

impl<'a> Segment<'a> {
    /// Parse a TCP segment in place. Does not verify the checksum; see
    /// [`verify_checksum`].
    pub fn parse(bytes: &'a [u8]) -> Result<Segment<'a>, WireError> {
        if bytes.len() < HEADER_LEN {
            return Err(WireError::Truncated);
        }
        let be16 = |o: usize| u16::from_be_bytes([bytes[o], bytes[o + 1]]);
        let be32 =
            |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        let header_len = usize::from(bytes[offsets::DATA_OFFSET] >> 4) * 4;
        if header_len < HEADER_LEN || header_len > bytes.len() {
            return Err(WireError::BadDataOffset);
        }
        let options = TcpOptions::parse(&bytes[HEADER_LEN..header_len])?;
        Ok(Segment {
            src_port: be16(offsets::SRC_PORT),
            dst_port: be16(offsets::DST_PORT),
            seq: Seq(be32(offsets::SEQ)),
            ack: Seq(be32(offsets::ACK)),
            flags: Flags(bytes[offsets::FLAGS]),
            window: be16(offsets::WINDOW),
            checksum: be16(offsets::CHECKSUM),
            urgent: be16(offsets::URGENT),
            header_len,
            options,
            payload: &bytes[header_len..],
        })
    }

    /// Sequence-space length: payload plus one each for SYN and FIN.
    #[must_use]
    pub fn seq_len(&self) -> u32 {
        // Payload is bounded by the IP datagram length (< 2^32).
        let mut n = self.payload.len() as u32;
        if self.flags.contains(Flags::SYN) {
            n = n.wrapping_add(1);
        }
        if self.flags.contains(Flags::FIN) {
            n = n.wrapping_add(1);
        }
        n
    }
}

/// Fields of a header to serialize.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct HeaderFields {
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// Sequence number.
    pub seq: Seq,
    /// Acknowledgment number.
    pub ack: Seq,
    /// Control bits.
    pub flags: Flags,
    /// Window field (already scaled down).
    pub window: u16,
    /// Urgent pointer.
    pub urgent: u16,
}

/// Write the header and options into `out[..header_len]`, leaving the
/// checksum zero. Returns the header length.
pub fn write_header(
    out: &mut [u8],
    h: &HeaderFields,
    opts: &TcpOptions,
) -> Result<usize, WireError> {
    let opt_len = opts.encoded_len();
    if opt_len > MAX_OPTIONS_LEN {
        return Err(WireError::OptionsTooLong);
    }
    let header_len = HEADER_LEN + opt_len;
    if out.len() < header_len {
        return Err(WireError::BufferTooSmall);
    }
    out[offsets::SRC_PORT..offsets::SRC_PORT + 2].copy_from_slice(&h.src_port.to_be_bytes());
    out[offsets::DST_PORT..offsets::DST_PORT + 2].copy_from_slice(&h.dst_port.to_be_bytes());
    out[offsets::SEQ..offsets::SEQ + 4].copy_from_slice(&h.seq.0.to_be_bytes());
    out[offsets::ACK..offsets::ACK + 4].copy_from_slice(&h.ack.0.to_be_bytes());
    out[offsets::DATA_OFFSET] = ((header_len / 4) as u8) << 4;
    out[offsets::FLAGS] = h.flags.0;
    out[offsets::WINDOW..offsets::WINDOW + 2].copy_from_slice(&h.window.to_be_bytes());
    out[offsets::CHECKSUM..offsets::CHECKSUM + 2].copy_from_slice(&[0, 0]);
    out[offsets::URGENT..offsets::URGENT + 2].copy_from_slice(&h.urgent.to_be_bytes());
    opts.write(&mut out[HEADER_LEN..header_len])?;
    Ok(header_len)
}

/// Source and destination addresses for the pseudo-header, as seen by the
/// sender of the segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IpPair {
    /// IPv4 addresses.
    V4 {
        /// Source address.
        src: [u8; 4],
        /// Destination address.
        dst: [u8; 4],
    },
    /// IPv6 addresses.
    V6 {
        /// Source address.
        src: [u8; 16],
        /// Destination address.
        dst: [u8; 16],
    },
}

impl IpPair {
    /// The same pair seen from the other end.
    #[must_use]
    pub const fn reversed(self) -> IpPair {
        match self {
            IpPair::V4 { src, dst } => IpPair::V4 { src: dst, dst: src },
            IpPair::V6 { src, dst } => IpPair::V6 { src: dst, dst: src },
        }
    }

    /// Default MSS assumed when the peer sends no MSS option (RFC 9293
    /// §3.7.1: 536 for IPv4, 1220 for IPv6).
    #[must_use]
    pub const fn default_mss(self) -> u16 {
        match self {
            IpPair::V4 { .. } => 536,
            IpPair::V6 { .. } => 1220,
        }
    }
}

/// One's-complement sum of `bytes` (odd trailing byte padded with zero),
/// accumulated into `acc`. Carries are folded at the end by [`fold`].
#[must_use]
pub fn sum_bytes(mut acc: u64, bytes: &[u8]) -> u64 {
    let mut chunks = bytes.chunks_exact(2);
    for c in &mut chunks {
        acc += u64::from(u16::from_be_bytes([c[0], c[1]]));
    }
    if let [last] = chunks.remainder() {
        acc += u64::from(*last) << 8;
    }
    acc
}

/// Fold a 64-bit accumulator to a 16-bit one's-complement sum.
#[must_use]
pub fn fold(mut acc: u64) -> u16 {
    while acc >> 16 != 0 {
        acc = (acc & 0xffff) + (acc >> 16);
    }
    acc as u16
}

/// Partial sum of the pseudo-header for a segment of `tcp_len` bytes.
pub fn pseudo_header_sum(ips: IpPair, tcp_len: usize) -> Result<u64, WireError> {
    Ok(match ips {
        IpPair::V4 { src, dst } => {
            let len = u16::try_from(tcp_len).map_err(|_| WireError::TooLong)?;
            let acc = sum_bytes(sum_bytes(0, &src), &dst);
            acc + u64::from(IPPROTO_TCP) + u64::from(len)
        }
        IpPair::V6 { src, dst } => {
            let len = u32::try_from(tcp_len).map_err(|_| WireError::TooLong)?;
            let acc = sum_bytes(sum_bytes(0, &src), &dst);
            acc + u64::from(len >> 16) + u64::from(len & 0xffff) + u64::from(IPPROTO_TCP)
        }
    })
}

/// Compute the checksum of `segment` (header with any checksum field value,
/// treated as zero, plus payload).
pub fn compute_checksum(ips: IpPair, segment: &[u8]) -> Result<u16, WireError> {
    if segment.len() < HEADER_LEN {
        return Err(WireError::Truncated);
    }
    let mut acc = pseudo_header_sum(ips, segment.len())?;
    acc = sum_bytes(acc, &segment[..offsets::CHECKSUM]);
    acc = sum_bytes(acc, &segment[offsets::CHECKSUM + 2..]);
    Ok(!fold(acc))
}

/// Compute and store the checksum in place.
pub fn fill_checksum(ips: IpPair, segment: &mut [u8]) -> Result<(), WireError> {
    let c = compute_checksum(ips, segment)?;
    segment[offsets::CHECKSUM..offsets::CHECKSUM + 2].copy_from_slice(&c.to_be_bytes());
    Ok(())
}

/// Verify a received segment's checksum (sum including the checksum field
/// must be 0xffff).
pub fn verify_checksum(ips: IpPair, segment: &[u8]) -> Result<(), WireError> {
    if segment.len() < HEADER_LEN {
        return Err(WireError::Truncated);
    }
    let acc = sum_bytes(pseudo_header_sum(ips, segment.len())?, segment);
    if fold(acc) == 0xffff {
        Ok(())
    } else {
        Err(WireError::BadChecksum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc1071_numeric_example() {
        // RFC 1071 §3: bytes 00 01 f2 03 f4 f5 f6 f7 sum to ddf2.
        let bytes = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        assert_eq!(fold(sum_bytes(0, &bytes)), 0xddf2);
        assert_eq!(!fold(sum_bytes(0, &bytes)), 0x220d);
    }

    #[test]
    fn options_roundtrip() {
        let mut o = TcpOptions {
            mss: Some(1460),
            window_scale: Some(7),
            sack_permitted: true,
            timestamps: Some(Timestamps { val: 1, ecr: 2 }),
            ..TcpOptions::default()
        };
        let mut buf = [0u8; 40];
        let n = o.write(&mut buf).unwrap();
        assert_eq!(n, 24);
        assert_eq!(TcpOptions::parse(&buf[..n]).unwrap(), o);
        o = TcpOptions {
            timestamps: Some(Timestamps { val: 9, ecr: 8 }),
            ..TcpOptions::default()
        };
        o.sack_len = 3;
        for k in 0..3 {
            o.sack_blocks[k] = SackBlock {
                left: Seq(k as u32 * 10),
                right: Seq(k as u32 * 10 + 5),
            };
        }
        let n = o.write(&mut buf).unwrap();
        assert_eq!(n, 40);
        assert_eq!(TcpOptions::parse(&buf[..n]).unwrap(), o);
    }

    #[test]
    fn malformed_options_are_errors() {
        for bad in [
            &[2u8][..],
            &[2, 3, 0][..],
            &[2, 4, 0][..],
            &[3, 2][..],
            &[5, 9, 0, 0, 0, 0, 0, 0, 0][..],
            &[5, 2][..],
            &[8, 9, 0, 0, 0, 0, 0, 0, 0][..],
            &[99, 0][..],
            &[99, 1][..],
            &[4, 2, 4, 2][..],
        ] {
            assert!(
                matches!(
                    TcpOptions::parse(bad),
                    Err(WireError::MalformedOption { .. })
                ),
                "{bad:?}"
            );
        }
        // Unknown options are skipped; EOL ends the list.
        let o = TcpOptions::parse(&[99, 4, 1, 2, 0, 77, 77]).unwrap();
        assert_eq!(o, TcpOptions::default());
    }
}
