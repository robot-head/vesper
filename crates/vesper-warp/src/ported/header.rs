//! [`crate::header`]'s ported functions, forwarded to
//! `vesper/src/header.ves`. The Vesper parser returns spans of the
//! datagram, turned back into slices here.

use super::binding::{self as b, repr};
use super::packet_type_to;
use super::varint::varint_error;
use crate::PacketType;
use crate::header::{
    HeaderError, HeaderParser, LongHeader, LongPacketType, ParsedPacket, ProtectedHeader,
    RETRY_TAG_LEN, RetryPacket, Span, UnprotectedHeader, VersionNegotiation, WrittenLongHeader,
};
use crate::packet_number::{PnLen, TruncatedPn};
use vesper_port_abi::Word;

fn us(v: u64) -> usize {
    v as usize
}

pub(crate) fn header_error(e: b::HdrErr) -> HeaderError {
    use b::HdrErr as H;
    match e {
        H::Truncated { needed, available } => HeaderError::Truncated {
            needed: us(needed),
            available: us(available),
        },
        H::FixedBitClear => HeaderError::FixedBitClear,
        H::UnsupportedVersion { version } => HeaderError::UnsupportedVersion(version),
        H::CidTooLong { len } => HeaderError::CidTooLong(us(len)),
        H::BadConfiguredCidLen { len } => HeaderError::BadConfiguredCidLen(us(len)),
        H::LengthExceedsDatagram { length, available } => HeaderError::LengthExceedsDatagram {
            length,
            available: us(available),
        },
        H::TooShortForSample { needed, available } => HeaderError::TooShortForSample {
            needed: us(needed),
            available: us(available),
        },
        H::ReservedBitsSet => HeaderError::ReservedBitsSet,
        H::BadVersionList => HeaderError::BadVersionList,
        H::EmptyRetryToken => HeaderError::EmptyRetryToken,
        H::WrongForm => HeaderError::WrongForm,
        H::VarInt { e } => HeaderError::VarInt(varint_error(e)),
        H::BufferTooSmall { needed, available } => HeaderError::BufferTooSmall {
            needed: us(needed),
            available: us(available),
        },
        H::FieldOverflow => HeaderError::FieldOverflow,
    }
}

fn span(s: b::Sp) -> Span {
    Span {
        start: us(s.start),
        len: us(s.len),
    }
}

fn span_m(s: repr::Sp) -> Span {
    Span {
        start: us(s.start),
        len: us(s.len),
    }
}

fn sp(s: Span) -> repr::Sp {
    repr::Sp {
        start: s.start as u64,
        len: s.len as u64,
    }
}

fn packet_type_from(t: b::PktType) -> PacketType {
    match t {
        b::PktType::Initial => PacketType::Initial,
        b::PktType::ZeroRtt => PacketType::ZeroRtt,
        b::PktType::Handshake => PacketType::Handshake,
        b::PktType::OneRtt => PacketType::OneRtt,
    }
}

fn protected_from(h: b::Prot) -> ProtectedHeader {
    ProtectedHeader {
        packet_type: packet_type_from(h.packet_type),
        version: h.has_version.then_some(h.version),
        spin: h.has_spin.then_some(h.spin),
        dcid: span(h.dcid),
        scid: span(h.scid),
        token: span(h.token),
        pn_offset: us(h.pn_offset),
        packet_end: us(h.packet_end),
    }
}

fn protected_to(h: &ProtectedHeader) -> repr::Prot {
    repr::Prot {
        packet_type: Word::new(packet_type_to(h.packet_type)),
        has_version: Word::new(h.version.is_some()),
        version: Word::new(h.version.unwrap_or(0)),
        has_spin: Word::new(h.spin.is_some()),
        spin: Word::new(h.spin.unwrap_or(false)),
        dcid: sp(h.dcid),
        scid: sp(h.scid),
        token: sp(h.token),
        pn_offset: h.pn_offset as u64,
        packet_end: h.packet_end as u64,
    }
}

fn slice(buf: &[u8], s: b::Sp) -> &[u8] {
    &buf[us(s.start)..us(s.start + s.len)]
}

impl UnprotectedHeader {
    /// Step 4, after the AEAD open succeeded: nonzero reserved bits are a
    /// PROTOCOL_VIOLATION (RFC 9000 §17.2, §17.3.1). Checking them before
    /// authentication would let a forged header masquerade as a violation.
    pub fn check_reserved_bits(&self) -> Result<(), HeaderError> {
        b::header_check_reserved_bits(self.first_byte, self.key_phase.is_some())
            .map_err(header_error)
    }
}

impl ProtectedHeader {
    /// Mask applied to the first byte by header protection: low 5 bits for
    /// short headers, low 4 bits for long headers.
    pub fn first_byte_mask(&self) -> u8 {
        b::header_first_byte_mask(self.version.is_some())
    }
    /// Step 2: read the packet-number field from `packet` *after* header
    /// protection has been removed in place. Reserved bits are deliberately
    /// not checked here: RFC 9000 §17.2/§17.3.1 requires that check only
    /// after packet protection is also removed, so call
    /// [`UnprotectedHeader::check_reserved_bits`] after a successful AEAD open.
    pub fn packet_number_field(&self, packet: &[u8]) -> Result<UnprotectedHeader, HeaderError> {
        let u = b::header_pn_field(&protected_to(self), packet).map_err(header_error)?;
        Ok(UnprotectedHeader {
            first_byte: u.first_byte.get(),
            pn: TruncatedPn {
                len: PnLen(u.pn_len as u8),
                value: u.pn.get(),
            },
            key_phase: u.has_key_phase.get().then_some(u.key_phase.get()),
            header_len: us(u.header_len),
            payload: span_m(u.payload),
        })
    }
}

impl<'a> VersionNegotiation<'a> {
    pub fn versions(&self) -> impl Iterator<Item = u32> + 'a {
        let versions = self.versions;
        (0..versions.len() / 4).map(move |i| b::header_vn_version(versions, i as u64 * 4))
    }
}

impl HeaderParser {
    /// Step 1: parse the first packet in `datagram` up to the protected
    /// packet-number boundary. All offsets are relative to `datagram`; for
    /// coalesced packets, parse again from `&datagram[packet_end..]`.
    pub fn parse<'a>(&self, datagram: &'a [u8]) -> Result<ParsedPacket<'a>, HeaderError> {
        let parsed = b::header_parse(datagram, self.dcid_len as u64).map_err(header_error)?;
        Ok(match parsed {
            b::Parsed::Protected { h } => ParsedPacket::Protected(protected_from(h)),
            b::Parsed::Retry {
                version,
                dcid,
                scid,
                token,
                tag,
            } => {
                let tag = us(tag);
                let integrity_tag: &'a [u8; RETRY_TAG_LEN] = datagram[tag..tag + RETRY_TAG_LEN]
                    .try_into()
                    .map_err(|_| HeaderError::FieldOverflow)?;
                ParsedPacket::Retry(RetryPacket {
                    version,
                    dcid: slice(datagram, dcid),
                    scid: slice(datagram, scid),
                    token: slice(datagram, token),
                    integrity_tag,
                })
            }
            b::Parsed::VersionNegotiation {
                dcid,
                scid,
                versions,
            } => ParsedPacket::VersionNegotiation(VersionNegotiation {
                dcid: slice(datagram, dcid),
                scid: slice(datagram, scid),
                versions: slice(datagram, versions),
            }),
        })
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
        b::header_write_short(
            out,
            dcid,
            self.dcid_len as u64,
            spin,
            key_phase,
            full_pn,
            pn_len.bytes() as u64,
        )
        .map(us)
        .map_err(header_error)
    }

    /// Like [`Self::write_short`], but for a Destination Connection ID of
    /// any length up to [`crate::header::MAX_CID_LEN`]: the peer's
    /// connection IDs need not be as long as the ones this endpoint issues
    /// (and parses).
    pub fn write_short_to_peer(
        &self,
        out: &mut [u8],
        dcid: &[u8],
        spin: bool,
        key_phase: bool,
        full_pn: u64,
        pn_len: PnLen,
    ) -> Result<usize, HeaderError> {
        b::header_write_short_to_peer(
            out,
            dcid,
            self.dcid_len as u64,
            spin,
            key_phase,
            full_pn,
            pn_len.bytes() as u64,
        )
        .map(us)
        .map_err(header_error)
    }
}

/// Writes an unprotected version 1 long header (not Retry).
pub fn write_long(out: &mut [u8], h: &LongHeader<'_>) -> Result<WrittenLongHeader, HeaderError> {
    let ty = match h.ty {
        LongPacketType::Initial => 0,
        LongPacketType::ZeroRtt => 1,
        LongPacketType::Handshake => 2,
        LongPacketType::Retry => 3,
    };
    let w = b::header_write_long(
        out,
        ty,
        h.dcid,
        h.scid,
        h.token,
        h.full_pn,
        h.pn_len.bytes() as u64,
        h.payload_len as u64,
    )
    .map_err(header_error)?;
    Ok(WrittenLongHeader {
        pn_offset: us(w.pn_offset),
        header_len: us(w.header_len),
        packet_len: us(w.packet_len),
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
    b::header_write_retry(out, dcid, scid, token, integrity_tag)
        .map(us)
        .map_err(header_error)
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
    b::header_write_version_negotiation(out, unused, dcid, scid, versions)
        .map(us)
        .map_err(header_error)
}
