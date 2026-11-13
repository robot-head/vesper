//! The §33.3 inference gate over a simulated shared VirtIO ring: results
//! depend only on the single snapshot, even while a concurrent
//! "hypervisor" thread rewrites shared memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vesper_aegis::gate::*;
use vesper_aegis::host::*;

const RX: [u8; 32] = [0x11; 32];
const TX: [u8; 32] = [0x22; 32];
const AAD: [u8; 48] = [0x33; 48];
const SLOT: u32 = 256;

/// Confidential "scoring": count, sum and xor of the payload.
fn score(plain: &[u8], reply: &mut [u8; REPLY_LEN]) {
    let sum: u64 = plain.iter().map(|&b| u64::from(b)).sum();
    let xor = plain.iter().fold(0u8, |a, &b| a ^ b);
    reply[..8].copy_from_slice(&(plain.len() as u64).to_le_bytes());
    reply[8..16].copy_from_slice(&sum.to_le_bytes());
    reply[16] = xor;
}

fn gate() -> EnclaveGate {
    EnclaveGate::new(&RX, &TX, AAD, NonceSequence::new([9, 9, 9, 9], 1 << 20))
}

/// The gate's output as a pure function of the snapshot bytes.
fn reference(snapshot: &[u8]) -> Result<GateReply, ()> {
    let plain = tenant_open_frame(&RX, &AAD, snapshot).ok_or(())?;
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
    let frame = tenant_seal_frame(&RX, [1; 12], &AAD, b"tenant secret payload");
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
    let sealed = &slot[..FRAME_OVERHEAD + REPLY_LEN];
    assert_eq!(
        tenant_open_frame(&TX, &AAD, sealed).unwrap(),
        out.reply.to_vec()
    );
    assert!(slot[FRAME_OVERHEAD + REPLY_LEN..].iter().all(|&b| b == 0));
    assert!(
        !slot.windows(6).any(|w| w == b"secret"),
        "no plaintext in shared memory"
    );
}

#[test]
fn every_error_path_scrubs_and_releases_the_grant() {
    let mut ap = BounceAperture::new(1, SLOT);
    let host = ap.host_side();
    let mut g = gate();
    let mut scratch = PrivateRegion::with_capacity(4096);
    let good = tenant_seal_frame(&RX, [1; 12], &AAD, b"payload");
    let mut forged = good.clone();
    forged[14] ^= 1;
    type Case<'a> = (&'a [u8], u32, fn(&EnclaveError) -> bool);
    let cases: [Case<'_>; 4] = [
        (&good, SLOT + 1, |e| {
            matches!(e, EnclaveError::HostIagoAttempt(_))
        }),
        (&good, 10, |e| {
            matches!(e, EnclaveError::FrameTooShort { len: 10 })
        }),
        (&forged, forged.len() as u32, |e| {
            *e == EnclaveError::UnauthenticatedPayload
        }),
        (&good, good.len() as u32 - 1, |e| {
            *e == EnclaveError::UnauthenticatedPayload
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
    // Nonce exhaustion.
    let mut g = EnclaveGate::new(&RX, &TX, AAD, NonceSequence::new([0; 4], 0));
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
        Err(EnclaveError::RekeyRequired)
    );
    assert_eq!(ap.leaked_grants(), 0);
    assert_eq!(g.nonces().leaked_tickets(), 0);
}

/// Deterministic TOCTOU: the host rewrites the frame immediately after the
/// snapshot (inside the observer, before authentication). The output is
/// the untouched frame's output. A double-fetch implementation that
/// re-reads shared memory after validation would see the rewrite.
#[test]
fn mutation_after_snapshot_cannot_change_output() {
    let mut ap = BounceAperture::new(1, SLOT);
    let host = ap.host_side();
    let frame = tenant_seal_frame(&RX, [2; 12], &AAD, b"value=1");
    let evil = tenant_seal_frame(&RX, [3; 12], &AAD, b"value=9999");
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
}

/// Concurrent hypervisor thread flipping shared memory while the gate
/// runs: each output equals the reference applied to the bytes the gate
/// actually snapshotted, never anything else.
#[test]
fn concurrent_hypervisor_race() {
    let mut ap = BounceAperture::new(1, SLOT);
    let host = ap.host_side();
    let frames: Vec<Vec<u8>> = (0..4u8)
        .map(|i| tenant_seal_frame(&RX, [i; 12], &AAD, &[i + 1; 40]))
        .collect();
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
    let mut g = gate();
    let mut scratch = PrivateRegion::with_capacity(4096);
    let (mut ok, mut rejected) = (0, 0);
    for _ in 0..2000 {
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
            (Err(EnclaveError::UnauthenticatedPayload), Err(())) => rejected += 1,
            (got, want) => panic!("gate {got:?} vs snapshot reference {want:?}"),
        }
        assert!(!ap.is_granted(0));
    }
    stop.store(true, Ordering::Relaxed);
    hv.join().unwrap();
    assert_eq!(ok + rejected, 2000);
    eprintln!("race: {ok} authenticated snapshots, {rejected} torn snapshots rejected");
    assert_eq!(ap.leaked_grants(), 0);
}
