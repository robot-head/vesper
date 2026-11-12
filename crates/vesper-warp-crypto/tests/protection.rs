//! Tamper rejection, nonce-ticket discipline and key updates.

mod common;

use common::decode_pn;
use vesper_warp_crypto::{
    Cipher, CryptoError, InitialSecrets, OneRttKeys, Opener, Sealer, Side, TrafficSecret,
    leaked_nonce_tickets,
};

const CIPHERS: [Cipher; 3] = [
    Cipher::Aes128Gcm,
    Cipher::Aes256Gcm,
    Cipher::ChaCha20Poly1305,
];
const DCID: [u8; 8] = [0xab; 8];
const PN_OFFSET: usize = 1 + DCID.len();
const PAYLOAD: usize = 40;
const WINDOW: u64 = 1_000;

fn secret(cipher: Cipher, fill: u8) -> TrafficSecret {
    TrafficSecret::new(cipher, &vec![fill; cipher.secret_len()]).unwrap()
}

/// (client, server) 1-RTT keys sharing two application secrets.
fn pair(cipher: Cipher) -> (OneRttKeys, OneRttKeys) {
    let client = OneRttKeys::new(secret(cipher, 1), secret(cipher, 2), WINDOW).unwrap();
    let server = OneRttKeys::new(secret(cipher, 2), secret(cipher, 1), WINDOW).unwrap();
    (client, server)
}

/// Unprotected short-header packet with a 2-byte packet number.
fn short_packet(phase: bool, pn: u64, fill: u8) -> Vec<u8> {
    let mut b = vec![0x40 | (u8::from(phase) << 2) | 0x01];
    b.extend_from_slice(&DCID);
    b.extend_from_slice(&(pn as u16).to_be_bytes());
    b.extend(std::iter::repeat_n(fill, PAYLOAD));
    b.resize(b.len() + 16, 0);
    b
}

fn send(keys: &mut OneRttKeys, pn: u64) -> Vec<u8> {
    let mut b = short_packet(keys.send_key_phase(), pn, pn as u8);
    let t = keys.ticket(pn).unwrap();
    keys.seal(t, &mut b, PN_OFFSET, PAYLOAD).unwrap();
    b
}

fn recv(
    keys: &mut OneRttKeys,
    pkt: &mut [u8],
    largest: Option<u64>,
    now: u64,
) -> Result<vesper_warp_crypto::OneRttOpened, CryptoError> {
    keys.open_packet(pkt, PN_OFFSET, now, |h| {
        Some(decode_pn(largest, h.truncated_pn, h.pn_len))
    })
}

#[test]
fn round_trip_every_cipher() {
    for cipher in CIPHERS {
        let (mut c, mut s) = pair(cipher);
        for pn in 0..5 {
            let mut pkt = send(&mut c, pn);
            let o = recv(&mut s, &mut pkt, pn.checked_sub(1), 0).unwrap();
            assert_eq!(o.packet_number, pn);
            assert!(
                pkt[o.header_len..][..o.payload_len]
                    .iter()
                    .all(|&b| b == pn as u8)
            );
        }
        assert_eq!(c.audit().sealed, 5);
        assert_eq!(c.audit().outstanding, 0);
    }
}

#[test]
fn tampering_fails_without_changing_buffer_or_keys() {
    for cipher in CIPHERS {
        let (mut c, mut s) = pair(cipher);
        let good = send(&mut c, 7);
        let header_len = PN_OFFSET + 2;
        // first byte, a DCID byte, both pn bytes, payload, tag
        let positions = [
            0,
            3,
            PN_OFFSET,
            PN_OFFSET + 1,
            header_len + 5,
            good.len() - 1,
        ];
        for (i, &pos) in positions.iter().enumerate() {
            for flip in [0x01u8, 0x80] {
                let mut pkt = good.clone();
                pkt[pos] ^= flip;
                let before = pkt.clone();
                let err = recv(&mut s, &mut pkt, Some(6), 0).unwrap_err();
                assert!(
                    matches!(
                        err,
                        CryptoError::AuthenticationFailed
                            | CryptoError::InvalidHeaderForm
                            | CryptoError::KeyRetired
                    ),
                    "{cipher:?} pos {pos}: {err:?}"
                );
                assert_eq!(pkt, before, "{cipher:?} case {i}: buffer modified");
            }
        }
        // No key state moved: the untampered packet still opens in phase 0.
        assert_eq!(s.recv_generation(), 0);
        assert!(s.authentication_failures() > 0);
        let mut pkt = good.clone();
        let o = recv(&mut s, &mut pkt, Some(6), 0).unwrap();
        assert_eq!((o.packet_number, o.generation, o.promoted), (7, 0, false));
    }
}

#[test]
fn wrong_reconstructed_packet_number_fails() {
    let (mut c, mut s) = pair(Cipher::Aes128Gcm);
    let good = send(&mut c, 3);
    let mut pkt = good.clone();
    // A reconstruction that does not encode the truncated value.
    let e = s
        .open_packet(&mut pkt, PN_OFFSET, 0, |_| Some(4))
        .unwrap_err();
    assert_eq!(e, CryptoError::PacketNumberMismatch);
    // One that encodes it but in a different epoch: nonce differs.
    let e = s
        .open_packet(&mut pkt, PN_OFFSET, 0, |_| Some(3 + 65_536))
        .unwrap_err();
    assert_eq!(e, CryptoError::AuthenticationFailed);
    let e = s.open_packet(&mut pkt, PN_OFFSET, 0, |_| None).unwrap_err();
    assert_eq!(e, CryptoError::PacketNumberReconstruction);
    let e = s
        .open_packet(&mut pkt, PN_OFFSET, 0, |_| Some(u64::MAX))
        .unwrap_err();
    assert_eq!(e, CryptoError::PacketNumberOutOfRange(u64::MAX));
    assert_eq!(pkt, good);
}

#[test]
fn nonce_reuse_and_regression_rejected() {
    let (mut c, _) = pair(Cipher::ChaCha20Poly1305);
    let t = c.ticket(10).unwrap();
    assert_eq!(
        c.ticket(10).err(),
        Some(CryptoError::PacketNumberReused(10))
    );
    assert_eq!(
        c.ticket(4).err(),
        Some(CryptoError::PacketNumberRegression {
            requested: 4,
            next_allowed: 11
        })
    );
    assert_eq!(
        c.ticket(1 << 62).err(),
        Some(CryptoError::PacketNumberOutOfRange(1 << 62))
    );
    c.abandon(t).unwrap();
    // Abandoning burns the number.
    assert_eq!(
        c.ticket(10).err(),
        Some(CryptoError::PacketNumberReused(10))
    );
    let last = c.ticket((1 << 62) - 1).unwrap();
    c.abandon(last).unwrap();
    assert_eq!(
        c.next_ticket().err(),
        Some(CryptoError::PacketNumbersExhausted)
    );
    assert_eq!(c.audit().abandoned, 2);
}

#[test]
fn failed_seal_returns_ticket_and_leaves_buffer() {
    let (mut c, _) = pair(Cipher::Aes128Gcm);
    let t = c.ticket(5).unwrap();
    // Truncated pn in the header encodes 6, not 5.
    let mut b = short_packet(false, 6, 0);
    let before = b.clone();
    let err = c.seal(t, &mut b, PN_OFFSET, PAYLOAD).unwrap_err();
    assert_eq!(err.error, CryptoError::PacketNumberMismatch);
    assert_eq!(b, before);
    let t = err.ticket;
    // Wrong key-phase bit.
    let mut b = short_packet(true, 5, 0);
    let err = c.seal(t, &mut b, PN_OFFSET, PAYLOAD).unwrap_err();
    assert_eq!(err.error, CryptoError::KeyPhaseMismatch);
    let t = err.ticket;
    // Long header given to 1-RTT keys.
    let mut b = short_packet(false, 5, 0);
    b[0] |= 0x80;
    let err = c.seal(t, &mut b, PN_OFFSET, PAYLOAD).unwrap_err();
    assert_eq!(err.error, CryptoError::InvalidHeaderForm);
    let t = err.ticket;
    // No room for the tag.
    let mut b = short_packet(false, 5, 0);
    b.truncate(b.len() - 1);
    let err = c.seal(t, &mut b, PN_OFFSET, PAYLOAD).unwrap_err();
    assert!(matches!(err.error, CryptoError::BufferTooShort { .. }));
    let t = err.ticket;
    // Payload too short to sample: pn_len 2 + payload 1 + tag 16 < 4 + 16.
    let mut b = short_packet(false, 5, 0);
    let err = c.seal(t, &mut b, PN_OFFSET, 1).unwrap_err();
    assert!(matches!(err.error, CryptoError::SampleOutOfBounds { .. }));
    let t = err.ticket;
    let err = c.seal(t, &mut b, 0, PAYLOAD).unwrap_err();
    assert_eq!(err.error, CryptoError::InvalidPacketNumberOffset);
    // The ticket still works.
    let mut b = short_packet(false, 5, 0);
    assert!(c.seal(err.ticket, &mut b, PN_OFFSET, PAYLOAD).is_ok());
    assert_eq!(c.audit().sealed, 1);
}

#[test]
fn foreign_and_stale_tickets() {
    let (mut c, mut s) = pair(Cipher::Aes128Gcm);
    let t = s.ticket(0).unwrap();
    let mut b = short_packet(false, 0, 0);
    let err = c.seal(t, &mut b, PN_OFFSET, PAYLOAD).unwrap_err();
    assert_eq!(err.error, CryptoError::StaleTicket);
    let t = c.abandon(err.ticket).unwrap_err(); // not c's ticket
    s.abandon(t).unwrap();
}

#[test]
fn dropped_ticket_is_counted() {
    let (mut c, _) = pair(Cipher::Aes128Gcm);
    let before = leaked_nonce_tickets();
    drop(c.ticket(0).unwrap());
    assert!(leaked_nonce_tickets() > before);
    assert_eq!(c.audit().outstanding, 1);
    // The leaked packet number is never re-issued.
    assert_eq!(c.ticket(0).err(), Some(CryptoError::PacketNumberReused(0)));
}

#[test]
fn confidentiality_limit_forces_key_update() {
    let (c, mut s) = pair(Cipher::Aes128Gcm);
    let mut c = c.with_confidentiality_limit(3);
    for pn in 0..3 {
        let mut pkt = send(&mut c, pn);
        recv(&mut s, &mut pkt, pn.checked_sub(1), 0).unwrap();
    }
    assert_eq!(
        c.ticket(3).err(),
        Some(CryptoError::ConfidentialityLimitReached)
    );
    c.on_packet_acked(2);
    c.initiate_key_update().unwrap();
    let mut pkt = send(&mut c, 3);
    assert!(recv(&mut s, &mut pkt, Some(2), 0).unwrap().promoted);
    assert_eq!(Cipher::Aes128Gcm.confidentiality_limit(), 1 << 23);
}

#[test]
fn integrity_limit_stops_opening() {
    let (mut c, s) = pair(Cipher::ChaCha20Poly1305);
    let mut s = s.with_integrity_limit(2);
    let good = send(&mut c, 0);
    for _ in 0..2 {
        let mut bad = good.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(
            recv(&mut s, &mut bad, None, 0).unwrap_err(),
            CryptoError::AuthenticationFailed
        );
    }
    let mut pkt = good.clone();
    assert_eq!(
        recv(&mut s, &mut pkt, None, 0).unwrap_err(),
        CryptoError::IntegrityLimitReached
    );
    assert_eq!(pkt, good);
}

#[test]
fn key_update_round_trip_and_retirement() {
    for cipher in CIPHERS {
        let (mut c, mut s) = pair(cipher);
        let p0 = send(&mut c, 0);
        let p1 = send(&mut c, 1);
        let mut x = p0.clone();
        recv(&mut s, &mut x, None, 0).unwrap();

        // Not permitted until a current-phase packet is acknowledged.
        assert_eq!(
            c.initiate_key_update(),
            Err(CryptoError::KeyUpdateNotPermitted)
        );
        c.on_packet_acked(0);
        let stale = c.ticket(2).unwrap();
        c.initiate_key_update().unwrap();
        assert!(c.send_key_phase());
        assert_eq!(c.send_generation(), 1);
        // A ticket issued under the old key is stale; it comes back.
        let mut b = short_packet(true, 2, 0);
        let err = c.seal(stale, &mut b, PN_OFFSET, PAYLOAD).unwrap_err();
        assert_eq!(err.error, CryptoError::StaleTicket);
        c.abandon(err.ticket).unwrap();
        // Cannot update again until the peer responds and acks.
        assert_eq!(
            c.initiate_key_update(),
            Err(CryptoError::KeyUpdateNotPermitted)
        );

        // New-phase packet promotes the server and makes it respond.
        let mut p3 = send(&mut c, 3);
        let o = recv(&mut s, &mut p3, Some(0), 100).unwrap();
        assert_eq!((o.packet_number, o.generation, o.promoted), (3, 1, true));
        assert_eq!((s.recv_generation(), s.send_generation()), (1, 1));
        assert!(s.send_key_phase());

        // A reordered old-phase packet still opens with the previous key.
        let mut x = p1.clone();
        let o = recv(&mut s, &mut x, Some(3), 200).unwrap();
        assert_eq!((o.packet_number, o.generation, o.promoted), (1, 0, false));

        // Server reply in phase 1 promotes the client's receive side.
        let mut r = send(&mut s, 0);
        let o = recv(&mut c, &mut r, None, 300).unwrap();
        assert_eq!((o.generation, o.promoted), (1, true));
        assert_eq!((c.recv_generation(), c.send_generation()), (1, 1));

        // After the window, the previous key is gone.
        let mut x = p1.clone();
        assert_eq!(
            recv(&mut s, &mut x, Some(3), 100 + WINDOW).unwrap_err(),
            CryptoError::KeyRetired
        );
        assert_eq!(x, p1);
        assert!(!s.has_previous_recv_key());

        // Second update, initiated by the server this time.
        s.on_packet_acked(0);
        s.initiate_key_update().unwrap();
        let mut r = send(&mut s, 1);
        let o = recv(&mut c, &mut r, Some(0), 400).unwrap();
        assert_eq!((o.generation, o.promoted), (2, true));
        assert!(!c.send_key_phase());
        let mut p = send(&mut c, 4);
        // The client's response moves the server's receive side to phase 2.
        let o = recv(&mut s, &mut p, Some(3), 500).unwrap();
        assert_eq!((o.generation, o.promoted), (2, true));
        assert_eq!((s.recv_generation(), s.send_generation()), (2, 2));
    }
}

#[test]
fn rejects_long_header_on_one_rtt_and_bad_offsets() {
    let (mut c, mut s) = pair(Cipher::Aes128Gcm);
    let mut pkt = send(&mut c, 0);
    let good = pkt.clone();
    pkt[0] |= 0x80; // HP mask for long headers covers only 4 bits
    let before = pkt.clone();
    assert!(recv(&mut s, &mut pkt, None, 0).is_err());
    assert_eq!(pkt, before);
    let mut pkt = good.clone();
    assert_eq!(
        s.open_packet(&mut pkt, 0, 0, |_| Some(0)).unwrap_err(),
        CryptoError::InvalidPacketNumberOffset
    );
    assert!(matches!(
        s.open_packet(&mut pkt, good.len() - 19, 0, |_| Some(0))
            .unwrap_err(),
        CryptoError::SampleOutOfBounds { .. }
    ));
    assert!(matches!(
        s.open_packet(&mut pkt, usize::MAX, 0, |_| Some(0))
            .unwrap_err(),
        CryptoError::SampleOutOfBounds { .. }
    ));
    assert_eq!(pkt, good);
}

#[test]
fn initial_open_tamper_and_garbage_never_panic() {
    let secrets = InitialSecrets::new_v1(&DCID).unwrap();
    let (seal, _) = secrets.keys_for(Side::Client);
    let (_, open) = secrets.keys_for(Side::Server);
    let mut sealer = Sealer::new(seal);
    let mut opener = Opener::new(open);
    // Long header: first byte, version, dcid len + dcid, scid len, token len, length, pn
    let mut hdr = vec![0xc1, 0, 0, 0, 1, 8];
    hdr.extend_from_slice(&DCID);
    hdr.extend_from_slice(&[0, 0, 0x40, 0x3a, 0x00, 0x00]);
    let pn_offset = hdr.len() - 2;
    let mut pkt = hdr.clone();
    pkt.extend(std::iter::repeat_n(0u8, 40));
    pkt.resize(pkt.len() + 16, 0);
    let t = sealer.next_ticket().unwrap();
    sealer.seal(t, &mut pkt, pn_offset, 40).unwrap();
    let good = pkt.clone();
    for pos in 0..good.len() {
        let mut bad = good.clone();
        bad[pos] ^= 0x10;
        let r = opener.open_packet(&mut bad, pn_offset, |h| {
            Some(decode_pn(None, h.truncated_pn, h.pn_len))
        });
        assert!(r.is_err(), "flip at {pos} accepted");
        let mut expect = good.clone();
        expect[pos] ^= 0x10;
        assert_eq!(bad, expect);
    }
    assert_eq!(opener.largest_opened(), None);
    // Deterministic garbage of every length up to 80 bytes at many offsets.
    let mut x: u32 = 0x1234_5678;
    for len in 0..80 {
        let mut buf: Vec<u8> = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        for off in 0..len + 2 {
            let _ = opener.open_packet(&mut buf, off, |h| Some(h.truncated_pn));
        }
    }
    let mut pkt = good.clone();
    assert!(
        opener
            .open_packet(&mut pkt, pn_offset, |h| Some(h.truncated_pn))
            .is_ok()
    );
}

#[test]
fn secret_length_checked() {
    assert_eq!(
        TrafficSecret::new(Cipher::Aes256Gcm, &[0; 32]).err(),
        Some(CryptoError::SecretLength {
            expected: 48,
            actual: 32
        })
    );
    let r = OneRttKeys::new(
        secret(Cipher::Aes128Gcm, 1),
        secret(Cipher::Aes256Gcm, 2),
        0,
    );
    assert_eq!(r.err().map(|_| ()), Some(()));
}
