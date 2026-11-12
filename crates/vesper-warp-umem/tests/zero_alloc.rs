//! "0 Alloc": the warmed-up harvest → turn-around → submit → complete →
//! retire → replenish loop performs no heap allocation, measured with a
//! counting global allocator. Covers the driver and the software NIC in
//! loopback mode; it says nothing about a kernel backend.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use vesper_warp_umem::{
    InFlightQuicPkt, RetireOutcome, SoftNic, SoftNicMode, UmemAuditor, UmemConfig, XdpNic,
};

struct Counting;
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards every call to `System` unchanged; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn steady_state_loop_is_allocation_free() {
    let cfg = UmemConfig {
        frame_count: 64,
        frame_size: 2048,
        headroom: 128,
        fill_size: 64,
        rx_size: 32,
        tx_size: 32,
        completion_size: 32,
    };
    let mut xsk = SoftNic::xsk(cfg, SoftNicMode::Loopback).unwrap();
    xsk.replenish_fill(48);
    let mut auditor = UmemAuditor::new();
    // Seed a few packets; loopback then echoes them forever.
    for _ in 0..4 {
        xsk.backend_mut().deliver(&[0x5a; 200]);
    }
    let mut leases: [Option<InFlightQuicPkt>; 8] = Default::default();
    let mut pn = 0u64;
    let mut iteration = |xsk: &mut vesper_warp_umem::SoftXsk, auditor: &mut UmemAuditor| -> u64 {
        let start = pn;
        xsk.backend_mut().step();
        while let Some(rx) = xsk.harvest_rx().expect("well-formed") {
            let len = rx.pkt_len();
            let mut tx = xsk.turn_around_tx_in_place(rx, len).expect("in place");
            xsk.tx_bytes_mut(&mut tx).unwrap()[0] ^= 1;
            let lease = xsk.submit_paced_tx(tx, pn, pn * 1000).expect("tx space");
            let slot = &mut leases[(pn % 8) as usize];
            if let Some(old) = slot.replace(lease) {
                let outcome = xsk.retire(old).unwrap();
                assert!(matches!(
                    outcome,
                    RetireOutcome::Freed | RetireOutcome::AwaitingCompletion
                ));
            }
            pn += 1;
        }
        xsk.harvest_completions(64);
        xsk.replenish_fill(64);
        auditor.audit(xsk).expect("conserved");
        pn - start
    };
    for _ in 0..200 {
        iteration(&mut xsk, &mut auditor);
    }
    // Positive control: the counter does see allocations.
    COUNTING.store(true, Ordering::SeqCst);
    let probe = std::hint::black_box(Box::new([0u8; 64]));
    COUNTING.store(false, Ordering::SeqCst);
    drop(probe);
    assert!(
        ALLOCS.swap(0, Ordering::SeqCst) >= 1,
        "counting allocator is live"
    );

    let mut packets = 0;
    COUNTING.store(true, Ordering::SeqCst);
    for _ in 0..5_000 {
        packets += iteration(&mut xsk, &mut auditor);
    }
    COUNTING.store(false, Ordering::SeqCst);
    let allocations = ALLOCS.load(Ordering::SeqCst);
    assert!(packets >= 5_000, "the loop moved packets: {packets}");
    assert_eq!(
        allocations, 0,
        "steady-state loop (including the auditor) allocated {allocations} times over {packets} packets"
    );
}
