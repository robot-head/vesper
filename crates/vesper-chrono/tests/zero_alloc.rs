//! "0 Alloc" evidence: after warm-up (the ring has wrapped and overwritten,
//! every snapshot slot is used), recording effects, digests, `inout` writes,
//! obligation hops and snapshots performs no heap allocation, measured with a
//! counting global allocator. This binary holds only this test so no other
//! thread allocates while counting.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use vesper_chrono::{FlightRecorder, HopKind, RecorderConfig};

struct Counting;
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn workload(rec: &mut FlightRecorder, time: &mut u64, rounds: u64) {
    let payload = [0x5au8; 200];
    let image = [7u8; 512];
    for i in 0..rounds {
        *time += 1;
        let len = (i % 200) as usize;
        rec.record_effect(
            i % 8,
            *time,
            (i % 100) as u16,
            (i % 7) as u16,
            &payload[..len],
        )
        .unwrap();
        rec.record_effect_digest(i % 8, *time, 3, 1, &payload[..len])
            .unwrap();
        rec.record_inout_write(i % 8, *time, i % 32, i as u32)
            .unwrap();
        let id = rec.mint_obligation(i % 8, *time, 1, None).unwrap();
        rec.record_obligation_hop(i % 8, *time, id, HopKind::SinkMove, 2)
            .unwrap();
        rec.record_obligation_hop(i % 8, *time, id, HopKind::UnpackDischarge, 3)
            .unwrap();
        if i % 64 == 0 {
            rec.capture_snapshot(*time, &image[..(i % 512) as usize])
                .unwrap();
        }
        let mut seen = 0u64;
        rec.for_each_snapshot(|s| seen += s.image.len() as u64);
        std::hint::black_box(seen);
    }
}

#[test]
fn steady_state_recording_allocates_nothing() {
    let mut rec = FlightRecorder::new(RecorderConfig {
        ring_bytes: 64 * 1024,
        max_records: 1024,
        max_payload: 256,
        snapshot_slots: 4,
        snapshot_image_bytes: 512,
    })
    .unwrap();
    let mut time = 0;
    workload(&mut rec, &mut time, 2_000);
    assert!(rec.evicted_records() > 0, "warm-up wrapped the ring");

    ALLOCS.store(0, Ordering::SeqCst);
    COUNTING.store(true, Ordering::SeqCst);
    workload(&mut rec, &mut time, 20_000);
    COUNTING.store(false, Ordering::SeqCst);
    let allocs = ALLOCS.load(Ordering::SeqCst);
    assert_eq!(allocs, 0, "heap allocations in steady state");
    assert!(rec.evicted_records() > 100_000);
    eprintln!(
        "120000 records + 313 snapshots after warm-up: {allocs} allocations, {} evicted",
        rec.evicted_records()
    );
}
