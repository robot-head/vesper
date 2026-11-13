//! Frame parse/encode round trips, Table 3 permissions, truncation at every
//! byte and random-byte robustness.

mod common;
use common::Rng;
use vesper_warp::PacketType;
use vesper_warp::frame::{
    AckFrame, AckRange, ConnectionClose, EcnCounts, Frame, FrameError, StreamFrame, frames,
    parse_frame, scan_payload,
};

const ALL_TYPES: [PacketType; 4] = [
    PacketType::Initial,
    PacketType::ZeroRtt,
    PacketType::Handshake,
    PacketType::OneRtt,
];

fn ack_ranges() -> [AckRange; 3] {
    [
        AckRange {
            smallest: 95,
            largest: 100,
        },
        AckRange {
            smallest: 80,
            largest: 90,
        },
        AckRange {
            smallest: 0,
            largest: 3,
        },
    ]
}

fn sample_frames<'a>(ranges: &'a [AckRange]) -> Vec<Frame<'a>> {
    let mut v = vec![
        Frame::Padding { len: 3 },
        Frame::Ping,
        Frame::Ack(AckFrame::from_ranges(ranges, 1234, None).unwrap()),
        Frame::Ack(
            AckFrame::from_ranges(
                &ranges[..1],
                0,
                Some(EcnCounts {
                    ect0: 5,
                    ect1: 0,
                    ce: 70_000,
                }),
            )
            .unwrap(),
        ),
        Frame::Crypto {
            offset: 1 << 40,
            data: b"client hello",
        },
        Frame::MaxData(1 << 30),
        Frame::MaxStreamData {
            stream_id: 4,
            max: 65_536,
        },
        Frame::ConnectionClose(ConnectionClose {
            error_code: 0x0a,
            frame_type: Some(0x08),
            reason: b"bad",
        }),
        Frame::ConnectionClose(ConnectionClose {
            error_code: 7,
            frame_type: None,
            reason: b"",
        }),
        Frame::HandshakeDone,
    ];
    for (offset, fin) in [(0, false), (0, true), (77, false), (1 << 50, true)] {
        v.push(Frame::Stream(StreamFrame {
            stream_id: 8,
            offset,
            data: b"stream data",
            fin,
            has_length: true,
        }));
    }
    v
}

fn parse_all(buf: &[u8], pt: PacketType) -> Result<Vec<Frame<'_>>, FrameError> {
    frames(buf, pt).collect()
}

#[test]
fn roundtrip_every_supported_frame() {
    let ranges = ack_ranges();
    for frame in sample_frames(&ranges) {
        let mut buf = [0u8; 128];
        let n = frame.encode(&mut buf).unwrap();
        let (parsed, used) = parse_frame(&buf[..n], PacketType::OneRtt).unwrap();
        assert_eq!(used, n, "{frame:?}");
        // ACK frames compare by content: wire vs decoded representation.
        match (&frame, &parsed) {
            (Frame::Ack(a), Frame::Ack(b)) => {
                assert_eq!(a.largest, b.largest);
                assert_eq!(a.ack_delay, b.ack_delay);
                assert_eq!(a.ecn, b.ecn);
                assert_eq!(a.range_count(), b.range_count());
                assert!(a.ranges().eq(b.ranges()));
                assert_eq!(b.smallest(), a.ranges().last().unwrap().smallest);
            }
            _ => assert_eq!(frame, parsed),
        }
        // Encoding the parsed frame reproduces the bytes.
        let mut again = [0u8; 128];
        assert_eq!(parsed.encode(&mut again), Ok(n));
        assert_eq!(&again[..n], &buf[..n]);
        // Too-small output buffers are an error.
        assert_eq!(frame.encode(&mut buf[..n - 1]), Err(FrameError::Encode));
    }
}

#[test]
fn stream_without_length_runs_to_end_and_all_type_bits() {
    for ty in 0x08u8..=0x0f {
        let mut buf = vec![ty, 0x04];
        if ty & 0x04 != 0 {
            buf.push(0x05);
        }
        if ty & 0x02 != 0 {
            buf.push(0x03);
        }
        buf.extend_from_slice(b"abc");
        let (frame, used) = parse_frame(&buf, PacketType::OneRtt).unwrap();
        assert_eq!(used, buf.len());
        let Frame::Stream(s) = frame else { panic!() };
        assert_eq!(s.stream_id, 4);
        assert_eq!(s.offset, if ty & 0x04 != 0 { 5 } else { 0 });
        assert_eq!(s.fin, ty & 0x01 != 0);
        assert_eq!(s.has_length, ty & 0x02 != 0);
        assert_eq!(s.data, b"abc");
    }
}

#[test]
fn ack_wire_ranges_decode() {
    // largest 100, delay 2, 2 extra ranges, first range 5 → [95,100];
    // gap 3, len 10 → largest 95-3-2=90, [80,90]; gap 75, len 3 → [0,3].
    let wire = [
        0x02, 0x40, 0x64, 0x02, 0x02, 0x05, 0x03, 0x0a, 0x40, 0x4b, 0x03,
    ];
    let (Frame::Ack(a), _) = parse_frame(&wire, PacketType::Initial).unwrap() else {
        panic!()
    };
    assert_eq!(a.ranges().collect::<Vec<_>>(), ack_ranges());
    // A range running below zero is malformed.
    let bad = [0x02, 0x05, 0x00, 0x00, 0x06];
    assert_eq!(
        parse_frame(&bad, PacketType::Initial),
        Err(FrameError::Malformed { frame_type: 2 })
    );
    let bad = [0x02, 0x05, 0x00, 0x01, 0x00, 0x04, 0x00];
    assert_eq!(
        parse_frame(&bad, PacketType::Initial),
        Err(FrameError::Malformed { frame_type: 2 })
    );
    // from_ranges rejects adjacent or unordered ranges.
    let adjacent = [
        AckRange {
            smallest: 5,
            largest: 6,
        },
        AckRange {
            smallest: 1,
            largest: 4,
        },
    ];
    assert!(AckFrame::from_ranges(&adjacent, 0, None).is_err());
    assert!(AckFrame::from_ranges(&[], 0, None).is_err());
}

#[test]
fn table_3_permissions() {
    let ranges = ack_ranges();
    for frame in sample_frames(&ranges) {
        let mut buf = [0u8; 128];
        let n = frame.encode(&mut buf).unwrap();
        for pt in ALL_TYPES {
            let expected = match frame {
                Frame::Padding { .. } | Frame::Ping => true,
                Frame::ConnectionClose(c) if c.frame_type.is_some() => true,
                Frame::Ack(_) | Frame::Crypto { .. } => pt != PacketType::ZeroRtt,
                Frame::HandshakeDone => pt == PacketType::OneRtt,
                _ => matches!(pt, PacketType::ZeroRtt | PacketType::OneRtt),
            };
            let result = parse_frame(&buf[..n], pt);
            assert_eq!(result.is_ok(), expected, "{frame:?} in {pt:?}");
            if !expected {
                assert!(matches!(result, Err(FrameError::NotAllowed { .. })));
            }
        }
    }
}

#[test]
fn unknown_unsupported_and_non_minimal_types() {
    assert_eq!(
        parse_frame(&[0x1f], PacketType::OneRtt),
        Err(FrameError::UnknownType(0x1f))
    );
    assert_eq!(
        parse_frame(&[0x41, 0x00], PacketType::OneRtt),
        Err(FrameError::UnknownType(0x100))
    );
    // Wave six implements the last RFC 9000 frames; short or malformed
    // encodings of them are typed errors.
    for ty in [0x07u8, 0x18, 0x19, 0x1a, 0x1b] {
        let wire = [ty, 0, 0, 0];
        let r = parse_frame(&wire, PacketType::OneRtt);
        if ty == 0x19 {
            // RETIRE_CONNECTION_ID with sequence number 0 is well formed.
            assert!(r.is_ok());
        } else {
            assert!(
                matches!(
                    r,
                    Err(FrameError::Truncated { .. } | FrameError::Malformed { .. })
                ),
                "{ty:#x}: {r:?}"
            );
        }
    }
    // PING as a two-byte varint.
    assert_eq!(
        parse_frame(&[0x40, 0x01], PacketType::OneRtt),
        Err(FrameError::NonMinimalType(1))
    );
    assert_eq!(
        scan_payload(&[], PacketType::OneRtt, |_| {}),
        Err(FrameError::EmptyPayload)
    );
}

#[test]
fn offset_overflow_is_rejected() {
    // CRYPTO at offset 2^62-1 with one byte of data.
    let wire = [
        0x06, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01, 0xaa,
    ];
    assert_eq!(
        parse_frame(&wire, PacketType::Initial),
        Err(FrameError::OffsetOverflow { frame_type: 6 })
    );
    let f = Frame::Crypto {
        offset: (1 << 62) - 1,
        data: b"x",
    };
    assert_eq!(f.encode(&mut [0u8; 32]), Err(FrameError::Encode));
}

#[test]
fn payload_scan_summarises_ack_eliciting_and_in_flight() {
    let mut buf = [0u8; 64];
    let ranges = ack_ranges();
    let ack = Frame::Ack(AckFrame::from_ranges(&ranges, 0, None).unwrap());
    let n = ack.encode(&mut buf).unwrap();
    let info = scan_payload(&buf[..n], PacketType::OneRtt, |_| {}).unwrap();
    assert!(!info.ack_eliciting && !info.in_flight);
    let m = Frame::Padding { len: 4 }.encode(&mut buf[n..]).unwrap();
    let info = scan_payload(&buf[..n + m], PacketType::OneRtt, |_| {}).unwrap();
    assert!(!info.ack_eliciting && info.in_flight);
    assert_eq!(info.frames, 2, "a PADDING run is one frame");
    let k = Frame::Ping.encode(&mut buf[n + m..]).unwrap();
    let mut seen = 0;
    let info = scan_payload(&buf[..n + m + k], PacketType::OneRtt, |_| seen += 1).unwrap();
    assert!(info.ack_eliciting && info.in_flight);
    assert_eq!(seen, 3);
}

#[test]
fn every_truncation_point_is_an_error_not_a_panic() {
    let ranges = ack_ranges();
    for frame in sample_frames(&ranges) {
        let mut buf = [0u8; 128];
        let n = frame.encode(&mut buf).unwrap();
        for cut in 1..n {
            let result = parse_frame(&buf[..cut], PacketType::OneRtt);
            match (frame, result) {
                // PADDING runs are any length.
                (Frame::Padding { .. }, Ok((Frame::Padding { len }, _))) => assert_eq!(len, cut),
                (_, Ok(parsed)) => panic!("{frame:?} truncated at {cut} parsed as {parsed:?}"),
                (_, Err(FrameError::Truncated { .. })) => {}
                (_, Err(e)) => panic!("{frame:?} at {cut}: unexpected {e:?}"),
            }
        }
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x77a2_0201);
    let ranges = ack_ranges();
    let mut seeds = Vec::new();
    for frame in sample_frames(&ranges) {
        let mut buf = [0u8; 128];
        let n = frame.encode(&mut buf).unwrap();
        seeds.push(buf[..n].to_vec());
    }
    for i in 0..300_000u32 {
        let buf = if i.is_multiple_of(2) {
            let len = rng.below(48) as usize;
            let mut b = rng.bytes(len);
            if let Some(first) = b.first_mut() {
                *first &= 0x1f; // mostly known frame types
            }
            b
        } else {
            let mut p = seeds[rng.below(seeds.len() as u64) as usize].clone();
            let at = rng.below(p.len() as u64) as usize;
            p[at] = rng.next() as u8;
            p
        };
        let pt = ALL_TYPES[rng.below(4) as usize];
        let mut consumed = 0;
        for frame in frames(&buf, pt) {
            let Ok(frame) = frame else { break };
            consumed += 1;
            if let Frame::Ack(a) = frame {
                // Validated ranges iterate without arithmetic surprises.
                let count = a.ranges().count() as u64;
                assert_eq!(count, a.range_count());
                let _ = a.smallest();
            }
            let mut out = vec![0u8; buf.len() + 16];
            assert!(frame.encode(&mut out).is_ok(), "{frame:?}");
        }
        let _ = consumed;
        let _ = parse_all(&buf, pt);
    }
}
