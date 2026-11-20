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
/// Largest window the 16-bit window field carries unscaled (RFC 9293 §3.1).
pub const MAX_UNSCALED_WINDOW: u64 = 65_535;
/// Default send MSS without an MSS option over IPv4 (RFC 9293 §3.7.1).
pub const DEFAULT_IPV4_MSS: u16 = 536;
/// Default send MSS without an MSS option over IPv6 (RFC 9293 §3.7.1).
pub const DEFAULT_IPV6_MSS: u16 = 1220;

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

/// Option kinds and lengths (RFC 9293 §3.2, RFC 7323 §2.2 and §3.2, RFC
/// 2018 §2 and §3), and the padded sizes this crate encodes them in.
pub mod options {
    /// End of option list; the rest is padding.
    pub const END: u8 = 0;
    /// No-operation, used to pad options to a 4-byte boundary.
    pub const NOP: u8 = 1;
    /// Maximum segment size.
    pub const MSS: u8 = 2;
    /// Length of the MSS option.
    pub const MSS_LEN: u8 = 4;
    /// Window scale.
    pub const WINDOW_SCALE: u8 = 3;
    /// Length of the window scale option.
    pub const WINDOW_SCALE_LEN: u8 = 3;
    /// SACK permitted.
    pub const SACK_PERMITTED: u8 = 4;
    /// Length of the SACK-permitted option.
    pub const SACK_PERMITTED_LEN: u8 = 2;
    /// SACK blocks.
    pub const SACK: u8 = 5;
    /// Timestamps.
    pub const TIMESTAMPS: u8 = 8;
    /// Length of the timestamps option.
    pub const TIMESTAMPS_LEN: u8 = 10;
    /// Kind and length bytes before an option's body.
    pub const KIND_AND_LEN: usize = 2;
    /// Bytes of one SACK block (two sequence numbers).
    pub const SACK_BLOCK_LEN: usize = 8;
    /// Every option but the timestamps is encoded in one 4-byte word,
    /// NOP-padded.
    pub const PADDED_LEN: usize = 4;
    /// The timestamps option, NOP NOP-padded: 12 bytes.
    pub const PADDED_TIMESTAMPS_LEN: usize = 12;
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
}

#[cfg(feature = "vesper-port")]
pub use crate::ported::wire::{
    compute_checksum, fill_checksum, fold, pseudo_header_sum, sum_bytes, verify_checksum,
    write_header,
};
#[cfg(not(feature = "vesper-port"))]
pub use crate::reference::wire::{
    compute_checksum, fill_checksum, fold, pseudo_header_sum, sum_bytes, verify_checksum,
    write_header,
};
