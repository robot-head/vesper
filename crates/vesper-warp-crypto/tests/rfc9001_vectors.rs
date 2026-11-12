//! RFC 9001 Appendix A, reproduced byte-for-byte.

mod common;

use common::vectors::*;
use common::{decode_pn, hex};
use vesper_warp_crypto::{
    Cipher, CryptoError, InitialSecrets, OneRttKeys, Opener, Sealer, Side, TrafficSecret,
    retry_integrity_tag, verify_retry_integrity,
};

const DCID: &str = "8394c8f03e515708";

#[test]
fn a1_initial_keys() {
    let dcid = hex(DCID);
    assert_eq!(
        InitialSecrets::initial_secret_v1(&dcid).unwrap().to_vec(),
        hex("7db5df06e7a69e432496adedb00851923595221596ae2ae9fb8115c1e9ed0a44")
    );
    let s = InitialSecrets::new_v1(&dcid).unwrap();
    assert_eq!(
        s.client.as_bytes(),
        hex("c00cf151ca5be075ed0ebfb5c80323c42d6b7db67881289af4008f1f6c357aea")
    );
    let c = s.client.key_material();
    assert_eq!(c.key(), hex("1f369613dd76d5467730efcbe3b1a22d"));
    assert_eq!(c.iv().to_vec(), hex("fa044b2f42a3fd3b46fb255c"));
    assert_eq!(c.hp(), hex("9f50449e04a0e810283a1e9933adedd2"));

    assert_eq!(
        s.server.as_bytes(),
        hex("3c199828fd139efd216c155ad844cc81fb82fa8d7446fa7d78be803acdda951b")
    );
    let sv = s.server.key_material();
    assert_eq!(sv.key(), hex("cf3a5331653c364c88f0f379b6067e37"));
    assert_eq!(sv.iv().to_vec(), hex("0ac1493ca1905853b0bba03e"));
    assert_eq!(sv.hp(), hex("c206b8d9b9f0f37644430b490eeaa314"));
}

#[test]
fn initial_rejects_oversized_cid() {
    assert_eq!(
        InitialSecrets::new_v1(&[0u8; 21]).err(),
        Some(CryptoError::ConnectionIdTooLong(21))
    );
}

fn client_initial_plain() -> (Vec<u8>, usize) {
    let header = hex("c300000001088394c8f03e5157080000449e00000002");
    let mut payload = hex(CLIENT_CRYPTO_FRAME);
    payload.resize(1162, 0); // PADDING frames
    let mut buf = header.clone();
    buf.extend_from_slice(&payload);
    buf.resize(buf.len() + 16, 0);
    (buf, payload.len())
}

#[test]
fn a2_client_initial() {
    let secrets = InitialSecrets::new_v1(&hex(DCID)).unwrap();
    let (seal_keys, _) = secrets.keys_for(Side::Client);
    let mut sealer = Sealer::new(seal_keys);
    // Packet numbers 0 and 1 were "sent" earlier; burn them.
    for pn in 0..2 {
        let t = sealer.ticket(pn).unwrap();
        sealer.abandon(t).unwrap();
    }
    let ticket = sealer.ticket(2).unwrap();
    let (mut buf, payload_len) = client_initial_plain();
    let len = sealer.seal(ticket, &mut buf, 18, payload_len).unwrap();
    assert_eq!(len, 1200);
    assert_eq!(buf, hex(CLIENT_PROTECTED));

    // The server opens it through the lane-1 boundary.
    let (_, open_keys) = secrets.keys_for(Side::Server);
    let mut opener = Opener::new(open_keys);
    let hdr = opener.remove_header_protection(&mut buf, 18).unwrap();
    assert_eq!((hdr.pn_len, hdr.truncated_pn, hdr.first_byte), (4, 2, 0xc3));
    let pn = decode_pn(None, hdr.truncated_pn, hdr.pn_len);
    let opened = opener.open(&mut buf, &hdr, pn).unwrap();
    assert_eq!(
        (opened.packet_number, opened.header_len, opened.payload_len),
        (2, 22, 1162)
    );
    let (plain, _) = client_initial_plain();
    assert_eq!(buf[..22 + 1162], plain[..22 + 1162]);
}

#[test]
fn a3_server_initial() {
    let secrets = InitialSecrets::new_v1(&hex(DCID)).unwrap();
    let (seal_keys, _) = secrets.keys_for(Side::Server);
    let mut sealer = Sealer::new(seal_keys);
    let header = hex("c1000000010008f067a5502a4262b50040750001");
    let payload = hex(SERVER_PAYLOAD);
    let mut buf = header.clone();
    buf.extend_from_slice(&payload);
    buf.resize(buf.len() + 16, 0);
    let ticket = sealer.ticket(1).unwrap();
    let len = sealer.seal(ticket, &mut buf, 18, payload.len()).unwrap();
    assert_eq!(buf[..len], hex(SERVER_PROTECTED)[..]);

    let (_, open_keys) = secrets.keys_for(Side::Client);
    let mut opener = Opener::new(open_keys);
    let opened = opener
        .open_packet(&mut buf, 18, |h| {
            Some(decode_pn(None, h.truncated_pn, h.pn_len))
        })
        .unwrap();
    assert_eq!(opened.packet_number, 1);
    assert_eq!(
        buf[opened.header_len..opened.header_len + opened.payload_len],
        payload[..]
    );
}

#[test]
fn a4_retry_integrity_tag() {
    let retry = hex(RETRY_PACKET);
    let (body, tag) = retry.split_at(retry.len() - 16);
    assert_eq!(retry_integrity_tag(&hex(DCID), body).unwrap()[..], tag[..]);
    assert_eq!(verify_retry_integrity(&hex(DCID), &retry), Ok(()));
    let mut bad = retry.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert_eq!(
        verify_retry_integrity(&hex(DCID), &bad),
        Err(CryptoError::AuthenticationFailed)
    );
    assert_eq!(
        verify_retry_integrity(&hex("0011"), &retry),
        Err(CryptoError::AuthenticationFailed)
    );
    assert!(verify_retry_integrity(&hex(DCID), &retry[..15]).is_err());
}

const CHACHA_SECRET: &str = "9ac312a7f877468ebe69422748ad00a15443f18203a07d6060f688f30f21632b";
const CHACHA_PN: u64 = 654_360_564;

#[test]
fn a5_chacha20_poly1305_short_header() {
    let secret = TrafficSecret::new(Cipher::ChaCha20Poly1305, &hex(CHACHA_SECRET)).unwrap();
    let m = secret.key_material();
    assert_eq!(
        m.key(),
        hex("c6d98ff3441c3fe1b2182094f69caa2ed4b716b65488960a7a984979fb23e1c8")
    );
    assert_eq!(m.iv().to_vec(), hex("e0459b3474bdd0e44a41c144"));
    assert_eq!(
        m.hp(),
        hex("25a282b9e82f06f21f488917a4fc8f1b73573685608597d0efcb076b0ab7a7a4")
    );
    assert_eq!(
        secret.next_phase().as_bytes(),
        hex("1223504755036d556342ee9361d253421a826c9ecdf3c7148684b36b714881f9")
    );
    let keys = m.keys();
    assert_eq!(
        keys.packet.nonce(CHACHA_PN).to_vec(),
        hex("e0459b3474bdd0e46d417eb0")
    );
    let sample: [u8; 16] = hex("5e5cd55c41f69080575d7999c25a5bfb").try_into().unwrap();
    assert_eq!(keys.header.mask(&sample).to_vec(), hex("aefefe7d03"));

    // Through 1-RTT keys (phase 0) as the server's send side.
    let peer = TrafficSecret::new(Cipher::ChaCha20Poly1305, &[7u8; 32]).unwrap();
    let mut keys = OneRttKeys::new(secret, peer, 1_000).unwrap();
    let mut buf = hex("4200bff4");
    buf.push(0x01);
    buf.resize(buf.len() + 16, 0);
    let ticket = keys.ticket(CHACHA_PN).unwrap();
    let len = keys.seal(ticket, &mut buf, 1, 1).unwrap();
    assert_eq!(len, 21);
    assert_eq!(buf, hex("4cfe4189655e5cd55c41f69080575d7999c25a5bfb"));

    // The client opens it with the same secret as its receive key.
    let local = TrafficSecret::new(Cipher::ChaCha20Poly1305, &[7u8; 32]).unwrap();
    let remote = TrafficSecret::new(Cipher::ChaCha20Poly1305, &hex(CHACHA_SECRET)).unwrap();
    let mut client = OneRttKeys::new(local, remote, 1_000).unwrap();
    let opened = client
        .open_packet(&mut buf, 1, 0, |h| {
            Some(decode_pn(Some(CHACHA_PN - 1), h.truncated_pn, h.pn_len))
        })
        .unwrap();
    assert_eq!(opened.packet_number, CHACHA_PN);
    assert_eq!(&buf[opened.header_len..][..opened.payload_len], &[0x01]);
    assert_eq!(&buf[..4], &hex("4200bff4")[..]);
}

#[test]
fn chacha_header_protection_handles_max_counter() {
    let keys = TrafficSecret::new(Cipher::ChaCha20Poly1305, &hex(CHACHA_SECRET))
        .unwrap()
        .keys();
    let mut sample = [0u8; 16];
    sample[..4].copy_from_slice(&[0xff; 4]);
    // Regression: an attacker-chosen sample with counter 2^32 - 1 used to
    // panic inside the buffered ChaCha20 wrapper. The mask must match an
    // independent RFC 8439 §2.3 block function (validated on the A.5 sample
    // first), since the wrapper itself cannot produce that block.
    let hp = TrafficSecret::new(Cipher::ChaCha20Poly1305, &hex(CHACHA_SECRET))
        .unwrap()
        .key_material();
    let hp: [u8; 32] = hp.hp().try_into().unwrap();
    let a5: [u8; 16] = hex("5e5cd55c41f69080575d7999c25a5bfb").try_into().unwrap();
    assert_eq!(reference_mask(&hp, &a5).to_vec(), hex("aefefe7d03"));
    assert_eq!(keys.header.mask(&sample), reference_mask(&hp, &sample));

    // And a packet carrying that sample is rejected cleanly, not by panic.
    let mut opener = vesper_warp_crypto::Opener::new(keys);
    let mut pkt = vec![0x40u8; 40];
    pkt[5..9].copy_from_slice(&[0xff; 4]);
    let before = pkt.clone();
    assert!(
        opener
            .open_packet(&mut pkt, 1, |h| Some(h.truncated_pn))
            .is_err()
    );
    assert_eq!(pkt, before);
}

/// Test-only RFC 8439 §2.3 ChaCha20 block function, first 5 bytes, with the
/// RFC 9001 §5.4.4 counter/nonce split of the sample.
fn reference_mask(key: &[u8; 32], sample: &[u8; 16]) -> [u8; 5] {
    let word = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap());
    let mut st = [0u32; 16];
    st[..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
    for i in 0..8 {
        st[4 + i] = word(&key[4 * i..4 * i + 4]);
    }
    for i in 0..4 {
        st[12 + i] = word(&sample[4 * i..4 * i + 4]);
    }
    let mut x = st;
    fn qr(x: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
        x[a] = x[a].wrapping_add(x[b]);
        x[d] = (x[d] ^ x[a]).rotate_left(16);
        x[c] = x[c].wrapping_add(x[d]);
        x[b] = (x[b] ^ x[c]).rotate_left(12);
        x[a] = x[a].wrapping_add(x[b]);
        x[d] = (x[d] ^ x[a]).rotate_left(8);
        x[c] = x[c].wrapping_add(x[d]);
        x[b] = (x[b] ^ x[c]).rotate_left(7);
    }
    for _ in 0..10 {
        qr(&mut x, 0, 4, 8, 12);
        qr(&mut x, 1, 5, 9, 13);
        qr(&mut x, 2, 6, 10, 14);
        qr(&mut x, 3, 7, 11, 15);
        qr(&mut x, 0, 5, 10, 15);
        qr(&mut x, 1, 6, 11, 12);
        qr(&mut x, 2, 7, 8, 13);
        qr(&mut x, 3, 4, 9, 14);
    }
    let w0 = x[0].wrapping_add(st[0]).to_le_bytes();
    let w1 = x[1].wrapping_add(st[1]).to_le_bytes();
    [w0[0], w0[1], w0[2], w0[3], w1[0]]
}
