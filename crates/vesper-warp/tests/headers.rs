//! Header parse/serialize, the header-protection boundary, exhaustive
//! truncation and random-byte robustness.

mod common;
use common::Rng;
use vesper_warp::PacketType;
use vesper_warp::header::{
    HP_SAMPLE_LEN, HeaderError, HeaderParser, LongHeader, LongPacketType, ParsedPacket, QUIC_V1,
    write_long, write_retry, write_version_negotiation,
};
use vesper_warp::packet_number::{PnLen, decode_with_largest};

const DCID: [u8; 8] = [0xd0, 0xd1, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7];

/// Stand-in for lane 5: XOR a mask over the first byte's protected bits and
/// the 4-byte packet-number window, as RFC 9001 §5.4.1 does.
fn toggle_header_protection(packet: &mut [u8], pn_offset: usize, first_mask: u8, mask: [u8; 5]) {
    packet[0] ^= mask[0] & first_mask;
    // Real HP masks only the pn_len bytes; the length is in the protected
    // first byte, so the test masks the true length.
    let len = usize::from(packet[0] & 0x03) + 1;
    for i in 0..len {
        packet[pn_offset + i] ^= mask[1 + i];
    }
}

#[test]
fn short_header_matches_section_35_2_wire_struct() {
    let parser = HeaderParser::new(8).unwrap();
    assert_eq!(
        parser.short_layout().size(),
        13,
        "QuicShortHdrWire is 13 bytes"
    );
    let mut packet = [0u8; 64];
    let pn_len = PnLen::new(2).unwrap();
    let header_len = parser
        .write_short(&mut packet, &DCID, true, true, 0xa82f_9b32, pn_len)
        .unwrap();
    assert_eq!(header_len, 1 + 8 + 2);
    packet[header_len..].fill(0xee);

    let mask = [0x1b, 0x11, 0x22, 0x33, 0x44];
    toggle_header_protection(&mut packet, 9, 0x1f, mask);
    let ParsedPacket::Protected(h) = parser.parse(&packet).unwrap() else {
        panic!("short header")
    };
    assert_eq!(h.packet_type, PacketType::OneRtt);
    assert_eq!(h.dcid(&packet), &DCID);
    assert_eq!(h.spin, Some(true));
    assert_eq!(h.pn_offset, 9);
    assert_eq!(h.sample().start, 13);
    assert_eq!(h.sample().len, HP_SAMPLE_LEN);
    assert_eq!(h.first_byte_mask(), 0x1f);
    assert_eq!(h.packet_end, 64);

    // Lane 5 removes header protection in place; then the PN field is read
    // and the full number reconstructed *before* AEAD open (nonce input).
    toggle_header_protection(&mut packet, h.pn_offset, h.first_byte_mask(), mask);
    let u = h.packet_number_field(&packet).unwrap();
    assert_eq!(u.pn.len, pn_len);
    assert_eq!(u.pn.value, 0x9b32);
    assert_eq!(u.key_phase, Some(true));
    assert_eq!(u.header_len, header_len);
    assert_eq!(u.payload.start, header_len);
    assert_eq!(u.payload.end(), 64);
    assert_eq!(u.pn.decode(Some(0xa82f_30ea)), Ok(0xa82f_9b32));
    assert_eq!(
        decode_with_largest(Some(0xa82f_30ea), 0x9b32, 16),
        Ok(0xa82f_9b32)
    );
}

#[test]
fn reserved_bits_after_unprotect_are_rejected() {
    let parser = HeaderParser::new(8).unwrap();
    let mut packet = [0u8; 40];
    parser
        .write_short(&mut packet, &DCID, false, false, 7, PnLen::new(1).unwrap())
        .unwrap();
    packet[0] |= 0x08;
    let ParsedPacket::Protected(h) = parser.parse(&packet).unwrap() else {
        panic!()
    };
    assert_eq!(
        h.packet_number_field(&packet),
        Err(HeaderError::ReservedBitsSet)
    );
}

#[test]
fn configured_dcid_lengths() {
    for len in [0usize, 1, 8, 20] {
        let parser = HeaderParser::new(len).unwrap();
        assert_eq!(parser.short_layout().size(), 1 + len + 4);
        let dcid = vec![0xab; len];
        let mut packet = vec![0u8; 1 + len + 4 + HP_SAMPLE_LEN];
        parser
            .write_short(&mut packet, &dcid, false, false, 1, PnLen::new(4).unwrap())
            .unwrap();
        let ParsedPacket::Protected(h) = parser.parse(&packet).unwrap() else {
            panic!()
        };
        assert_eq!(h.dcid(&packet), dcid.as_slice());
        // One byte short of the sample is rejected.
        assert!(matches!(
            parser.parse(&packet[..packet.len() - 1]),
            Err(HeaderError::TooShortForSample { .. })
        ));
    }
    assert_eq!(
        HeaderParser::new(21).unwrap_err(),
        HeaderError::BadConfiguredCidLen(21)
    );
}

fn long_packet(ty: LongPacketType, token: &[u8], payload_len: usize) -> (Vec<u8>, usize) {
    let mut out = vec![0u8; 256];
    let h = LongHeader {
        ty,
        dcid: &DCID,
        scid: &[1, 2, 3, 4, 5],
        token,
        full_pn: 0x1234,
        pn_len: PnLen::new(2).unwrap(),
        payload_len,
    };
    let w = write_long(&mut out, &h).unwrap();
    out.truncate(w.packet_len);
    (out, w.pn_offset)
}

#[test]
fn long_headers_roundtrip_and_coalesce() {
    let parser = HeaderParser::new(8).unwrap();
    for (ty, pt, token) in [
        (LongPacketType::Initial, PacketType::Initial, &b"tok"[..]),
        (LongPacketType::Initial, PacketType::Initial, &b""[..]),
        (LongPacketType::ZeroRtt, PacketType::ZeroRtt, &b""[..]),
        (LongPacketType::Handshake, PacketType::Handshake, &b""[..]),
    ] {
        let (first, pn_offset) = long_packet(ty, token, 40);
        let (second, _) = long_packet(LongPacketType::Handshake, b"", 30);
        let mut datagram = first.clone();
        datagram.extend_from_slice(&second);
        datagram.extend_from_slice(&[0u8; 7]); // trailing bytes after the packets
        let ParsedPacket::Protected(h) = parser.parse(&datagram).unwrap() else {
            panic!()
        };
        assert_eq!(h.packet_type, pt);
        assert_eq!(h.version, Some(QUIC_V1));
        assert_eq!(h.dcid(&datagram), &DCID);
        assert_eq!(h.scid(&datagram), &[1, 2, 3, 4, 5]);
        assert_eq!(h.token(&datagram), token);
        assert_eq!(h.pn_offset, pn_offset);
        assert_eq!(h.packet_end, first.len());
        assert_eq!(h.first_byte_mask(), 0x0f);
        let u = h.packet_number_field(&datagram).unwrap();
        assert_eq!(u.pn.value, 0x1234);
        assert_eq!(u.key_phase, None);
        assert_eq!(u.payload.len, 40);
        let ParsedPacket::Protected(next) = parser.parse(&datagram[h.packet_end..]).unwrap() else {
            panic!()
        };
        assert_eq!(next.packet_type, PacketType::Handshake);
        assert_eq!(next.packet_end, second.len());
    }
}

#[test]
fn long_header_errors() {
    let parser = HeaderParser::new(8).unwrap();
    let (mut p, _) = long_packet(LongPacketType::Handshake, b"", 40);
    p[0] &= !0x40;
    assert_eq!(parser.parse(&p), Err(HeaderError::FixedBitClear));
    let (mut p, _) = long_packet(LongPacketType::Handshake, b"", 40);
    p[1..5].copy_from_slice(&0xff00_001du32.to_be_bytes());
    assert_eq!(
        parser.parse(&p),
        Err(HeaderError::UnsupportedVersion(0xff00_001d))
    );
    let (mut p, _) = long_packet(LongPacketType::Handshake, b"", 40);
    p[5] = 21;
    assert_eq!(parser.parse(&p), Err(HeaderError::CidTooLong(21)));
    // Length larger than the datagram.
    let (p, _) = long_packet(LongPacketType::Handshake, b"", 40);
    assert!(matches!(
        parser.parse(&p[..p.len() - 1]),
        Err(HeaderError::LengthExceedsDatagram { .. })
    ));
    // Payload too short for the header-protection sample.
    let (p, _) = long_packet(LongPacketType::Handshake, b"", 17);
    assert!(matches!(
        parser.parse(&p),
        Err(HeaderError::TooShortForSample { .. })
    ));
    assert!(
        write_long(
            &mut [0u8; 64],
            &LongHeader {
                ty: LongPacketType::Handshake,
                dcid: &[0; 21],
                scid: &[],
                token: &[],
                full_pn: 0,
                pn_len: PnLen::new(1).unwrap(),
                payload_len: 20,
            }
        )
        .is_err()
    );
}

#[test]
fn retry_and_version_negotiation() {
    let parser = HeaderParser::new(8).unwrap();
    let mut out = [0u8; 128];
    let tag = [0x5a; 16];
    let n = write_retry(&mut out, &DCID, &[9, 9], b"retry-token", &tag).unwrap();
    let ParsedPacket::Retry(r) = parser.parse(&out[..n]).unwrap() else {
        panic!()
    };
    assert_eq!(r.version, QUIC_V1);
    assert_eq!(r.dcid, &DCID);
    assert_eq!(r.scid, &[9, 9]);
    assert_eq!(r.token, b"retry-token");
    assert_eq!(r.integrity_tag, &tag);
    // Token-less Retry is discarded.
    let n = write_retry(&mut out, &DCID, &[], b"x", &tag).unwrap();
    // Dropping one byte leaves exactly the 16-byte tag after the SCID.
    assert_eq!(
        parser.parse(&out[..n - 1]),
        Err(HeaderError::EmptyRetryToken)
    );

    let n = write_version_negotiation(&mut out, 0x55, &[1; 30], &DCID, &[QUIC_V1, 0x6b33_43cf])
        .unwrap();
    let ParsedPacket::VersionNegotiation(vn) = parser.parse(&out[..n]).unwrap() else {
        panic!()
    };
    assert_eq!(vn.dcid.len(), 30, "VN may echo CIDs longer than 20 bytes");
    assert_eq!(vn.scid, &DCID);
    assert_eq!(vn.versions().collect::<Vec<_>>(), [QUIC_V1, 0x6b33_43cf]);
    assert_eq!(
        parser.parse(&out[..n - 1]),
        Err(HeaderError::BadVersionList)
    );
    assert_eq!(
        parser.parse(&out[..n - 8]),
        Err(HeaderError::BadVersionList)
    );
}

fn valid_packets(parser: &HeaderParser) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    for (ty, token) in [
        (LongPacketType::Initial, &b"abc"[..]),
        (LongPacketType::ZeroRtt, &b""[..]),
        (LongPacketType::Handshake, &b""[..]),
    ] {
        packets.push(long_packet(ty, token, 24).0);
    }
    let mut short = vec![0u8; 13 + HP_SAMPLE_LEN];
    parser
        .write_short(&mut short, &DCID, false, true, 3, PnLen::new(3).unwrap())
        .unwrap();
    packets.push(short);
    let mut out = [0u8; 64];
    let n = write_retry(&mut out, &DCID, &[7], b"tk", &[1; 16]).unwrap();
    packets.push(out[..n].to_vec());
    let n = write_version_negotiation(&mut out, 0, &DCID, &[], &[QUIC_V1]).unwrap();
    packets.push(out[..n].to_vec());
    packets
}

#[test]
fn every_truncation_point_is_an_error_not_a_panic() {
    let parser = HeaderParser::new(8).unwrap();
    for packet in valid_packets(&parser) {
        assert!(parser.parse(&packet).is_ok());
        for cut in 0..packet.len() {
            let result = parser.parse(&packet[..cut]);
            if let Ok(parsed) = result {
                // Packets without a length field run to the datagram end: a
                // shorter VN is a VN with fewer versions (at 4-byte steps) and
                // a shorter Retry has a shorter token, which the Retry
                // Integrity Tag (packet protection) then rejects. Nothing
                // else may parse.
                assert!(
                    matches!(parsed, ParsedPacket::VersionNegotiation(_))
                        && (packet.len() - cut).is_multiple_of(4)
                        || matches!(parsed, ParsedPacket::Retry(r) if !r.token.is_empty()),
                    "truncated parse succeeded at {cut}: {parsed:?}"
                );
            }
        }
    }
}

#[test]
fn random_bytes_never_panic() {
    let parser = HeaderParser::new(8).unwrap();
    let mut rng = Rng(0x77a2_0101);
    let seeds = valid_packets(&parser);
    for i in 0..200_000u32 {
        let mut packet = if i.is_multiple_of(2) {
            let len = rng.below(80) as usize;
            rng.bytes(len)
        } else {
            let mut p = seeds[rng.below(seeds.len() as u64) as usize].clone();
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(p.len() as u64) as usize;
                p[at] = rng.next() as u8;
            }
            p
        };
        if let Ok(ParsedPacket::Protected(h)) = parser.parse(&packet) {
            assert!(h.pn_offset + 4 + HP_SAMPLE_LEN <= h.packet_end);
            assert!(h.packet_end <= packet.len());
            packet[0] &= !0x18; // pretend HP removal left reserved bits clear
            let _ = h.packet_number_field(&packet);
            let _ = h.packet_number_field(&packet[..h.pn_offset.min(packet.len())]);
        }
    }
}
