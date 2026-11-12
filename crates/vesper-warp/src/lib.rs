//! Vesper Warp (design §35) QUIC packet core: sans-IO, no sockets, no crypto.
//!
//! * [`varint`]: RFC 9000 §16 variable-length integers.
//! * [`packet_number`]: RFC 9000 Appendix A.2 encoding and A.3 decoding
//!   (§35.5 `decode_quic_packet_number_rfc9000`).
//! * [`header`]: short (§35.2 `QuicShortHdrWire`, configured DCID length) and
//!   long (RFC 9000 §17.2) headers, parsed in place up to the
//!   header-protection boundary.
//! * [`frame`]: zero-copy frame parse/encode with per-packet-type permissions.
//! * [`ack`]: per-space received-packet tracking, duplicate detection and
//!   bounded ACK generation.
//! * [`recovery`]: RFC 9002 RTT estimation, loss detection, PTO and
//!   persistent congestion, emitting congestion-control-agnostic events.
//!
//! Receive order (wave-four contract): [`header::parse`] reports the
//! protected packet-number offset; packet protection (lane 5) removes header
//! protection in place; [`header::ProtectedHeader::packet_number_field`] reads
//! the now-unprotected length and truncated value and
//! [`packet_number::decode_quic_packet_number_rfc9000`] reconstructs the full
//! packet number, which lane 5's AEAD open takes for the RFC 9001 §5.3 nonce.
//! Frames are parsed only from the opened payload.
#![forbid(unsafe_code)]

pub mod ack;
pub mod frame;
pub mod header;
pub mod packet_number;
pub mod recovery;
pub mod varint;

use std::fmt;

/// Packet-number spaces (RFC 9000 §12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PnSpace {
    Initial,
    Handshake,
    ApplicationData,
}

impl PnSpace {
    pub const ALL: [PnSpace; 3] = [Self::Initial, Self::Handshake, Self::ApplicationData];
    pub const fn index(self) -> usize {
        match self {
            Self::Initial => 0,
            Self::Handshake => 1,
            Self::ApplicationData => 2,
        }
    }
}

/// Packet types that carry frames; they decide frame permissions (RFC 9000
/// §12.4 Table 3). 0-RTT and 1-RTT share the application-data space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketType {
    Initial,
    ZeroRtt,
    Handshake,
    OneRtt,
}

impl PacketType {
    pub const fn space(self) -> PnSpace {
        match self {
            Self::Initial => PnSpace::Initial,
            Self::Handshake => PnSpace::Handshake,
            Self::ZeroRtt | Self::OneRtt => PnSpace::ApplicationData,
        }
    }
}

/// Largest packet number plus one, and largest varint plus one (2^62).
pub const PN_LIMIT: u64 = 1 << 62;

/// RFC 9000 §20.1 transport error codes used by this crate's typed errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorCode {
    FrameEncodingError,
    ProtocolViolation,
    FlowControlError,
}

impl TransportErrorCode {
    pub const fn code(self) -> u64 {
        match self {
            Self::FlowControlError => 0x03,
            Self::FrameEncodingError => 0x07,
            Self::ProtocolViolation => 0x0a,
        }
    }
}

/// Union of the module errors, for callers that want one type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuicError {
    VarInt(varint::VarIntError),
    PacketNumber(packet_number::PnError),
    Header(header::HeaderError),
    Frame(frame::FrameError),
    Ack(ack::AckError),
    Recovery(recovery::RecoveryError),
}

impl fmt::Display for QuicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VarInt(e) => write!(f, "varint: {e:?}"),
            Self::PacketNumber(e) => write!(f, "packet number: {e:?}"),
            Self::Header(e) => write!(f, "header: {e:?}"),
            Self::Frame(e) => write!(f, "frame: {e:?}"),
            Self::Ack(e) => write!(f, "ack: {e:?}"),
            Self::Recovery(e) => write!(f, "recovery: {e:?}"),
        }
    }
}
impl std::error::Error for QuicError {}

macro_rules! from_error {
    ($($variant:ident($ty:ty)),*) => {$(
        impl From<$ty> for QuicError {
            fn from(e: $ty) -> Self { Self::$variant(e) }
        }
    )*};
}
from_error!(
    VarInt(varint::VarIntError),
    PacketNumber(packet_number::PnError),
    Header(header::HeaderError),
    Frame(frame::FrameError),
    Ack(ack::AckError),
    Recovery(recovery::RecoveryError)
);
