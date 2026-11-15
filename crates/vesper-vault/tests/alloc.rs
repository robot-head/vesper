//! Zero heap allocations on the warmed-up seal/open hot path, measured with
//! a counting global allocator scoped to the executing thread. Libtest's
//! other threads must not perturb the synchronous hot-path measurement.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use vesper_vault::{
    AeadAlgorithm, AeadOpener, AeadSealer, Aes256Gcm, ChaCha20Poly1305, CiphertextFrame,
    FRAME_HEADER_LEN, LockPolicy, SecretSlot,
};

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn note_allocation() {
    // Allocator calls during TLS teardown must still reach System.
    let _ = ALLOCS.try_with(|n| n.set(n.get().wrapping_add(1)));
}

fn allocation_count() -> u64 {
    ALLOCS.with(Cell::get)
}

// SAFETY: forwards to the system allocator unchanged, only counting calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        note_allocation();
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
    let start = allocation_count();
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
    let allocs = allocation_count() - start;
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

#[test]
fn own_thread_allocations_and_reallocations_are_counted() {
    let before = allocation_count();
    // Keep the allocation observable and force growth on the second push.
    let mut bytes = std::hint::black_box(Vec::with_capacity(1));
    bytes.push(1u8);
    bytes.push(2);
    std::hint::black_box(&bytes);
    let measured = allocation_count() - before;
    drop(bytes);
    assert_eq!(
        measured, 2,
        "counter must detect allocation and reallocation"
    );
}

#[test]
fn unrelated_worker_allocations_do_not_change_measurement() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let phase = Arc::new(AtomicUsize::new(0));
    let other = Arc::clone(&phase);
    let worker = std::thread::spawn(move || {
        while other.load(Ordering::Acquire) == 0 {
            std::hint::spin_loop();
        }
        let bytes = std::hint::black_box(Box::new([7u8; 1024]));
        std::hint::black_box(&bytes);
        drop(bytes);
        other.store(2, Ordering::Release);
    });
    let before = allocation_count();
    phase.store(1, Ordering::Release);
    let start = std::time::Instant::now();
    while phase.load(Ordering::Acquire) != 2 {
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        std::hint::spin_loop();
    }
    let measured = allocation_count() - before;
    worker.join().unwrap();
    assert_eq!(
        measured, 0,
        "unrelated worker polluted the hot-path measurement"
    );
}
