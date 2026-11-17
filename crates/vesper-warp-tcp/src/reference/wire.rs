//! The Rust reference implementation of [`crate::wire`], compiled without
//! `vesper-port` (the Vesper build is `vesper/wire.ves`, `options.ves`,
//! `segment.ves` and `checksum.ves`).

use crate::seq::Seq;
use crate::wire::{
    Flags, HEADER_LEN, HeaderFields, IPPROTO_TCP, IpPair, MAX_OPTIONS_LEN, MAX_SACK_BLOCKS,
    SackBlock, Segment, TcpOptions, Timestamps, WireError, offsets,
};

impl Flags {
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
impl TcpOptions {
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
                    for (k, chunk) in body.as_chunks::<8>().0.iter().enumerate() {
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

impl IpPair {
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
    let (pairs, rest) = bytes.as_chunks::<2>();
    for c in pairs {
        acc += u64::from(u16::from_be_bytes(*c));
    }
    if let [last] = rest {
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
