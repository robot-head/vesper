//! §33.1 item 3: the inference gate with its keys in Vault secret slots.
//! Round trips through Vault `AeadSealer`/`AeadOpener` frames, nonce
//! uniqueness on both sides, replay rejection, rekey at the record limit,
//! and shredding on retire.

use vesper_aegis::gate::*;
use vesper_aegis::host::*;
use vesper_vault::{
    AeadAlgorithm, AeadOpener, AeadSealer, Aes256Gcm, AuthError, ChaCha20Poly1305, CiphertextFrame,
    FRAME_HEADER_LEN, FrameHeader, LeakAuditor, LockPolicy, ObligationKind, SecretSlot,
};

const AAD: [u8; 48] = [0x33; 48];
const SLOT: u32 = 512;

fn slot(byte: u8) -> SecretSlot<32> {
    SecretSlot::from_fn(LockPolicy::BestEffort, |k| k.fill(byte)).unwrap()
}

fn score(plain: &[u8], reply: &mut [u8; REPLY_LEN]) {
    let sum: u64 = plain.iter().map(|&b| u64::from(b)).sum();
    reply[..8].copy_from_slice(&(plain.len() as u64).to_le_bytes());
    reply[8..16].copy_from_slice(&sum.to_le_bytes());
}

/// Tenant and enclave halves of one channel.
struct Channel<A: AeadAlgorithm> {
    tenant_tx: AeadSealer<A>,
    tenant_rx: AeadOpener<A>,
    gate: VaultEnclaveGate<A>,
}

fn channel<A: AeadAlgorithm>(limit: u64) -> Channel<A> {
    Channel {
        tenant_tx: AeadSealer::new(slot(0x11), 0),
        tenant_rx: AeadOpener::new(slot(0x22), 0),
        gate: VaultEnclaveGate::new(
            AeadOpener::new(slot(0x11), 0),
            AeadSealer::with_record_limit(slot(0x22), 0, limit),
            AAD,
        ),
    }
}

impl<A: AeadAlgorithm> Channel<A> {
    fn request(&mut self, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; FRAME_HEADER_LEN + payload.len()];
        frame[FRAME_HEADER_LEN..].copy_from_slice(payload);
        let t = self.tenant_tx.mint_nonce().unwrap();
        self.tenant_tx.seal_in_place(t, &AAD, &mut frame).unwrap();
        frame
    }

    fn retire(self) {
        let _ = self.tenant_tx.retire();
        let _ = self.tenant_rx.retire();
        let (a, b) = self.gate.retire();
        let _ = (a, b);
    }
}

fn run<A: AeadAlgorithm>(
    ch: &mut Channel<A>,
    ap: &mut BounceAperture,
    frame: &[u8],
) -> (Result<GateReply, VaultGateError>, Vec<u8>) {
    let host = ap.host_side();
    host.write(0, 0, frame);
    let mut scratch = PrivateRegion::with_capacity(4096);
    let grant = ap.grant(0).unwrap();
    let out = ch.gate.process_attested_enclave_req(
        ap,
        &mut scratch,
        grant,
        UntrustedHostVal::from_host(frame.len() as u32),
        &mut score,
    );
    assert!(!ap.is_granted(0), "grant discharged on every path");
    assert_eq!(scratch.used(), 0);
    let mut slot_bytes = vec![0u8; SLOT as usize];
    host.read(0, 0, &mut slot_bytes);
    (out, slot_bytes)
}

fn round_trip<A: AeadAlgorithm>() {
    let before = LeakAuditor::this_thread();
    let mut ch = channel::<A>(1 << 20);
    let mut ap = BounceAperture::new(1, SLOT);
    let mut seen = Vec::new();
    for i in 0..8u8 {
        let frame = ch.request(&[i; 40]);
        let (out, mut shared) = run(&mut ch, &mut ap, &frame);
        let out = out.unwrap();
        assert_eq!(out.plaintext_len, 40);
        // Only a Vault frame reaches shared memory; the tenant opens it.
        let reply_frame = &mut shared[..FRAME_HEADER_LEN + REPLY_LEN];
        let hdr = FrameHeader::parse(reply_frame).unwrap();
        seen.push((hdr.key_epoch, hdr.nonce_seq));
        assert!(!reply_frame[FRAME_HEADER_LEN..].starts_with(&40u64.to_le_bytes()));
        let plain = ch
            .tenant_rx
            .open_in_place(&AAD, CiphertextFrame::new(reply_frame))
            .unwrap();
        assert_eq!(plain.bytes(), &out.reply[..]);
        assert!(
            shared[FRAME_HEADER_LEN + REPLY_LEN..]
                .iter()
                .all(|&b| b == 0)
        );
    }
    // Reply nonces: unique and strictly increasing within the epoch.
    assert!(seen.windows(2).all(|w| w[0].0 == w[1].0 && w[0].1 < w[1].1));
    assert_eq!(ch.gate.sealer().next_seq(), 8);
    ch.retire();
    let after = LeakAuditor::this_thread();
    assert_eq!(
        after.count(ObligationKind::AeadKey),
        before.count(ObligationKind::AeadKey),
        "retire shreds every key without a leak"
    );
    assert_eq!(
        after.count(ObligationKind::NonceTicket),
        before.count(ObligationKind::NonceTicket)
    );
}

#[test]
fn vault_gate_round_trip_chacha20poly1305() {
    round_trip::<ChaCha20Poly1305>();
}

#[test]
fn vault_gate_round_trip_aes256gcm() {
    round_trip::<Aes256Gcm>();
}

#[test]
fn replayed_and_reordered_requests_are_rejected() {
    let mut ch = channel::<ChaCha20Poly1305>(1 << 20);
    let mut ap = BounceAperture::new(1, SLOT);
    let first = ch.request(b"first");
    let second = ch.request(b"second");
    let third = ch.request(b"third");
    run(&mut ch, &mut ap, &second).0.unwrap();
    // Replay of the same frame, and an older frame delivered late.
    for (frame, seq) in [(&second, 1), (&first, 0)] {
        let (out, shared) = run(&mut ch, &mut ap, frame);
        assert_eq!(
            out,
            Err(VaultGateError::ReplayedNonce {
                epoch: 0,
                seq,
                last: 1
            })
        );
        assert!(shared.iter().all(|&b| b == 0), "slot scrubbed");
    }
    run(&mut ch, &mut ap, &third).0.unwrap();
    ch.retire();
}

#[test]
fn forged_frames_do_not_advance_the_replay_window() {
    let mut ch = channel::<ChaCha20Poly1305>(1 << 20);
    let mut ap = BounceAperture::new(1, SLOT);
    let good = ch.request(b"payload");
    // A forged frame claiming a far-future sequence fails authentication
    // and must not lock out the real next frame.
    let mut forged = good.clone();
    forged[4..12].copy_from_slice(&u64::MAX.to_be_bytes());
    let (out, shared) = run(&mut ch, &mut ap, &forged);
    assert_eq!(
        out,
        Err(VaultGateError::UnauthenticatedPayload(
            AuthError::AuthenticationTagMismatch
        ))
    );
    assert!(shared.iter().all(|&b| b == 0));
    // Tampered ciphertext; wrong epoch; too short.
    let mut bad = good.clone();
    bad[FRAME_HEADER_LEN] ^= 1;
    assert!(matches!(
        run(&mut ch, &mut ap, &bad).0,
        Err(VaultGateError::UnauthenticatedPayload(_))
    ));
    let mut epoch = good.clone();
    epoch[3] = 1;
    assert!(matches!(
        run(&mut ch, &mut ap, &epoch).0,
        Err(VaultGateError::UnauthenticatedPayload(
            AuthError::WrongEpoch { .. }
        ))
    ));
    assert!(matches!(
        run(&mut ch, &mut ap, &good[..10]).0,
        Err(VaultGateError::FrameTooShort { len: 10 })
    ));
    run(&mut ch, &mut ap, &good).0.unwrap();
    ch.retire();
}

#[test]
fn transmit_record_limit_forces_rekey() {
    let mut ch = channel::<ChaCha20Poly1305>(2);
    let mut ap = BounceAperture::new(1, SLOT);
    for _ in 0..2 {
        let f = ch.request(b"x");
        run(&mut ch, &mut ap, &f).0.unwrap();
    }
    let f = ch.request(b"x");
    let (out, shared) = run(&mut ch, &mut ap, &f);
    assert_eq!(out, Err(VaultGateError::RekeyRequired));
    assert!(shared.iter().all(|&b| b == 0));
    assert_eq!(ch.gate.sealer().remaining(), 0);
    ch.retire();
}

#[test]
fn host_iago_lengths_are_rejected() {
    let mut ch = channel::<ChaCha20Poly1305>(1 << 20);
    let mut ap = BounceAperture::new(1, SLOT);
    let f = ch.request(b"payload");
    let host = ap.host_side();
    host.write(0, 0, &f);
    let mut scratch = PrivateRegion::with_capacity(4096);
    let grant = ap.grant(0).unwrap();
    let out = ch.gate.process_attested_enclave_req(
        &mut ap,
        &mut scratch,
        grant,
        UntrustedHostVal::from_host(SLOT + 1),
        &mut score,
    );
    assert!(matches!(out, Err(VaultGateError::HostIagoAttempt(_))));
    assert!(!ap.is_granted(0));
    ch.retire();
}

#[test]
fn attest_channel_binding_is_unavailable_without_a_tee() {
    let mut region = PrivateRegion::with_capacity(4096);
    assert!(matches!(
        attest_channel_binding(&mut region, &[0x42; 48]),
        Err(vesper_aegis::TeeError::HardwareUnavailable { .. })
    ));
    assert_eq!(region.used(), 0);
}
