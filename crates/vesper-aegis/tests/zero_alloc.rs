//! "0 Alloc" (design §33.3): after warm-up, the inference gate performs
//! no heap allocation per request, measured with a counting global
//! allocator on this thread.

#![allow(deprecated)] // the legacy EnclaveGate shim is still tested

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use vesper_aegis::gate::*;
use vesper_aegis::host::*;

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    let _ = COUNTING.try_with(|c| {
        if c.get() {
            let _ = ALLOCS.try_with(|a| a.set(a.get() + 1));
        }
    });
}

// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn gate_steady_state_does_not_allocate() {
    let rx = [1u8; 32];
    let tx = [2u8; 32];
    let aad = [3u8; 48];
    let mut ap = BounceAperture::new(1, 512);
    let host = ap.host_side();
    let frame = tenant_seal_frame(&rx, [4; 12], &aad, &[7u8; 200]);
    let mut gate = EnclaveGate::new(&rx, &tx, aad, NonceSequence::new([0; 4], u64::MAX));
    let mut scratch = PrivateRegion::with_capacity(4096);
    let mut eval = |p: &[u8], r: &mut [u8; REPLY_LEN]| r[0] = p.len() as u8;
    let mut run = |ap: &mut BounceAperture, scratch: &mut PrivateRegion| {
        host.write(0, 0, &frame);
        let grant = ap.grant(0).unwrap();
        gate.process_attested_enclave_req(
            ap,
            scratch,
            grant,
            UntrustedHostVal::from_host(frame.len() as u32),
            &mut eval,
        )
        .unwrap();
    };
    for _ in 0..16 {
        run(&mut ap, &mut scratch);
    }
    COUNTING.with(|c| c.set(true));
    for _ in 0..1000 {
        run(&mut ap, &mut scratch);
    }
    COUNTING.with(|c| c.set(false));
    assert_eq!(ALLOCS.with(Cell::get), 0);
}

#[test]
fn vault_gate_steady_state_does_not_allocate() {
    use vesper_vault::{
        AeadOpener, AeadSealer, ChaCha20Poly1305, FRAME_HEADER_LEN, LockPolicy, SecretSlot,
    };
    let slot = |b: u8| SecretSlot::<32>::from_fn(LockPolicy::BestEffort, |k| k.fill(b)).unwrap();
    let aad = [3u8; 48];
    let mut tenant = AeadSealer::<ChaCha20Poly1305>::new(slot(1), 0);
    // Fresh frames, sealed before counting starts (the gate rejects replays).
    let frames: Vec<Vec<u8>> = (0..1016)
        .map(|_| {
            let mut f = vec![7u8; FRAME_HEADER_LEN + 200];
            let t = tenant.mint_nonce().unwrap();
            tenant.seal_in_place(t, &aad, &mut f).unwrap();
            f
        })
        .collect();
    let mut gate = VaultEnclaveGate::new(
        AeadOpener::<ChaCha20Poly1305>::new(slot(1), 0),
        AeadSealer::new(slot(2), 0),
        aad,
    );
    let mut ap = BounceAperture::new(1, 512);
    let host = ap.host_side();
    let mut scratch = PrivateRegion::with_capacity(4096);
    let mut eval = |p: &[u8], r: &mut [u8; REPLY_LEN]| r[0] = p.len() as u8;
    let mut run = |ap: &mut BounceAperture, scratch: &mut PrivateRegion, frame: &[u8]| {
        host.write(0, 0, frame);
        let grant = ap.grant(0).unwrap();
        gate.process_attested_enclave_req(
            ap,
            scratch,
            grant,
            UntrustedHostVal::from_host(frame.len() as u32),
            &mut eval,
        )
        .unwrap();
    };
    let (warm, steady) = frames.split_at(16);
    for f in warm {
        run(&mut ap, &mut scratch, f);
    }
    ALLOCS.with(|a| a.set(0));
    COUNTING.with(|c| c.set(true));
    for f in steady {
        run(&mut ap, &mut scratch, f);
    }
    COUNTING.with(|c| c.set(false));
    assert_eq!(ALLOCS.with(Cell::get), 0);
    let _ = (gate.retire(), tenant.retire());
}
