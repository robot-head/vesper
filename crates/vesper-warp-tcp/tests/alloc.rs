//! Steady-state segment processing performs zero heap allocations.
//!
//! A counting global allocator observes the whole warmed-up exchange:
//! segment emission, checksum, parsing, ACK/SACK processing, out-of-order
//! insertion into the caller's ring, reassembly, retransmission and timers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use vesper_warp_tcp::conn::{Config, SendView};
use vesper_warp_tcp::rtt::RtoConfig;
use vesper_warp_tcp::{Connection, IpPair, State};

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
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const MS: u64 = 1_000_000;
const SLOTS: usize = 512;

struct Wire {
    bufs: Vec<[u8; 1600]>,
    lens: Vec<usize>,
    n: usize,
}

impl Wire {
    fn new() -> Wire {
        Wire {
            bufs: vec![[0u8; 1600]; SLOTS],
            lens: vec![0; SLOTS],
            n: 0,
        }
    }
}

fn cfg(port: u16) -> Config {
    Config {
        local_port: port,
        mss: 1460,
        recv_buffer: 128 * 1024,
        rto: RtoConfig {
            initial_ns: 200 * MS,
            min_ns: 50 * MS,
            max_ns: 2_000 * MS,
            granularity_ns: MS,
        },
        delayed_ack_ns: 5 * MS,
        max_in_flight_segments: 256,
        ..Config::default()
    }
}

struct S {
    a: Connection,
    b: Connection,
    ips: IpPair,
    data: Vec<u8>,
    ring_a: Vec<u8>,
    ring_b: Vec<u8>,
    ab: Wire,
    ba: Wire,
    now: u64,
    written: u64,
    delivered: u64,
    stream_ok: bool,
    sent_count: u64,
}

impl S {
    fn round(&mut self, count: bool) {
        COUNTING.store(count, Ordering::Relaxed);
        self.now += MS;
        let now = self.now;
        let (a, b) = (&mut self.a, &mut self.b);
        if a.state() == State::Established
            && self.written < self.data.len() as u64
            && a.written() - a.acked_offset() < 512 * 1024
        {
            let n = 64 * 1024;
            a.write(n).unwrap();
            self.written += n;
        }
        // A -> B, dropping every 97th data segment (exercises SACK/OOO).
        self.ab.n = 0;
        let view = SendView {
            base: 0,
            bytes: &self.data,
        };
        while self.ab.n < SLOTS {
            let k = self.ab.n;
            let Some(t) = a.poll_transmit(now, &view, &mut self.ab.bufs[k]).unwrap() else {
                break;
            };
            self.sent_count += 1;
            if t.len > 100 && self.sent_count.is_multiple_of(97) {
                continue;
            }
            self.ab.lens[k] = t.len;
            self.ab.n += 1;
        }
        for k in 0..self.ab.n {
            let rec = b
                .on_segment(
                    now,
                    self.ips,
                    &self.ab.bufs[k][..self.ab.lens[k]],
                    &mut self.ring_b,
                )
                .unwrap();
            for c in rec.chunks() {
                let off = self.delivered as usize;
                self.stream_ok &= c == &self.data[off..off + c.len()];
                self.delivered += c.len() as u64;
            }
        }
        let unread = b.unread();
        b.recv_release(unread).unwrap();
        self.ba.n = 0;
        while self.ba.n < SLOTS {
            let k = self.ba.n;
            let Some(t) = b
                .poll_transmit(now, &SendView::EMPTY, &mut self.ba.bufs[k])
                .unwrap()
            else {
                break;
            };
            self.ba.lens[k] = t.len;
            self.ba.n += 1;
        }
        for k in 0..self.ba.n {
            let rec = a
                .on_segment(
                    now,
                    self.ips.reversed(),
                    &self.ba.bufs[k][..self.ba.lens[k]],
                    &mut self.ring_a,
                )
                .unwrap();
            assert!(rec.is_empty());
        }
        while a.take_released().is_some() {}
        for c in [a, b] {
            if c.poll_timeout().is_some_and(|d| d <= now) {
                c.on_timeout(now);
            }
            while c.take_event().is_some() {}
        }
        COUNTING.store(false, Ordering::Relaxed);
    }
}

#[test]
fn steady_state_is_allocation_free() {
    let ips = IpPair::V4 {
        src: [10, 0, 0, 1],
        dst: [10, 0, 0, 2],
    };
    let mut a = Connection::new(cfg(1000)).unwrap();
    let mut b = Connection::new(cfg(2000)).unwrap();
    b.listen(7).unwrap();
    a.connect(0, ips, 2000, 9).unwrap();
    let mut s = S {
        a,
        b,
        ips,
        data: (0..32u32 * 1024 * 1024).map(|i| (i % 251) as u8).collect(),
        ring_a: vec![0u8; 128 * 1024],
        ring_b: vec![0u8; 128 * 1024],
        ab: Wire::new(),
        ba: Wire::new(),
        now: 0,
        written: 0,
        delivered: 0,
        stream_ok: true,
        sent_count: 0,
    };
    for _ in 0..300 {
        s.round(false);
    }
    assert_eq!(s.a.state(), State::Established);
    assert!(
        s.delivered > 1024 * 1024,
        "warm-up moved data: {}",
        s.delivered
    );
    let before = s.delivered;
    let stats_before = s.a.stats();
    ALLOCS.store(0, Ordering::Relaxed);
    for _ in 0..2000 {
        s.round(true);
    }
    let allocs = ALLOCS.load(Ordering::Relaxed);
    let st = s.a.stats();
    assert!(
        s.delivered - before > 8 * 1024 * 1024,
        "steady state moved data: {}",
        s.delivered - before
    );
    assert!(
        st.retransmits > stats_before.retransmits,
        "loss recovery exercised: {st:?}"
    );
    assert!(st.recoveries > stats_before.recoveries, "{st:?}");
    assert!(s.b.ooo_ranges() > 0 || s.b.stats().ooo_dropped == 0);
    assert!(s.stream_ok, "byte stream intact");
    assert_eq!(allocs, 0, "heap allocations in steady state");
}
