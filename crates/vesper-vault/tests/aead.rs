//! Nonce tickets, sealing, the Doom-principle opener and exhaustion.

use vesper_vault::aead::{AeadAlgorithm, SEQ_SPACE_LIMIT};
use vesper_vault::{
    AeadOpener, AeadSealer, Aes256Gcm, AuthError, ChaCha20Poly1305, CiphertextFrame, CryptoError,
    FRAME_HEADER_LEN, FrameHeader, LeakAuditor, LockPolicy, ObligationKind, SecretSlot,
};

fn key(byte: u8) -> SecretSlot<32> {
    SecretSlot::from_fn(LockPolicy::BestEffort, |k| k.fill(byte)).unwrap()
}

fn pair<A: AeadAlgorithm>(epoch: u32) -> (AeadSealer<A>, AeadOpener<A>) {
    (
        AeadSealer::new(key(7), epoch),
        AeadOpener::new(key(7), epoch),
    )
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; FRAME_HEADER_LEN];
    f.extend_from_slice(payload);
    f
}

fn round_trip<A: AeadAlgorithm>() {
    let (mut sealer, opener) = pair::<A>(3);
    let mut seen = Vec::new();
    for msg in [
        &b"hello vault"[..],
        b"",
        b"a longer record body that spans more than one block",
    ] {
        let t = sealer.mint_nonce().unwrap();
        seen.push((t.epoch(), t.seq()));
        let mut buf = frame(msg);
        let sealed = sealer.seal_in_place(t, b"aad", &mut buf).unwrap();
        assert_eq!(sealed.frame_len, buf.len());
        let hdr = FrameHeader::parse(&buf).unwrap();
        assert_eq!((hdr.key_epoch, hdr.nonce_seq), (sealed.epoch, sealed.seq));
        if !msg.is_empty() {
            assert_ne!(&buf[FRAME_HEADER_LEN..], msg, "payload encrypted");
        }
        let pt = opener
            .open_in_place(b"aad", CiphertextFrame::new(&mut buf))
            .unwrap();
        assert_eq!(pt.bytes(), msg);
        assert_eq!(pt.header().nonce_seq, sealed.seq);
    }
    // Strictly increasing sequence numbers under one epoch.
    assert_eq!(seen, vec![(3, 0), (3, 1), (3, 2)]);
    assert!(sealer.retire().verified_zero);
    assert!(opener.retire().verified_zero);
}

#[test]
fn aes_gcm_round_trip() {
    round_trip::<Aes256Gcm>();
}

#[test]
fn chacha_round_trip() {
    round_trip::<ChaCha20Poly1305>();
}

/// Ciphertext matches an independent call to the RustCrypto crate with the
/// documented nonce layout `epoch_be32 || seq_be64`.
#[test]
fn nonce_layout_matches_documented_construction() {
    use aes_gcm::aead::{AeadInPlace, KeyInit};
    let (mut sealer, opener) = pair::<Aes256Gcm>(0x0102_0304);
    let _ = sealer.mint_nonce().unwrap().burn();
    let t = sealer.mint_nonce().unwrap();
    let mut buf = frame(b"layout");
    sealer.seal_in_place(t, b"", &mut buf).unwrap();
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(&0x0102_0304u32.to_be_bytes());
    nonce[4..].copy_from_slice(&1u64.to_be_bytes());
    let mut expect = b"layout".to_vec();
    let tag = aes_gcm::Aes256Gcm::new(&[7u8; 32].into())
        .encrypt_in_place_detached(&nonce.into(), b"", &mut expect)
        .unwrap();
    assert_eq!(&buf[FRAME_HEADER_LEN..], &expect[..]);
    assert_eq!(&buf[12..28], tag.as_slice());
    assert_eq!(&buf[0..4], &[1, 2, 3, 4]);
    assert_eq!(&buf[4..12], &1u64.to_be_bytes());
    let _ = sealer.retire();
    let _ = opener.retire();
}

fn tamper_zeroizes<A: AeadAlgorithm>() {
    let (mut sealer, opener) = pair::<A>(1);
    let secret = b"attack at dawn, bring snacks";
    let t = sealer.mint_nonce().unwrap();
    let mut good = frame(secret);
    sealer.seal_in_place(t, b"hdr", &mut good).unwrap();
    // Flip each region in turn: header epoch/seq, tag, payload, and AAD.
    type Mutation = Box<dyn Fn(&mut Vec<u8>)>;
    let cases: Vec<(&str, Mutation, &[u8], AuthError)> = vec![
        (
            "seq",
            Box::new(|b: &mut Vec<u8>| b[11] ^= 1),
            b"hdr",
            AuthError::AuthenticationTagMismatch,
        ),
        (
            "tag",
            Box::new(|b: &mut Vec<u8>| b[20] ^= 0x80),
            b"hdr",
            AuthError::AuthenticationTagMismatch,
        ),
        (
            "payload",
            Box::new(|b: &mut Vec<u8>| b[30] ^= 4),
            b"hdr",
            AuthError::AuthenticationTagMismatch,
        ),
        (
            "aad",
            Box::new(|_: &mut Vec<u8>| {}),
            b"HDR",
            AuthError::AuthenticationTagMismatch,
        ),
        (
            "epoch",
            Box::new(|b: &mut Vec<u8>| b[3] ^= 1),
            b"hdr",
            AuthError::WrongEpoch {
                expected: 1,
                found: 0,
            },
        ),
        (
            "truncated",
            Box::new(|b: &mut Vec<u8>| b.truncate(20)),
            b"hdr",
            AuthError::MalformedHeader,
        ),
    ];
    for (what, mutate, aad, expected) in cases {
        let mut buf = good.clone();
        mutate(&mut buf);
        let err = opener
            .open_in_place(aad, CiphertextFrame::new(&mut buf))
            .unwrap_err();
        assert_eq!(err, expected, "{what}");
        assert!(buf.iter().all(|&b| b == 0), "{what}: buffer not zeroized");
    }
    // The untouched frame still opens.
    let pt = opener
        .open_in_place(b"hdr", CiphertextFrame::new(&mut good))
        .unwrap();
    assert_eq!(pt.bytes(), secret);
    let _ = sealer.retire();
    let _ = opener.retire();
}

#[test]
fn tamper_zeroizes_aes_gcm() {
    tamper_zeroizes::<Aes256Gcm>();
}

#[test]
fn tamper_zeroizes_chacha() {
    tamper_zeroizes::<ChaCha20Poly1305>();
}

#[test]
fn wrong_key_fails_and_zeroizes() {
    let mut sealer = AeadSealer::<ChaCha20Poly1305>::new(key(1), 0);
    let opener = AeadOpener::<ChaCha20Poly1305>::new(key(2), 0);
    let t = sealer.mint_nonce().unwrap();
    let mut buf = frame(b"secret");
    sealer.seal_in_place(t, b"", &mut buf).unwrap();
    assert_eq!(
        opener
            .open_in_place(b"", CiphertextFrame::new(&mut buf))
            .unwrap_err(),
        AuthError::AuthenticationTagMismatch
    );
    assert!(buf.iter().all(|&b| b == 0));
    let _ = sealer.retire();
    let _ = opener.retire();
}

#[test]
fn failed_seal_returns_the_ticket() {
    let mut a = AeadSealer::<Aes256Gcm>::new(key(1), 0);
    let b = AeadSealer::<Aes256Gcm>::new(key(1), 0);
    let t = a.mint_nonce().unwrap();
    let mut short = [0u8; FRAME_HEADER_LEN - 1];
    let err = a.seal_in_place(t, b"", &mut short).unwrap_err();
    assert_eq!(err.error, CryptoError::BufferTooSmall);
    let t = err.ticket.expect("ticket handed back");
    // A ticket only seals under the sealer that minted it.
    let mut buf = frame(b"x");
    let err = b.seal_in_place(t, b"", &mut buf).unwrap_err();
    assert_eq!(err.error, CryptoError::ForeignTicket);
    let t = err.ticket.unwrap();
    assert_eq!(t.seq(), 0);
    a.seal_in_place(t, b"", &mut buf).unwrap();
    let _ = a.retire();
    let _ = b.retire();
}

#[test]
fn exhaustion_at_the_record_limit_requires_rekey() {
    let mut s = AeadSealer::<Aes256Gcm>::with_record_limit(key(1), 0, 3);
    assert_eq!(s.remaining(), 3);
    for seq in 0..3 {
        assert_eq!(s.mint_nonce().unwrap().burn().seq, seq);
    }
    assert_eq!(
        s.mint_nonce().unwrap_err(),
        CryptoError::EpochExhaustedRekeyRequired
    );
    assert_eq!(s.remaining(), 0);
    let (mut s, receipt) = s.rotate(key(2)).unwrap();
    assert!(receipt.verified_zero);
    assert_eq!((s.epoch(), s.next_seq()), (1, 0));
    assert_eq!(s.mint_nonce().unwrap().burn().epoch, 1);
    let _ = s.retire();

    // The algorithm limits: AES-GCM at 2^24.5 records, ChaCha at the §23.3
    // sequence bound, and a resumed sealer one short of it.
    assert_eq!(Aes256Gcm::RECORD_LIMIT, 23_726_566);
    let mut c =
        AeadSealer::<ChaCha20Poly1305>::with_start(key(3), 9, SEQ_SPACE_LIMIT - 1, u64::MAX);
    assert_eq!(c.mint_nonce().unwrap().burn().seq, 0xFFFF_FFFF_FFFF_FFFD);
    assert_eq!(
        c.mint_nonce().unwrap_err(),
        CryptoError::EpochExhaustedRekeyRequired
    );
    let _ = c.retire();
    let mut g = AeadSealer::<Aes256Gcm>::with_start(key(3), 0, u64::MAX - 5, u64::MAX);
    assert_eq!(
        g.mint_nonce().unwrap_err(),
        CryptoError::EpochExhaustedRekeyRequired
    );

    // Epoch space exhaustion hands both owners back.
    let s = AeadSealer::<Aes256Gcm>::new(key(1), u32::MAX);
    let err = s.rotate(key(2)).unwrap_err();
    assert_eq!(err.error, CryptoError::EpochSpaceExhausted);
    let _ = err.sealer.retire();
    let _ = err.new_key.shred();
    let _ = g.retire();
}

#[test]
fn leaked_obligations_are_counted() {
    let before = LeakAuditor::this_thread();
    let mut s = AeadSealer::<Aes256Gcm>::new(key(1), 0);
    drop(s.mint_nonce().unwrap());
    let mut buf = frame(b"zz");
    buf[FRAME_HEADER_LEN] = 0x55;
    drop(CiphertextFrame::new(&mut buf));
    assert!(
        buf.iter().all(|&b| b == 0),
        "a dropped frame is still wiped"
    );
    drop(s);
    let after = LeakAuditor::this_thread();
    for kind in [
        ObligationKind::NonceTicket,
        ObligationKind::CiphertextFrame,
        ObligationKind::AeadKey,
    ] {
        assert_eq!(after.count(kind), before.count(kind) + 1, "{kind:?}");
    }
    // The sealer's key slot was shredded by the sealer, not leaked twice.
    assert_eq!(
        after.count(ObligationKind::SecretSlot),
        before.count(ObligationKind::SecretSlot)
    );

    // Proper exits record nothing.
    let before = LeakAuditor::this_thread();
    let mut s = AeadSealer::<Aes256Gcm>::new(key(1), 0);
    let _ = s.mint_nonce().unwrap().burn();
    let mut buf = frame(b"zz");
    CiphertextFrame::new(&mut buf).discard();
    let _ = s.retire();
    assert_eq!(LeakAuditor::this_thread(), before);
}

/// Many tickets, all distinct, never regressing, across a rotation.
#[test]
fn nonces_never_repeat_or_regress() {
    let mut s = AeadSealer::<ChaCha20Poly1305>::with_record_limit(key(1), 0, 1000);
    let mut seen = std::collections::BTreeSet::new();
    let mut last = None;
    for round in 0..3 {
        loop {
            match s.mint_nonce() {
                Ok(t) => {
                    let id = (t.epoch(), t.seq());
                    assert!(seen.insert(id), "nonce reused: {id:?}");
                    if let Some(prev) = last {
                        assert!(id > prev, "regressed {prev:?} -> {id:?}");
                    }
                    last = Some(id);
                    let _ = t.burn();
                }
                Err(e) => {
                    assert_eq!(e, CryptoError::EpochExhaustedRekeyRequired);
                    break;
                }
            }
        }
        let (n, _) = s.rotate(key(round + 2)).unwrap();
        s = n;
    }
    assert_eq!(seen.len(), 3000);
    let _ = s.retire();
}
