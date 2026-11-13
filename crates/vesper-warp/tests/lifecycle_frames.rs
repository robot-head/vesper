//! Wave six (issue #247): NEW_TOKEN, NEW_CONNECTION_ID,
//! RETIRE_CONNECTION_ID, PATH_CHALLENGE and PATH_RESPONSE parse, encode,
//! obey RFC 9000 Table 3 and reject malformed encodings.

use vesper_warp::PacketType;
use vesper_warp::frame::{Frame, FrameError, parse_frame};

fn round_trip(f: Frame<'_>, ty: PacketType) {
    let mut buf = [0u8; 128];
    let n = f.encode(&mut buf).unwrap();
    let (back, len) = parse_frame(&buf[..n], ty).unwrap();
    assert_eq!(len, n);
    assert_eq!(back, f);
}

#[test]
fn lifecycle_frames_round_trip() {
    let token = [7u8; 16];
    round_trip(Frame::NewToken { token: b"tok" }, PacketType::OneRtt);
    round_trip(
        Frame::NewConnectionId {
            seq: 5,
            retire_prior_to: 2,
            cid: &[1, 2, 3, 4, 5, 6, 7, 8],
            reset_token: &token,
        },
        PacketType::ZeroRtt,
    );
    round_trip(Frame::RetireConnectionId { seq: 9 }, PacketType::OneRtt);
    round_trip(Frame::PathChallenge(*b"abcdefgh"), PacketType::ZeroRtt);
    round_trip(Frame::PathResponse(*b"hgfedcba"), PacketType::OneRtt);
    // Probing frames (RFC 9000 §9.1).
    assert!(Frame::PathChallenge([0; 8]).is_probing());
    assert!(Frame::Padding { len: 3 }.is_probing());
    assert!(!Frame::Ping.is_probing());
    assert!(Frame::RetireConnectionId { seq: 0 }.is_ack_eliciting());
}

#[test]
fn table_3_permissions() {
    let cases: [(&[u8], [bool; 4]); 5] = [
        // [Initial, 0-RTT, Handshake, 1-RTT]
        (&[0x07, 0x01, 0xaa], [false, false, false, true]),
        (&[0x19, 0x00], [false, true, false, true]),
        (&[0x1a, 1, 2, 3, 4, 5, 6, 7, 8], [false, true, false, true]),
        (&[0x1b, 1, 2, 3, 4, 5, 6, 7, 8], [false, false, false, true]),
        (
            &[
                0x18, 1, 0, 1, 0xcc, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
            ],
            [false, true, false, true],
        ),
    ];
    let types = [
        PacketType::Initial,
        PacketType::ZeroRtt,
        PacketType::Handshake,
        PacketType::OneRtt,
    ];
    for (wire, allowed) in cases {
        for (ty, ok) in types.into_iter().zip(allowed) {
            let r = parse_frame(wire, ty);
            assert_eq!(r.is_ok(), ok, "{wire:x?} in {ty:?}: {r:?}");
            if !ok {
                assert!(matches!(r, Err(FrameError::NotAllowed { .. })));
            }
        }
    }
}

#[test]
fn malformed_lifecycle_frames_are_typed_errors() {
    let malformed = |wire: &[u8]| {
        assert!(
            matches!(
                parse_frame(wire, PacketType::OneRtt),
                Err(FrameError::Malformed { .. })
            ),
            "{wire:x?}"
        )
    };
    // Empty NEW_TOKEN.
    malformed(&[0x07, 0x00]);
    // NEW_CONNECTION_ID: zero length, 21 bytes, retire_prior_to > seq.
    malformed(&[0x18, 1, 0, 0]);
    malformed(&[0x18, 1, 0, 21]);
    malformed(&[0x18, 1, 2, 4, 1, 2, 3, 4]);
    // Truncated reset token and path data.
    assert!(matches!(
        parse_frame(&[0x18, 1, 0, 1, 0xcc, 0, 1, 2], PacketType::OneRtt),
        Err(FrameError::Truncated { .. })
    ));
    assert!(matches!(
        parse_frame(&[0x1a, 1, 2, 3], PacketType::OneRtt),
        Err(FrameError::Truncated { .. })
    ));
    // Encoding refuses what parsing would reject.
    let mut buf = [0u8; 64];
    assert!(Frame::NewToken { token: b"" }.encode(&mut buf).is_err());
    assert!(
        Frame::NewConnectionId {
            seq: 1,
            retire_prior_to: 2,
            cid: &[1],
            reset_token: &[0; 16]
        }
        .encode(&mut buf)
        .is_err()
    );
}
