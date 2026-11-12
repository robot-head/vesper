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

use crate::packet_number::{PnLen, TruncatedPn};
use crate::varint::{self, Reader, VarIntError, Writer};
use crate::{PacketType, TransportErrorCode};
use vesper_ffi::{
    Declaration, Endian, Field, Integer, Registry, ViewError, WireLayout, WireType, split_wire,
    split_wire_mut,
};

pub const QUIC_V1: u32 = 0x0000_0001;
pub const MAX_CID_LEN: usize = 20;
/// RFC 9001 §5.4.2 header-protection sample length.
pub const HP_SAMPLE_LEN: usize = 16;
/// The sample starts this many bytes after `pn_offset`.
pub const HP_SAMPLE_OFFSET: usize = 4;
/// RFC 9001 §5.8 Retry Integrity Tag length.
pub const RETRY_TAG_LEN: usize = 16;

const HEADER_FORM: u8 = 0x80;
const FIXED_BIT: u8 = 0x40;
const SPIN_BIT: u8 = 0x20;
const SHORT_RESERVED: u8 = 0x18;
const KEY_PHASE: u8 = 0x04;
const LONG_RESERVED: u8 = 0x0c;

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
    const fn bits(self) -> u8 {
        match self {
            Self::Initial => 0,
            Self::ZeroRtt => 1,
            Self::Handshake => 2,
            Self::Retry => 3,
        }
    }
    const fn from_first(first: u8) -> Self {
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

impl UnprotectedHeader {
    /// Step 4, after the AEAD open succeeded: nonzero reserved bits are a
    /// PROTOCOL_VIOLATION (RFC 9000 §17.2, §17.3.1). Checking them before
    /// authentication would let a forged header masquerade as a violation.
    pub const fn check_reserved_bits(&self) -> Result<(), HeaderError> {
        let reserved = if self.key_phase.is_some() {
            SHORT_RESERVED
        } else {
            LONG_RESERVED
        };
        if self.first_byte & reserved != 0 {
            Err(HeaderError::ReservedBitsSet)
        } else {
            Ok(())
        }
    }
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
    /// Mask applied to the first byte by header protection: low 5 bits for
    /// short headers, low 4 bits for long headers.
    pub const fn first_byte_mask(&self) -> u8 {
        if self.version.is_none() { 0x1f } else { 0x0f }
    }
    /// Step 2: read the packet-number field from `packet` *after* header
    /// protection has been removed in place. Reserved bits are deliberately
    /// not checked here: RFC 9000 §17.2/§17.3.1 requires that check only
    /// after packet protection is also removed, so call
    /// [`UnprotectedHeader::check_reserved_bits`] after a successful AEAD open.
    pub fn packet_number_field(&self, packet: &[u8]) -> Result<UnprotectedHeader, HeaderError> {
        let available = packet.len().min(self.packet_end);
        let first = *packet.first().ok_or(HeaderError::Truncated {
            needed: 1,
            available,
        })?;
        let len = PnLen::from_first_byte(first);
        let header_len = self.pn_offset + len.bytes();
        let pn = packet
            .get(..available)
            .and_then(|p| p.get(self.pn_offset..))
            .and_then(|field| TruncatedPn::read(len, field))
            .ok_or(HeaderError::Truncated {
                needed: header_len,
                available,
            })?;
        Ok(UnprotectedHeader {
            first_byte: first,
            pn,
            key_phase: self.version.is_none().then_some(first & KEY_PHASE != 0),
            header_len,
            payload: Span {
                start: header_len,
                len: self.packet_end - header_len,
            },
        })
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
    versions: &'a [u8],
}

impl<'a> VersionNegotiation<'a> {
    pub fn versions(&self) -> impl Iterator<Item = u32> + 'a {
        self.versions
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_be_bytes(*c))
    }
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
    short: WireLayout,
    long_prefix: WireLayout,
    dcid_len: usize,
}

const SHORT_FIELDS: [&str; 3] = ["flags_spin_keyphase", "dest_conn_id", "pkt_num_trunc"];

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

    /// Step 1: parse the first packet in `datagram` up to the protected
    /// packet-number boundary. All offsets are relative to `datagram`; for
    /// coalesced packets, parse again from `&datagram[packet_end..]`.
    pub fn parse<'a>(&self, datagram: &'a [u8]) -> Result<ParsedPacket<'a>, HeaderError> {
        let first = *datagram.first().ok_or(HeaderError::Truncated {
            needed: 1,
            available: 0,
        })?;
        if first & HEADER_FORM == 0 {
            self.parse_short(datagram).map(ParsedPacket::Protected)
        } else {
            self.parse_long(datagram)
        }
    }

    fn parse_short(&self, datagram: &[u8]) -> Result<ProtectedHeader, HeaderError> {
        let (view, _) = split_wire(&self.short, datagram)?;
        let first = view.read_u64(&[SHORT_FIELDS[0]])? as u8;
        if first & FIXED_BIT == 0 {
            return Err(HeaderError::FixedBitClear);
        }
        let pn_offset = 1 + self.dcid_len;
        check_sample(pn_offset, datagram.len())?;
        Ok(ProtectedHeader {
            packet_type: PacketType::OneRtt,
            version: None,
            spin: Some(first & SPIN_BIT != 0),
            dcid: Span {
                start: 1,
                len: self.dcid_len,
            },
            scid: Span::default(),
            token: Span::default(),
            pn_offset,
            packet_end: datagram.len(),
        })
    }

    fn parse_long<'a>(&self, datagram: &'a [u8]) -> Result<ParsedPacket<'a>, HeaderError> {
        let (prefix, _) = split_wire(&self.long_prefix, datagram)?;
        let first = prefix.read_u64(&["first"])? as u8;
        let version = prefix.read_u64(&["version"])? as u32;
        let dcid_len = prefix.read_u64(&["dcid_len"])? as usize;
        let mut r = Reader::new(datagram);
        r.bytes(self.long_prefix.size())
            .ok_or(HeaderError::FieldOverflow)?;
        if version != 0 {
            if first & FIXED_BIT == 0 {
                return Err(HeaderError::FixedBitClear);
            }
            if version != QUIC_V1 {
                return Err(HeaderError::UnsupportedVersion(version));
            }
            if dcid_len > MAX_CID_LEN {
                return Err(HeaderError::CidTooLong(dcid_len));
            }
        }
        let dcid_start = r.position();
        let dcid = take(&mut r, dcid_len)?;
        let scid_len = usize::from(r.u8().ok_or(HeaderError::Truncated {
            needed: r.position() + 1,
            available: datagram.len(),
        })?);
        if version != 0 && scid_len > MAX_CID_LEN {
            return Err(HeaderError::CidTooLong(scid_len));
        }
        let scid_start = r.position();
        let scid = take(&mut r, scid_len)?;
        if version == 0 {
            let versions = r.remaining();
            if versions.is_empty() || !versions.len().is_multiple_of(4) {
                return Err(HeaderError::BadVersionList);
            }
            return Ok(ParsedPacket::VersionNegotiation(VersionNegotiation {
                dcid,
                scid,
                versions,
            }));
        }
        let ty = LongPacketType::from_first(first);
        let packet_type = match ty {
            LongPacketType::Initial => PacketType::Initial,
            LongPacketType::ZeroRtt => PacketType::ZeroRtt,
            LongPacketType::Handshake => PacketType::Handshake,
            LongPacketType::Retry => {
                let rest = r.remaining();
                if rest.len() < RETRY_TAG_LEN {
                    return Err(HeaderError::Truncated {
                        needed: r.position() + RETRY_TAG_LEN,
                        available: datagram.len(),
                    });
                }
                let (token, tag) = rest.split_at(rest.len() - RETRY_TAG_LEN);
                if token.is_empty() {
                    return Err(HeaderError::EmptyRetryToken);
                }
                let integrity_tag = tag.try_into().map_err(|_| HeaderError::FieldOverflow)?;
                return Ok(ParsedPacket::Retry(RetryPacket {
                    version,
                    dcid,
                    scid,
                    token,
                    integrity_tag,
                }));
            }
        };
        let mut token = Span::default();
        if ty == LongPacketType::Initial {
            let token_len = r.varint()?;
            let start = r.position();
            let len = usize::try_from(token_len).map_err(|_| HeaderError::FieldOverflow)?;
            take(&mut r, len)?;
            token = Span { start, len };
        }
        let length = r.varint()?;
        let pn_offset = r.position();
        let available = datagram.len() - pn_offset;
        let packet_end = usize::try_from(length)
            .ok()
            .filter(|len| *len <= available)
            .map(|len| pn_offset + len)
            .ok_or(HeaderError::LengthExceedsDatagram { length, available })?;
        check_sample(pn_offset, packet_end)?;
        Ok(ParsedPacket::Protected(ProtectedHeader {
            packet_type,
            version: Some(version),
            spin: None,
            dcid: Span {
                start: dcid_start,
                len: dcid_len,
            },
            scid: Span {
                start: scid_start,
                len: scid_len,
            },
            token,
            pn_offset,
            packet_end,
        }))
    }

    /// Writes an unprotected short header; returns the header length (the
    /// payload starts there). `out` must hold at least the 4-byte
    /// packet-number window of `QuicShortHdrWire`.
    pub fn write_short(
        &self,
        out: &mut [u8],
        dcid: &[u8],
        spin: bool,
        key_phase: bool,
        full_pn: u64,
        pn_len: PnLen,
    ) -> Result<usize, HeaderError> {
        if dcid.len() != self.dcid_len {
            return Err(HeaderError::BadConfiguredCidLen(dcid.len()));
        }
        let (mut view, _) = split_wire_mut(&self.short, out)?;
        let first = FIXED_BIT
            | if spin { SPIN_BIT } else { 0 }
            | if key_phase { KEY_PHASE } else { 0 }
            | pn_len.first_byte_bits();
        view.write_u64(&[SHORT_FIELDS[0]], u64::from(first))?;
        view.bytes_mut(&[SHORT_FIELDS[1]])?.copy_from_slice(dcid);
        let pn_offset = 1 + self.dcid_len;
        crate::packet_number::write_truncated(full_pn, pn_len, &mut out[pn_offset..])
            .map_err(|_| HeaderError::FieldOverflow)?;
        Ok(pn_offset + pn_len.bytes())
    }
}

fn take<'a>(r: &mut Reader<'a>, len: usize) -> Result<&'a [u8], HeaderError> {
    let needed = r.position().saturating_add(len);
    let available = r.position() + r.remaining().len();
    r.bytes(len)
        .ok_or(HeaderError::Truncated { needed, available })
}

fn check_sample(pn_offset: usize, end: usize) -> Result<(), HeaderError> {
    let needed = pn_offset + HP_SAMPLE_OFFSET + HP_SAMPLE_LEN;
    if end < needed {
        Err(HeaderError::TooShortForSample {
            needed,
            available: end,
        })
    } else {
        Ok(())
    }
}

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

fn cid_len(cid: &[u8]) -> Result<u8, HeaderError> {
    if cid.len() > MAX_CID_LEN {
        Err(HeaderError::CidTooLong(cid.len()))
    } else {
        Ok(cid.len() as u8)
    }
}

/// Writes an unprotected version 1 long header (not Retry).
pub fn write_long(out: &mut [u8], h: &LongHeader<'_>) -> Result<WrittenLongHeader, HeaderError> {
    if h.ty == LongPacketType::Retry {
        return Err(HeaderError::WrongForm);
    }
    if h.ty != LongPacketType::Initial && !h.token.is_empty() {
        return Err(HeaderError::FieldOverflow);
    }
    let mut w = Writer::new(out);
    w.u8(HEADER_FORM | FIXED_BIT | (h.ty.bits() << 4) | h.pn_len.first_byte_bits())?;
    w.bytes(&QUIC_V1.to_be_bytes())?;
    w.u8(cid_len(h.dcid)?)?;
    w.bytes(h.dcid)?;
    w.u8(cid_len(h.scid)?)?;
    w.bytes(h.scid)?;
    if h.ty == LongPacketType::Initial {
        w.varint(h.token.len() as u64)?;
        w.bytes(h.token)?;
    }
    let length = h
        .pn_len
        .bytes()
        .checked_add(h.payload_len)
        .ok_or(HeaderError::FieldOverflow)?;
    if length > varint::MAX as usize {
        return Err(HeaderError::FieldOverflow);
    }
    w.varint(length as u64)?;
    let pn_offset = w.position();
    let mut pn = [0u8; 4];
    crate::packet_number::write_truncated(h.full_pn, h.pn_len, &mut pn)
        .map_err(|_| HeaderError::FieldOverflow)?;
    w.bytes(&pn[..h.pn_len.bytes()])?;
    let header_len = w.position();
    Ok(WrittenLongHeader {
        pn_offset,
        header_len,
        packet_len: header_len + h.payload_len,
    })
}

/// Writes a Retry packet; the integrity tag is computed by packet protection.
pub fn write_retry(
    out: &mut [u8],
    dcid: &[u8],
    scid: &[u8],
    token: &[u8],
    integrity_tag: &[u8; RETRY_TAG_LEN],
) -> Result<usize, HeaderError> {
    if token.is_empty() {
        return Err(HeaderError::EmptyRetryToken);
    }
    let mut w = Writer::new(out);
    w.u8(HEADER_FORM | FIXED_BIT | (LongPacketType::Retry.bits() << 4))?;
    w.bytes(&QUIC_V1.to_be_bytes())?;
    w.u8(cid_len(dcid)?)?;
    w.bytes(dcid)?;
    w.u8(cid_len(scid)?)?;
    w.bytes(scid)?;
    w.bytes(token)?;
    w.bytes(integrity_tag)?;
    Ok(w.position())
}

/// Writes a Version Negotiation packet. `unused` supplies the seven
/// arbitrary low bits of the first byte (RFC 9000 §17.2.1).
pub fn write_version_negotiation(
    out: &mut [u8],
    unused: u8,
    dcid: &[u8],
    scid: &[u8],
    versions: &[u32],
) -> Result<usize, HeaderError> {
    if versions.is_empty() {
        return Err(HeaderError::BadVersionList);
    }
    let len = |cid: &[u8]| u8::try_from(cid.len()).map_err(|_| HeaderError::FieldOverflow);
    let mut w = Writer::new(out);
    w.u8(HEADER_FORM | (unused & 0x7f))?;
    w.bytes(&0u32.to_be_bytes())?;
    w.u8(len(dcid)?)?;
    w.bytes(dcid)?;
    w.u8(len(scid)?)?;
    w.bytes(scid)?;
    for v in versions {
        w.bytes(&v.to_be_bytes())?;
    }
    Ok(w.position())
}
