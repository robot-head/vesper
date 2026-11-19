//! The frame, priority and capsule codecs through the public API, so both
//! builds (and the divergent port builds, `port/check.sh`) run them: frames
//! and SETTINGS (RFC 9114 §7), priorities and PRIORITY_UPDATE (RFC 9218),
//! and capsules (RFC 9297 §3).

use vesper_prism_h3::capsule::{
    CLOSE_WEBTRANSPORT_SESSION, Capsule, CapsuleError, CapsuleReader, DATAGRAM,
    DRAIN_WEBTRANSPORT_SESSION, write_capsule, write_close_session,
};
use vesper_prism_h3::frame::{
    self, DATA, Frame, FrameError, FrameReader, GOAWAY, HEADERS, SETTINGS, Settings, put_varint,
    write_frame, write_varint_frame,
};
use vesper_prism_h3::priority::{PRIORITY_UPDATE_REQUEST, Priority, PriorityUpdate, schedule};

fn read_all(r: &mut FrameReader, parts: &[&[u8]]) -> Vec<Frame> {
    let mut frames = Vec::new();
    for part in parts {
        r.push(part);
        while let Some(f) = r.next_frame().unwrap() {
            frames.push(f);
        }
    }
    frames
}

#[test]
fn frames_round_trip_split_anywhere() {
    let settings = Settings {
        qpack_max_table_capacity: 4096,
        qpack_blocked_streams: 16,
        max_field_section_size: Some(8192),
        enable_connect_protocol: true,
        h3_datagram: true,
        webtransport_max_sessions: Some(4),
        no_rfc7540_priorities: true,
    };
    let mut wire = Vec::new();
    settings.encode(&mut wire);
    write_frame(&mut wire, 0x21, b"grease");
    write_frame(&mut wire, HEADERS, b"\x00\x00\xd1");
    write_frame(&mut wire, DATA, b"hello");
    write_frame(&mut wire, DATA, b"");
    write_varint_frame(&mut wire, GOAWAY, 64);
    write_varint_frame(&mut wire, frame::MAX_PUSH_ID, 16383);
    write_varint_frame(&mut wire, frame::CANCEL_PUSH, 1 << 30);
    PriorityUpdate {
        push: false,
        element_id: 8,
        priority: Priority::new(1, true),
    }
    .encode(&mut wire);
    for split in 0..wire.len() {
        let mut r = FrameReader::new(1024);
        let frames = read_all(&mut r, &[&wire[..split], &wire[split..]]);
        assert!(r.finish().is_ok(), "split {split}");
        let mut data = Vec::new();
        let rest: Vec<&Frame> = frames
            .iter()
            .filter(|f| match f {
                Frame::Data(d) => {
                    data.extend_from_slice(d);
                    false
                }
                _ => true,
            })
            .collect();
        assert_eq!(data, b"hello", "split {split}");
        assert_eq!(
            rest,
            [
                &Frame::Settings(settings),
                &Frame::Unknown(0x21),
                &Frame::Headers(b"\x00\x00\xd1".to_vec()),
                &Frame::GoAway(64),
                &Frame::MaxPushId(16383),
                &Frame::CancelPush(1 << 30),
                &Frame::PriorityUpdate(PriorityUpdate {
                    push: false,
                    element_id: 8,
                    priority: Priority::new(1, true),
                }),
            ],
            "split {split}"
        );
    }
}

#[test]
fn settings_wire_format() {
    let mut wire = Vec::new();
    Settings::default().encode(&mut wire);
    // Capacity 0, blocked streams 0, and the reserved setting 250 = 0.
    assert_eq!(wire, [0x04, 0x07, 0x01, 0x00, 0x07, 0x00, 0x40, 0xfa, 0x00]);
    // Any of the three WebTransport identifiers is accepted.
    assert_eq!(Settings::parse(&[0x01]), Err(FrameError::Malformed));
    let mut p = Vec::new();
    put_varint(&mut p, 0x14e9_cd29);
    put_varint(&mut p, 3);
    assert_eq!(
        Settings::parse(&p).unwrap().webtransport_max_sessions,
        Some(3)
    );
    assert_eq!(
        Settings::parse(&[0x08, 0x02]),
        Err(FrameError::BadSetting(8))
    );
    assert_eq!(
        Settings::parse(&[0x06, 0x01, 0x06, 0x01]),
        Err(FrameError::BadSetting(6))
    );
}

#[test]
fn malformed_frames_are_typed_errors() {
    let mut r = FrameReader::new(16);
    r.push(&[0x02, 0x00]);
    assert_eq!(r.next_frame(), Err(FrameError::Http2Frame(2)));
    let mut r = FrameReader::new(16);
    r.push(&[HEADERS as u8, 17]);
    assert_eq!(
        r.next_frame(),
        Err(FrameError::TooLarge {
            frame_type: HEADERS,
            len: 17
        })
    );
    let mut r = FrameReader::new(16);
    r.push(&[GOAWAY as u8, 2, 0, 0]);
    assert_eq!(r.next_frame(), Err(FrameError::Malformed));
    let mut r = FrameReader::new(16);
    r.push(&[SETTINGS as u8, 4, 1, 0, 1, 0]);
    assert_eq!(r.next_frame(), Err(FrameError::BadSetting(1)));
    let mut r = FrameReader::new(16);
    r.push(&[DATA as u8, 5, b'a']);
    assert!(matches!(r.next_frame(), Ok(Some(Frame::Data(_)))));
    assert_eq!(r.finish(), Err(FrameError::Malformed));
    let mut r = FrameReader::new(16);
    let mut wire = Vec::new();
    put_varint(&mut wire, 0x40);
    put_varint(&mut wire, 1 << 40);
    r.push(&wire);
    assert_eq!(r.next_frame(), Ok(Some(Frame::Unknown(0x40))));
    r.push(&[0u8; 4096]);
    assert_eq!(r.next_frame(), Ok(None));
    assert_eq!(r.buffered(), 0);
}

#[test]
fn webtransport_stream_signal() {
    let mut r = FrameReader::with_webtransport(64);
    r.push(&[0x40, 0x41, 0x04, b'w', b't']);
    assert_eq!(r.next_frame(), Ok(Some(Frame::WebTransportStream(4))));
    assert_eq!(r.take_rest(), b"wt");
    let mut r = FrameReader::new(64);
    r.push(&[0x40, 0x41, 0x00]);
    assert_eq!(r.next_frame(), Ok(Some(Frame::Unknown(0x41))));
}

#[test]
fn rfc9218_priorities() {
    assert_eq!(Priority::parse(b"u=5, i"), Priority::new(5, true));
    assert_eq!(Priority::parse(b"i=?0, u=1"), Priority::new(1, false));
    assert_eq!(Priority::parse(b"u=8, i"), Priority::new(3, true));
    assert_eq!(Priority::parse(b"u=-0"), Priority::new(0, false));
    assert_eq!(Priority::parse(b"u=1.5"), Priority::default());
    assert_eq!(
        Priority::parse(b"foo=(a b);x=1, u=2;q, bar=:AAEC:, tok=a/b, s=\"x\\\"\", i"),
        Priority::new(2, true)
    );
    assert_eq!(Priority::parse(b"u=1, u=6"), Priority::new(6, false));
    for bad in [&b"u=1,"[..], b"U=1", b"u=1 i", b"u==1", b"u=1, i=?2"] {
        assert_eq!(Priority::parse(bad), Priority::default(), "{bad:?}");
    }
    for u in 0..8 {
        for i in [false, true] {
            let p = Priority::new(u, i);
            assert_eq!(Priority::parse(p.serialize().as_bytes()), p);
        }
    }
    assert_eq!(Priority::new(1, true).serialize(), "u=1, i");
    assert_eq!(
        PriorityUpdate::parse(PRIORITY_UPDATE_REQUEST, &[4, b'u', b'=', 0xff]),
        Err(FrameError::Malformed)
    );
}

#[test]
fn schedule_orders_by_urgency_then_round_robin() {
    let s = [
        (0, Priority::new(3, false)),
        (4, Priority::new(1, true)),
        (8, Priority::new(1, true)),
        (12, Priority::new(1, false)),
        (16, Priority::new(7, false)),
        (20, Priority::new(3, false)),
        (24, Priority::new(1, true)),
    ];
    assert_eq!(
        schedule(&s, 0),
        vec![vec![12], vec![4, 8, 24], vec![0], vec![20], vec![16]]
    );
    assert_eq!(schedule(&s, 4)[1], vec![8, 24, 4]);
    assert!(schedule(&[], 3).is_empty());
}

#[test]
fn capsules_round_trip_split_anywhere() {
    let mut wire = Vec::new();
    write_capsule(&mut wire, DATAGRAM, b"\x00hello");
    write_capsule(&mut wire, 0x17, &[9; 40]);
    write_capsule(&mut wire, DRAIN_WEBTRANSPORT_SESSION, b"");
    write_close_session(
        &mut wire,
        0xdead_beef,
        "bye \u{e9}\u{4e16}\u{1f600}".as_bytes(),
    );
    for split in 0..wire.len() {
        let mut r = CapsuleReader::new(64);
        let mut got = Vec::new();
        for part in [&wire[..split], &wire[split..]] {
            r.push(part);
            while let Some(c) = r.next_capsule().unwrap() {
                got.push(c);
            }
        }
        assert!(r.finish().is_ok(), "split {split}");
        assert_eq!(
            got,
            [
                Capsule::Datagram(b"\x00hello".to_vec()),
                Capsule::Unknown(0x17),
                Capsule::DrainSession,
                Capsule::CloseSession {
                    code: 0xdead_beef,
                    reason: "bye \u{e9}\u{4e16}\u{1f600}".as_bytes().to_vec()
                },
            ],
            "split {split}"
        );
    }
}

#[test]
fn bad_capsules_are_typed_errors() {
    let reject = |value: &[u8]| {
        let mut wire = Vec::new();
        write_close_session(&mut wire, 1, value);
        let mut r = CapsuleReader::new(8);
        r.push(&wire);
        r.next_capsule()
    };
    // Overlong, surrogate, above U+10FFFF, truncated, stray continuation.
    for bad in [
        &[0xc0, 0x80][..],
        &[0xed, 0xa0, 0x80],
        &[0xf4, 0x90, 0x80, 0x80],
        &[0xe4, 0xb8],
        &[0x80],
    ] {
        assert_eq!(
            reject(bad),
            Err(CapsuleError::Malformed {
                capsule_type: CLOSE_WEBTRANSPORT_SESSION
            }),
            "{bad:?}"
        );
    }
    let mut r = CapsuleReader::new(8);
    let mut wire = Vec::new();
    write_capsule(&mut wire, DATAGRAM, &[0; 9]);
    r.push(&wire);
    assert_eq!(
        r.next_capsule(),
        Err(CapsuleError::TooLarge {
            capsule_type: DATAGRAM,
            len: 9
        })
    );
    let mut r = CapsuleReader::new(8);
    r.push(&[0x40, 0x17, 0x05, 1, 2]);
    assert_eq!(r.next_capsule(), Ok(Some(Capsule::Unknown(0x17))));
    assert_eq!(r.next_capsule(), Ok(None));
    assert_eq!(r.finish(), Err(CapsuleError::Truncated));
}
