//! [`crate::wire`] forwarded to the Vesper build, with the conversions
//! between its public types and the word types of the export ABI.

use super::binding as b;
use super::types::{Blk, Hdr, Ips, Opts};
use crate::seq::Seq;
use crate::wire::{
    Flags, HeaderFields, IpPair, MAX_SACK_BLOCKS, SackBlock, Segment, TcpOptions, WireError,
};

pub(crate) fn ips_to(ips: IpPair) -> Ips {
    let w64 = |b: &[u8]| {
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        u64::from_be_bytes(a)
    };
    match ips {
        IpPair::V4 { src, dst } => Ips::V4 {
            src: u32::from_be_bytes(src),
            dst: u32::from_be_bytes(dst),
        },
        IpPair::V6 { src, dst } => Ips::V6 {
            s0: w64(&src[..8]),
            s1: w64(&src[8..]),
            d0: w64(&dst[..8]),
            d1: w64(&dst[8..]),
        },
    }
}

pub(crate) fn ips_from(ips: Ips) -> IpPair {
    let a16 = |hi: u64, lo: u64| {
        let mut a = [0u8; 16];
        a[..8].copy_from_slice(&hi.to_be_bytes());
        a[8..].copy_from_slice(&lo.to_be_bytes());
        a
    };
    match ips {
        Ips::V4 { src, dst } => IpPair::V4 {
            src: src.to_be_bytes(),
            dst: dst.to_be_bytes(),
        },
        Ips::V6 { s0, s1, d0, d1 } => IpPair::V6 {
            src: a16(s0, s1),
            dst: a16(d0, d1),
        },
    }
}

fn blk(b: SackBlock) -> Blk {
    Blk {
        left: b.left.0,
        right: b.right.0,
    }
}

fn sack(b: Blk) -> SackBlock {
    SackBlock {
        left: Seq(b.left),
        right: Seq(b.right),
    }
}

pub(crate) fn opts_to(o: &TcpOptions) -> Opts {
    let [b0, b1, b2, b3] = o.sack_blocks;
    Opts {
        mss: o.mss,
        window_scale: o.window_scale,
        sack_permitted: o.sack_permitted,
        b0: blk(b0),
        b1: blk(b1),
        b2: blk(b2),
        b3: blk(b3),
        sack_len: o.sack_len,
        timestamps: o.timestamps,
    }
}

pub(crate) fn opts_from(o: Opts) -> TcpOptions {
    let sack_blocks: [SackBlock; MAX_SACK_BLOCKS] =
        [sack(o.b0), sack(o.b1), sack(o.b2), sack(o.b3)];
    TcpOptions {
        mss: o.mss,
        window_scale: o.window_scale,
        sack_permitted: o.sack_permitted,
        sack_blocks,
        sack_len: o.sack_len,
        timestamps: o.timestamps,
    }
}

fn hdr(h: &HeaderFields) -> Hdr {
    Hdr {
        src_port: h.src_port,
        dst_port: h.dst_port,
        seq: h.seq.0,
        ack: h.ack.0,
        flags: h.flags.0,
        window: h.window,
        urgent: h.urgent,
    }
}

// Lengths cross the boundary as u64 and are bounded by the slices passed.
fn len(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

impl Flags {
    /// Whether every bit of `other` is set.
    #[must_use]
    pub fn contains(self, other: Flags) -> bool {
        b::flags_contains(self.0, other.0)
    }

    /// Union of two flag sets.
    #[must_use]
    pub fn with(self, other: Flags) -> Flags {
        Flags(b::flags_with(self.0, other.0))
    }
}

impl TcpOptions {
    /// Encoded length, including NOP padding to a 4-byte boundary.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        len(b::opts_encoded_len(opts_to(self)))
    }

    /// Serialize into `out`, returning the number of bytes written (a
    /// multiple of 4).
    pub fn write(&self, out: &mut [u8]) -> Result<usize, WireError> {
        b::opts_write(opts_to(self), out).map(len)
    }

    /// Parse an option list (the bytes between the fixed header and the data
    /// offset).
    pub fn parse(bytes: &[u8]) -> Result<TcpOptions, WireError> {
        b::opts_parse(bytes).map(opts_from)
    }
}

impl<'a> Segment<'a> {
    /// Parse a TCP segment in place. Does not verify the checksum; see
    /// [`verify_checksum`].
    pub fn parse(bytes: &'a [u8]) -> Result<Segment<'a>, WireError> {
        let p = b::segment_parse(bytes)?;
        let header_len = len(p.header_len);
        Ok(Segment {
            src_port: p.src_port,
            dst_port: p.dst_port,
            seq: Seq(p.seq),
            ack: Seq(p.ack),
            flags: Flags(p.flags),
            window: p.window,
            checksum: p.checksum,
            urgent: p.urgent,
            header_len,
            options: opts_from(p.options),
            payload: &bytes[header_len..],
        })
    }

    /// Sequence-space length: payload plus one each for SYN and FIN.
    #[must_use]
    pub fn seq_len(&self) -> u32 {
        b::segment_seq_len(self.flags.0, self.payload.len() as u64)
    }
}

impl IpPair {
    /// Default MSS assumed when the peer sends no MSS option (RFC 9293
    /// §3.7.1: 536 for IPv4, 1220 for IPv6).
    #[must_use]
    pub fn default_mss(self) -> u16 {
        b::ips_default_mss(ips_to(self))
    }
}

/// Write the header and options into `out[..header_len]`, leaving the
/// checksum zero. Returns the header length.
pub fn write_header(
    out: &mut [u8],
    h: &HeaderFields,
    opts: &TcpOptions,
) -> Result<usize, WireError> {
    b::write_header(out, hdr(h), opts_to(opts)).map(len)
}

/// One's-complement sum of `bytes` (odd trailing byte padded with zero),
/// accumulated into `acc`. Carries are folded at the end by [`fold`].
#[must_use]
pub fn sum_bytes(acc: u64, bytes: &[u8]) -> u64 {
    b::sum_bytes(acc, bytes)
}

/// Fold a 64-bit accumulator to a 16-bit one's-complement sum.
#[must_use]
pub fn fold(acc: u64) -> u16 {
    b::fold(acc)
}

/// Partial sum of the pseudo-header for a segment of `tcp_len` bytes.
pub fn pseudo_header_sum(ips: IpPair, tcp_len: usize) -> Result<u64, WireError> {
    b::pseudo_header_sum(ips_to(ips), tcp_len as u64)
}

/// Compute the checksum of `segment` (header with any checksum field value,
/// treated as zero, plus payload).
pub fn compute_checksum(ips: IpPair, segment: &[u8]) -> Result<u16, WireError> {
    b::compute_checksum(ips_to(ips), segment)
}

/// Compute and store the checksum in place.
pub fn fill_checksum(ips: IpPair, segment: &mut [u8]) -> Result<(), WireError> {
    b::fill_checksum(ips_to(ips), segment)
}

/// Verify a received segment's checksum (sum including the checksum field
/// must be 0xffff).
pub fn verify_checksum(ips: IpPair, segment: &[u8]) -> Result<(), WireError> {
    b::verify_checksum(ips_to(ips), segment)
}
