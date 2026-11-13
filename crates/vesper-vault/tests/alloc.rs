//! Zero heap allocations on the warmed-up seal/open hot path, measured with
//! a counting global allocator. One test per binary so no other test
//! allocates concurrently.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use vesper_vault::{
    AeadAlgorithm, AeadOpener, AeadSealer, Aes256Gcm, ChaCha20Poly1305, CiphertextFrame,
    FRAME_HEADER_LEN, LockPolicy, SecretSlot,
};

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards to the system allocator unchanged, only counting calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn hot_path<A: AeadAlgorithm>() -> u64 {
    let key = || SecretSlot::<32>::from_fn(LockPolicy::BestEffort, |k| k.fill(9)).unwrap();
    let mut sealer = AeadSealer::<A>::new(key(), 0);
    let opener = AeadOpener::<A>::new(key(), 0);
    let mut buf = [0u8; FRAME_HEADER_LEN + 1200];
    // Warm up.
    for _ in 0..8 {
        let t = sealer.mint_nonce().unwrap();
        sealer.seal_in_place(t, b"aad", &mut buf).unwrap();
        let _ = opener
            .open_in_place(b"aad", CiphertextFrame::new(&mut buf))
            .unwrap();
    }
    let start = ALLOCS.load(Ordering::Relaxed);
    for i in 0..1000u32 {
        buf[FRAME_HEADER_LEN..FRAME_HEADER_LEN + 4].copy_from_slice(&i.to_le_bytes());
        let t = sealer.mint_nonce().unwrap();
        sealer.seal_in_place(t, b"aad", &mut buf).unwrap();
        let pt = opener
            .open_in_place(b"aad", CiphertextFrame::new(&mut buf))
            .unwrap();
        assert_eq!(&pt.bytes()[..4], &i.to_le_bytes());
    }
    // Tamper path too: authentication failure plus zeroization.
    for _ in 0..100 {
        let t = sealer.mint_nonce().unwrap();
        sealer.seal_in_place(t, b"aad", &mut buf).unwrap();
        buf[FRAME_HEADER_LEN] ^= 1;
        assert!(
            opener
                .open_in_place(b"aad", CiphertextFrame::new(&mut buf))
                .is_err()
        );
    }
    let allocs = ALLOCS.load(Ordering::Relaxed) - start;
    let _ = sealer.retire();
    let _ = opener.retire();
    allocs
}

#[test]
fn seal_and_open_do_not_allocate() {
    assert_eq!(hot_path::<Aes256Gcm>(), 0, "AES-256-GCM hot path allocated");
    assert_eq!(
        hot_path::<ChaCha20Poly1305>(),
        0,
        "ChaCha20-Poly1305 hot path allocated"
    );
}
