//! "0 Alloc" for reliable streams (wave five, issue #133): once a stream is
//! open and both endpoints are warm, writing, packing, acknowledging,
//! reading and echoing stream data performs no heap allocation. A counting global allocator measures this test's
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
use vesper_warp::endpoint::{Discard, EndpointConfig};

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
fn steady_state_stream_echo_allocates_nothing() {
    let config = EndpointConfig {
        initial_rtt_ns: 20 * MS,
        ..EndpointConfig::default()
    };
    let mut p = Pair::new(config, Discard::default(), Discard::default());
    p.record = false;
    p.client.tap_mut().log_enabled = false;
    p.server.tap_mut().log_enabled = false;
    let id = p.client.open_stream(p.c_conn, false).unwrap();
    let data = [0x5au8; 2048];
    let mut buf = vec![0u8; 8192];
    let mut echoed = 0u64;
    let mut returned = 0u64;
    let mut round = |p: &mut Pair<Discard, Discard>, echoed: &mut u64, returned: &mut u64| {
        let n = p.client.stream_write(p.c_conn, id, &data).unwrap();
        assert_eq!(n, data.len(), "the send ring keeps up");
        for _ in 0..4 {
            p.step();
            while p.server.poll_stream_event(p.s_conn).is_some() {}
            while p.client.poll_stream_event(p.c_conn).is_some() {}
            if p.server
                .connection(p.s_conn)
                .unwrap()
                .streams
                .recv_state(id)
                .is_some()
            {
                let space = p
                    .server
                    .connection(p.s_conn)
                    .unwrap()
                    .streams
                    .write_space(id)
                    .unwrap();
                let r = p
                    .server
                    .stream_read(p.s_conn, id, &mut buf[..space.min(8192)])
                    .unwrap();
                let w = p.server.stream_write(p.s_conn, id, &buf[..r.len]).unwrap();
                assert_eq!(w, r.len);
                *echoed += r.len as u64;
            }
            let r = p.client.stream_read(p.c_conn, id, &mut buf).unwrap();
            *returned += r.len as u64;
        }
    };
    for _ in 0..100 {
        round(&mut p, &mut echoed, &mut returned);
    }
    let before = (p.client.stats(), returned);

    ALLOCS.with(|a| a.set(0));
    COUNTING.with(|c| c.set(true));
    for _ in 0..300 {
        round(&mut p, &mut echoed, &mut returned);
    }
    COUNTING.with(|c| c.set(false));
    let allocs = ALLOCS.with(Cell::get);

    let c = p.client.stats();
    assert!(returned - before.1 >= 290 * 2048, "the echo kept flowing");
    assert!(c.stream_packets_sent > before.0.stream_packets_sent + 300);
    assert_eq!(c.transport_errors + p.server.stats().transport_errors, 0);
    let streams = p.client.connection(p.c_conn).unwrap().streams.stats();
    assert!(
        streams.control_frames_sent > 0,
        "credit updates were in the window"
    );
    assert_eq!(allocs, 0, "heap allocations in the steady state");

    COUNTING.with(|c| c.set(true));
    drop(std::hint::black_box(Vec::<u8>::with_capacity(8)));
    COUNTING.with(|c| c.set(false));
    assert_eq!(ALLOCS.with(Cell::get), 1);
}
