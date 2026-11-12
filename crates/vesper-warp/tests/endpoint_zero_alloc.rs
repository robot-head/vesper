//! "0 Alloc" (wave-four contract): the warmed-up end-to-end path performs
//! no heap allocation. A counting global allocator measures this test's
//! thread while two endpoints exchange stream data, echoes and ACKs:
//! harvest, header split, CID lookup, header-protection removal, PN
//! decode, AEAD open, frame scan, recovery, BBRv3, turn-around, seal, EDT
//! scheduling, the launch queue, paced submit, lease parking and
//! retirement, completions and fill-ring replenishment, on both sides.
//! This is the only test in the binary.

mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use support::*;
use vesper_warp::endpoint::{Discard, Echo, EndpointConfig, StreamMode};

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.with(Cell::get) {
            ALLOCS.with(|a| a.set(a.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.with(Cell::get) {
            ALLOCS.with(|a| a.set(a.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn steady_state_round_trips_allocate_nothing() {
    let config = EndpointConfig {
        initial_rtt_ns: 20 * MS,
        // The wave-four raw path: the Responder sees STREAM frames.
        stream_mode: StreamMode::Raw,
        ..EndpointConfig::default()
    };
    let mut p = Pair::new(config, Discard::default(), Echo::default());
    p.record = false;
    p.client.tap_mut().log_enabled = false;
    p.server.tap_mut().log_enabled = false;
    let data = [0x5au8; 512];
    let mut offset = 0u64;
    let mut round = |p: &mut Pair<Discard, Echo>| {
        for _ in 0..4 {
            p.client
                .send_stream(p.c_conn, 0, offset, &data, false, p.now)
                .unwrap();
            offset += data.len() as u64;
        }
        for _ in 0..4 {
            p.step();
        }
    };
    for _ in 0..100 {
        round(&mut p);
    }
    let before = (p.server.stats(), p.client.stats());

    ALLOCS.with(|a| a.set(0));
    COUNTING.with(|c| c.set(true));
    for _ in 0..300 {
        round(&mut p);
    }
    COUNTING.with(|c| c.set(false));
    let allocs = ALLOCS.with(Cell::get);

    let (s, c) = (p.server.stats(), p.client.stats());
    let echoed = s.replies - before.0.replies;
    assert_eq!(echoed, 1200, "every packet in the window was echoed");
    assert_eq!(c.rx_processed - before.1.rx_processed, 1200);
    assert!(s.leases_retired_acked - before.0.leases_retired_acked >= 1100);
    assert!(c.launch_released > before.1.launch_released, "pacing ran");
    assert_eq!(
        s.rx_dropped + c.rx_dropped + s.packets_lost + c.packets_lost,
        0
    );
    assert_eq!(allocs, 0, "heap allocations in the steady state");

    // The counter does see allocations on this thread.
    COUNTING.with(|c| c.set(true));
    drop(std::hint::black_box(Vec::<u8>::with_capacity(8)));
    COUNTING.with(|c| c.set(false));
    assert_eq!(ALLOCS.with(Cell::get), 1);
}
