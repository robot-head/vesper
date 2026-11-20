//! RFC 9000 §17 packet headers, parsed in place up to the header-protection
//! boundary.
//!
//! Parsing is two explicit steps, matching the wave-four packet-protection
//! contract:
//!
//! 1. [`HeaderParser::parse`] reads only bytes that header protection leaves
//!    in the clear and reports a [`ProtectedHeader`]: plain offsets (no
//!    borrow), including `pn_offset`, the start of the protected
//!    packet-number field, and the 16-byte sample range RFC 9001 §5.4.2 uses.
//! 2. Packet protection (lane 5) removes header protection in place; then
//!    [`ProtectedHeader::packet_number_field`] reads the unprotected first
//!    byte and truncated packet number from the same buffer. The caller
//!    reconstructs the full number with
//!    [`crate::packet_number::TruncatedPn::decode`] and hands it to the AEAD
//!    open, whose AAD is `packet[..header_len]`.
//! 3. Only after the AEAD open succeeds,
//!    [`UnprotectedHeader::check_reserved_bits`] rejects nonzero reserved
//!    bits (RFC 9000 §17.3.1).
//!
//! The short header is the §35.2 `QuicShortHdrWire` (`flags_spin_keyphase:
//! u8, dest_conn_id: ByteArray[n], pkt_num_trunc: u32be`), compiled through
//! `vesper-ffi` for a configured DCID length `n` (§35.2 fixes `n = 8`, giving
//! 13 bytes). Its 4-byte `pkt_num_trunc` field is the protected window: only
//! the first 1–4 bytes, as announced by the unprotected first byte, are the
//! packet number, the rest belong to the payload. The fixed long-header prefix
//! (`first`, `version`, `dcid_len`) also goes through a `vesper-ffi` layout.
//!
//! Only QUIC version 1 is supported. Other versions are a typed error;
//! Version Negotiation (version 0) is parsed and serialized.

// Constants, helpers and layouts that only the Rust implementation
// (src/reference/) reads.
#![cfg_attr(feature = "vesper-port", allow(dead_code))]

use crate::packet_number::{PnLen, TruncatedPn};
use crate::varint::VarIntError;
use crate::{PacketType, TransportErrorCode};
use vesper_ffi::{Declaration, Endian, Field, Integer, Registry, ViewError, WireLayout, WireType};

pub const QUIC_V1: u32 = 0x0000_0001;
pub const MAX_CID_LEN: usize = 20;
/// RFC 9001 §5.4.2 header-protection sample length.
pub const HP_SAMPLE_LEN: usize = 16;
/// The sample starts this many bytes after `pn_offset`.
pub const HP_SAMPLE_OFFSET: usize = 4;
/// RFC 9001 §5.8 Retry Integrity Tag length.
pub const RETRY_TAG_LEN: usize = 16;

pub(crate) const HEADER_FORM: u8 = 0x80;
pub(crate) const FIXED_BIT: u8 = 0x40;
pub(crate) const SPIN_BIT: u8 = 0x20;
pub(crate) const SHORT_RESERVED: u8 = 0x18;
pub(crate) const KEY_PHASE: u8 = 0x04;
pub(crate) const LONG_RESERVED: u8 = 0x0c;
/// First-byte bits under header protection (RFC 9001 §5.4.1): the low
/// five of a short header, the low four of a long one.
pub(crate) const SHORT_PROTECTED_BITS: u8 = 0x1f;
pub(crate) const LONG_PROTECTED_BITS: u8 = 0x0f;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    /// Fewer bytes than the header (or a length field) requires.
    Truncated {
        needed: usize,
        available: usize,
    },
    /// The fixed bit is zero (RFC 9000 §17.2/§17.3; greasing is not negotiated).
    FixedBitClear,
    UnsupportedVersion(u32),
    /// A connection ID length above 20 in a version 1 packet.
    CidTooLong(usize),
    /// The configured short-header DCID length is above 20.
    BadConfiguredCidLen(usize),
    /// The Length field runs past the end of the datagram.
    LengthExceedsDatagram {
        length: u64,
        available: usize,
    },
    /// Too few bytes after `pn_offset` for the header-protection sample.
    TooShortForSample {
        needed: usize,
        available: usize,
    },
    /// Reserved bits are nonzero after header-protection removal.
    ReservedBitsSet,
    /// Version Negotiation without a well-formed, non-empty version list.
    BadVersionList,
    EmptyRetryToken,
    /// A short-header packet was handed to long-header-only code or vice versa.
    WrongForm,
    VarInt(VarIntError),
    Layout(String),
    View(ViewError),
    BufferTooSmall {
        needed: usize,
        available: usize,
    },
    /// A value exceeds what its wire field can carry.
    FieldOverflow,
}

impl HeaderError {
    pub const fn transport_code(&self) -> TransportErrorCode {
        TransportErrorCode::ProtocolViolation
    }
}

impl From<VarIntError> for HeaderError {
    fn from(e: VarIntError) -> Self {
        match e {
            VarIntError::Truncated { needed, available } => Self::Truncated { needed, available },
            VarIntError::BufferTooSmall { needed, available } => {
                Self::BufferTooSmall { needed, available }
            }
            other => Self::VarInt(other),
        }
    }
}
impl From<ViewError> for HeaderError {
    fn from(e: ViewError) -> Self {
        match e {
            ViewError::ShortBuffer { needed, actual } => Self::Truncated {
                needed,
                available: actual,
            },
            other => Self::View(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongPacketType {
    Initial,
    ZeroRtt,
    Handshake,
    Retry,
}

impl LongPacketType {
    pub(crate) const fn bits(self) -> u8 {
        match self {
            Self::Initial => 0,
            Self::ZeroRtt => 1,
            Self::Handshake => 2,
            Self::Retry => 3,
        }
    }
    pub(crate) const fn from_first(first: u8) -> Self {
        match (first >> 4) & 0x03 {
            0 => Self::Initial,
            1 => Self::ZeroRtt,
            2 => Self::Handshake,
            _ => Self::Retry,
        }
    }
}

/// A byte range inside the packet buffer, kept as plain integers so the
/// header does not borrow the buffer that header-protection removal mutates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub len: usize,
}

impl Span {
    pub const fn end(self) -> usize {
        self.start + self.len
    }
    pub fn of(self, buf: &[u8]) -> &[u8] {
        buf.get(self.start..self.end()).unwrap_or(&[])
    }
}

/// A header whose first byte and packet number are still protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtectedHeader {
    pub packet_type: PacketType,
    /// Long headers only.
    pub version: Option<u32>,
    /// Short headers only: the spin bit is not header-protected.
    pub spin: Option<bool>,
    pub dcid: Span,
    pub scid: Span,
    /// Initial packets only.
    pub token: Span,
    /// Start of the protected packet-number field.
    pub pn_offset: usize,
    /// End of this packet: the Length field for long headers, the datagram
    /// end for short headers. A coalesced next packet starts here.
    pub packet_end: usize,
}

/// The header after header-protection removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnprotectedHeader {
    pub first_byte: u8,
    pub pn: TruncatedPn,
    /// Short headers only.
    pub key_phase: Option<bool>,
    /// Header length including the packet number: the AEAD AAD is
    /// `packet[..header_len]`.
    pub header_len: usize,
    /// Protected payload (ciphertext and tag) `header_len..packet_end`.
    pub payload: Span,
}

impl ProtectedHeader {
    pub fn dcid<'a>(&self, packet: &'a [u8]) -> &'a [u8] {
        self.dcid.of(packet)
    }
    pub fn scid<'a>(&self, packet: &'a [u8]) -> &'a [u8] {
        self.scid.of(packet)
    }
    pub fn token<'a>(&self, packet: &'a [u8]) -> &'a [u8] {
        self.token.of(packet)
    }
    /// RFC 9001 §5.4.2 sample position; parsing already checked it fits.
    pub const fn sample(&self) -> Span {
        Span {
            start: self.pn_offset + HP_SAMPLE_OFFSET,
            len: HP_SAMPLE_LEN,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPacket<'a> {
    pub version: u32,
    pub dcid: &'a [u8],
    pub scid: &'a [u8],
    pub token: &'a [u8],
    pub integrity_tag: &'a [u8; RETRY_TAG_LEN],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionNegotiation<'a> {
    pub dcid: &'a [u8],
    pub scid: &'a [u8],
    pub(crate) versions: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedPacket<'a> {
    Protected(ProtectedHeader),
    Retry(RetryPacket<'a>),
    VersionNegotiation(VersionNegotiation<'a>),
}

/// Compiled `vesper-ffi` layouts for the configured DCID length.
#[derive(Debug, Clone)]
pub struct HeaderParser {
    pub(crate) short: WireLayout,
    pub(crate) long_prefix: WireLayout,
    pub(crate) dcid_len: usize,
}

pub(crate) const SHORT_FIELDS: [&str; 3] = ["flags_spin_keyphase", "dest_conn_id", "pkt_num_trunc"];

impl HeaderParser {
    /// `short_dcid_len` is the length of connection IDs this endpoint issues
    /// (short headers do not carry it). §35.2 uses 8.
    pub fn new(short_dcid_len: usize) -> Result<Self, HeaderError> {
        if short_dcid_len > MAX_CID_LEN {
            return Err(HeaderError::BadConfiguredCidLen(short_dcid_len));
        }
        let u8_ = WireType::Integer(Integer::unsigned(8, Endian::Big));
        let registry = Registry::new([
            Declaration::new(
                "QuicShortHdrWire",
                vec![
                    Field::new(SHORT_FIELDS[0], u8_.clone()),
                    Field::new(SHORT_FIELDS[1], WireType::ByteArray(short_dcid_len)),
                    Field::new(
                        SHORT_FIELDS[2],
                        WireType::Integer(Integer::unsigned(32, Endian::Big)),
                    ),
                ],
            ),
            Declaration::new(
                "QuicLongHdrPrefixWire",
                vec![
                    Field::new("first", u8_.clone()),
                    Field::new(
                        "version",
                        WireType::Integer(Integer::unsigned(32, Endian::Big)),
                    ),
                    Field::new("dcid_len", u8_),
                ],
            ),
        ])
        .map_err(|e| HeaderError::Layout(e.to_string()))?;
        let short = registry
            .compile("QuicShortHdrWire")
            .map_err(|e| HeaderError::Layout(e.to_string()))?;
        let long_prefix = registry
            .compile("QuicLongHdrPrefixWire")
            .map_err(|e| HeaderError::Layout(e.to_string()))?;
        Ok(Self {
            short,
            long_prefix,
            dcid_len: short_dcid_len,
        })
    }

    pub const fn short_dcid_len(&self) -> usize {
        self.dcid_len
    }
    /// The `QuicShortHdrWire` layout (13 bytes for an 8-byte DCID).
    pub fn short_layout(&self) -> &WireLayout {
        &self.short
    }
}

pub use crate::imp::header::{write_long, write_retry, write_version_negotiation};

/// Fields of a version 1 Initial, 0-RTT or Handshake header to serialize.
#[derive(Debug, Clone, Copy)]
pub struct LongHeader<'a> {
    pub ty: LongPacketType,
    pub dcid: &'a [u8],
    pub scid: &'a [u8],
    /// Initial only; must be empty otherwise.
    pub token: &'a [u8],
    pub full_pn: u64,
    pub pn_len: PnLen,
    /// Bytes after the packet number: protected payload including AEAD tag.
    pub payload_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrittenLongHeader {
    pub pn_offset: usize,
    pub header_len: usize,
    /// `header_len + payload_len`.
    pub packet_len: usize,
}
