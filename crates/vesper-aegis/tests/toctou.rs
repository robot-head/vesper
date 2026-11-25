//! The §33.3 inference gate over a simulated shared VirtIO ring: results
//! depend only on the single snapshot, even while a concurrent
//! "hypervisor" thread rewrites shared memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vesper_aegis::gate::*;
use vesper_aegis::host::*;
use vesper_vault::{
    AeadOpener, AeadSealer, ChaCha20Poly1305, CiphertextFrame, FRAME_HEADER_LEN, LockPolicy,
    SecretSlot,
};

const RX: u8 = 0x11;
const TX: u8 = 0x22;
const AAD: [u8; 48] = [0x33; 48];
const SLOT: u32 = 256;

type Gate = VaultEnclaveGate<ChaCha20Poly1305>;

fn slot(byte: u8) -> SecretSlot<32> {
    SecretSlot::from_fn(LockPolicy::BestEffort, |k| k.fill(byte)).unwrap()
}

/// Confidential "scoring": count, sum and xor of the payload.
fn score(plain: &[u8], reply: &mut [u8; REPLY_LEN]) {
    let sum: u64 = plain.iter().map(|&b| u64::from(b)).sum();
    let xor = plain.iter().fold(0u8, |a, &b| a ^ b);
    reply[..8].copy_from_slice(&(plain.len() as u64).to_le_bytes());
    reply[8..16].copy_from_slice(&sum.to_le_bytes());
    reply[16] = xor;
}

fn gate_with_limit(limit: u64) -> Gate {
    VaultEnclaveGate::new(
        AeadOpener::new(slot(RX), 0),
        AeadSealer::with_record_limit(slot(TX), 0, limit),
        AAD,
    )
}

fn gate() -> Gate {
    gate_with_limit(1 << 20)
}

fn retire(g: Gate) {
    let _ = g.retire();
}

/// Tenant side: request frames with consecutive sequence numbers.
struct Tenant(AeadSealer<ChaCha20Poly1305>);
impl Tenant {
    fn new() -> Self {
        Tenant(AeadSealer::new(slot(RX), 0))
    }
    fn seal(&mut self, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; FRAME_HEADER_LEN + payload.len()];
        frame[FRAME_HEADER_LEN..].copy_from_slice(payload);
        let t = self.0.mint_nonce().unwrap();
        self.0.seal_in_place(t, &AAD, &mut frame).unwrap();
        frame
    }
    fn retire(self) {
        let _ = self.0.retire();
    }
}

/// Opens `frame` under `key` with a fresh opener (no replay window).
fn open(key: u8, frame: &[u8]) -> Option<Vec<u8>> {
    let opener: AeadOpener<ChaCha20Poly1305> = AeadOpener::new(slot(key), 0);
    let mut buf = frame.to_vec();
    let plain = opener
        .open_in_place(&AAD, CiphertextFrame::new(&mut buf))
        .ok()
        .map(|p| p.bytes().to_vec());
    let _ = opener.retire();
    plain
}

/// The gate's output as a pure function of the snapshot bytes.
fn reference(snapshot: &[u8]) -> Result<GateReply, ()> {
    let plain = open(RX, snapshot).ok_or(())?;
    let mut reply = [0u8; REPLY_LEN];
    score(&plain, &mut reply);
    Ok(GateReply {
        plaintext_len: plain.len(),
        reply,
    })
}

#[test]
fn gate_round_trip_only_ciphertext_reaches_host() {
    let mut ap = BounceAperture::new(1, SLOT);
    let host = ap.host_side();
    let mut tenant = Tenant::new();
    let frame = tenant.seal(b"tenant secret payload");
    host.write(0, 0, &frame);
    let mut g = gate();
    let mut scratch = PrivateRegion::with_capacity(4096);
    let grant = ap.grant(0).unwrap();
    let out = g
        .process_attested_enclave_req(
            &mut ap,
            &mut scratch,
            grant,
            UntrustedHostVal::from_host(frame.len() as u32),
            &mut score,
        )
        .unwrap();
    assert_eq!(out, reference(&frame).unwrap());
    assert!(!ap.is_granted(0));
    assert_eq!(scratch.used(), 0, "scratch region reset");
    let mut slot = vec![0u8; SLOT as usize];
    host.read(0, 0, &mut slot);
    let sealed = &slot[..VAULT_REPLY_FRAME_LEN];
    assert_eq!(open(TX, sealed).unwrap(), out.reply.to_vec());
    assert!(slot[VAULT_REPLY_FRAME_LEN..].iter().all(|&b| b == 0));
    assert!(
        !slot.windows(6).any(|w| w == b"secret"),
        "no plaintext in shared memory"
    );
    retire(g);
    tenant.retire();
}

#[test]
fn every_error_path_scrubs_and_releases_the_grant() {
    let mut ap = BounceAperture::new(1, SLOT);
    let host = ap.host_side();
    let mut g = gate();
    let mut tenant = Tenant::new();
    let mut scratch = PrivateRegion::with_capacity(4096);
    let good = tenant.seal(b"payload");
    let mut forged = good.clone();
    forged[FRAME_HEADER_LEN + 2] ^= 1;
    type Case<'a> = (&'a [u8], u32, fn(&VaultGateError) -> bool);
    let cases: [Case<'_>; 4] = [
        (&good, SLOT + 1, |e| {
            matches!(e, VaultGateError::HostIagoAttempt(_))
        }),
        (&good, 10, |e| {
            matches!(e, VaultGateError::FrameTooShort { len: 10 })
        }),
        (&forged, forged.len() as u32, |e| {
            matches!(e, VaultGateError::UnauthenticatedPayload(_))
        }),
        (&good, good.len() as u32 - 1, |e| {
            matches!(e, VaultGateError::UnauthenticatedPayload(_))
        }),
    ];
    for (frame, len, want) in cases {
        host.write(0, 0, frame);
        let grant = ap.grant(0).unwrap();
        let e = g
            .process_attested_enclave_req(
                &mut ap,
                &mut scratch,
                grant,
                UntrustedHostVal::from_host(len),
                &mut score,
            )
            .unwrap_err();
        assert!(want(&e), "{e:?}");
        assert!(!ap.is_granted(0));
        let mut slot = vec![1u8; SLOT as usize];
        host.read(0, 0, &mut slot);
        assert!(slot.iter().all(|&b| b == 0), "slot scrubbed after {e:?}");
    }
    retire(g);
    // Transmit nonces exhausted.
    let mut g = gate_with_limit(0);
    host.write(0, 0, &good);
    let grant = ap.grant(0).unwrap();
    assert_eq!(
        g.process_attested_enclave_req(
            &mut ap,
            &mut scratch,
            grant,
            UntrustedHostVal::from_host(good.len() as u32),
            &mut score
        ),
        Err(VaultGateError::RekeyRequired)
    );
    let mut slot = vec![1u8; SLOT as usize];
    host.read(0, 0, &mut slot);
    assert!(slot.iter().all(|&b| b == 0), "slot scrubbed after rekey");
    assert_eq!(ap.leaked_grants(), 0);
    retire(g);
    tenant.retire();
}

/// Deterministic TOCTOU: the host rewrites the frame immediately after the
/// snapshot (inside the observer, before authentication). The output is
/// the untouched frame's output. A double-fetch implementation that
/// re-reads shared memory after validation would see the rewrite.
#[test]
fn mutation_after_snapshot_cannot_change_output() {
    let mut ap = BounceAperture::new(1, SLOT);
    let host = ap.host_side();
    let mut tenant = Tenant::new();
    let frame = tenant.seal(b"value=1");
    let evil = tenant.seal(b"value=9999");
    host.write(0, 0, &frame);
    let mut g = gate();
    let mut scratch = PrivateRegion::with_capacity(4096);
    let grant = ap.grant(0).unwrap();
    let h = host.clone();
    let out = g
        .process_observed(
            &mut ap,
            &mut scratch,
            grant,
            UntrustedHostVal::from_host(frame.len() as u32),
            &mut score,
            &mut |_snap| h.write(0, 0, &evil),
        )
        .unwrap();
    assert_eq!(out, reference(&frame).unwrap());
    assert_ne!(out, reference(&evil).unwrap());

    // Contrast: a naive double fetch (validate the header from shared
    // memory, then read the body from shared memory again).
    host.write(0, 0, &frame);
    let mut first = vec![0u8; frame.len()];
    host.read(0, 0, &mut first);
    host.write(0, 0, &evil); // hypervisor races in between the two fetches
    let mut second = vec![0u8; evil.len()];
    host.read(0, 0, &mut second);
    assert_ne!(reference(&first), reference(&second));
    retire(g);
    tenant.retire();
}

/// Concurrent hypervisor thread flipping shared memory while the gate
/// runs: each output equals the reference applied to the bytes the gate
/// actually snapshotted, never anything else. Each request goes to a fresh
/// gate, so the replay window never rejects a frame the race replays.
#[test]
fn concurrent_hypervisor_race() {
    let mut ap = BounceAperture::new(1, SLOT);
    let host = ap.host_side();
    let mut tenant = Tenant::new();
    let frames: Vec<Vec<u8>> = (0..4u8).map(|i| tenant.seal(&[i + 1; 40])).collect();
    tenant.retire();
    let len = frames[0].len() as u32;
    let stop = Arc::new(AtomicBool::new(false));
    let hv = {
        let host = host.clone();
        let stop = Arc::clone(&stop);
        let frames = frames.clone();
        std::thread::spawn(move || {
            let mut i = 0usize;
            let mut x = 0x9E37_79B9u32;
            while !stop.load(Ordering::Relaxed) {
                if i.is_multiple_of(3) {
                    host.write(0, 0, &frames[i / 3 % 4]);
                } else {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    host.write(0, (x % len) as usize, &[x as u8]);
                }
                i += 1;
            }
        })
    };
    let mut scratch = PrivateRegion::with_capacity(4096);
    let (mut ok, mut rejected) = (0, 0);
    for _ in 0..500 {
        let mut g = gate();
        let grant = ap.grant(0).unwrap();
        let mut seen = Vec::new();
        let r = g.process_observed(
            &mut ap,
            &mut scratch,
            grant,
            UntrustedHostVal::from_host(len),
            &mut score,
            &mut |snap| seen = snap.to_vec(),
        );
        match (&r, reference(&seen)) {
            (Ok(a), Ok(b)) => {
                assert_eq!(*a, b);
                ok += 1;
            }
            (Err(VaultGateError::UnauthenticatedPayload(_)), Err(())) => rejected += 1,
            (got, want) => panic!("gate {got:?} vs snapshot reference {want:?}"),
        }
        assert!(!ap.is_granted(0));
        retire(g);
    }
    stop.store(true, Ordering::Relaxed);
    hv.join().unwrap();
    assert_eq!(ok + rejected, 500);
    eprintln!("race: {ok} authenticated snapshots, {rejected} torn snapshots rejected");
    assert_eq!(ap.leaked_grants(), 0);
}
