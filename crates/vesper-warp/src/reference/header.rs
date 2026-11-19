//! The Rust reference implementation of the ported functions of
//! [`crate::header`], compiled without `vesper-port` (the Vesper build is
//! `vesper/src/header.ves`). Frozen: bug fixes only (#1066).

use crate::PacketType;
use crate::header::*;
use crate::packet_number::{PnLen, TruncatedPn};
use crate::varint::{self, Reader, Writer};
use vesper_ffi::{split_wire, split_wire_mut};

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

impl<'a> VersionNegotiation<'a> {
    pub fn versions(&self) -> impl Iterator<Item = u32> + 'a {
        self.versions
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_be_bytes(*c))
    }
}

impl HeaderParser {
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

    /// Like [`Self::write_short`], but for a Destination Connection ID of
    /// any length up to [`MAX_CID_LEN`]: the peer's connection IDs need not
    /// be as long as the ones this endpoint issues (and parses).
    pub fn write_short_to_peer(
        &self,
        out: &mut [u8],
        dcid: &[u8],
        spin: bool,
        key_phase: bool,
        full_pn: u64,
        pn_len: PnLen,
    ) -> Result<usize, HeaderError> {
        if dcid.len() == self.dcid_len {
            return self.write_short(out, dcid, spin, key_phase, full_pn, pn_len);
        }
        if dcid.len() > MAX_CID_LEN {
            return Err(HeaderError::CidTooLong(dcid.len()));
        }
        let pn_offset = 1 + dcid.len();
        let needed = pn_offset + 4;
        if out.len() < needed {
            return Err(HeaderError::Truncated {
                needed,
                available: out.len(),
            });
        }
        out[0] = FIXED_BIT
            | if spin { SPIN_BIT } else { 0 }
            | if key_phase { KEY_PHASE } else { 0 }
            | pn_len.first_byte_bits();
        out[1..pn_offset].copy_from_slice(dcid);
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
