//! "0 Alloc": `on_ack` and `schedule_edt_ns` (plus the send/loss/ECN paths
//! used in steady state) perform no heap allocation after warm-up,
//! measured on the calling thread with a counting global allocator. The
//! synchronous controller does not dispatch work to other threads; libtest's
//! unrelated threads must not contribute to this operation's measurement.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vesper_warp_bbr::{
    AckedPacket, BbrConfig, Bbrv3, Delivered, LostPacket, PacketDeliveryState, RttSample,
    SentPacket,
};

struct Counting;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    alloc: u64,
    zeroed: u64,
    realloc: u64,
}

impl Counts {
    const ZERO: Self = Self {
        alloc: 0,
        zeroed: 0,
        realloc: 0,
    };

    fn total(self) -> u64 {
        self.alloc
            .saturating_add(self.zeroed)
            .saturating_add(self.realloc)
    }
}

#[derive(Clone, Copy)]
struct Counter {
    active: bool,
    in_hook: bool,
    counts: Counts,
}

thread_local! {
    // Const, non-Drop TLS needs no allocating lazy initializer. The hook
    // never formats, borrows a RefCell, or looks up a thread's identity.
    static COUNTER: Cell<Counter> = const { Cell::new(Counter {
        active: false,
        in_hook: false,
        counts: Counts::ZERO,
    }) };
}

enum Allocation {
    Alloc,
    Zeroed,
    Realloc,
}

fn record(kind: Allocation) {
    // Allocations during TLS teardown must still reach System. A reentrant
    // hook cannot borrow or initialize another heap-backed observer.
    let _ = COUNTER.try_with(|local| {
        let mut counter = local.get();
        if !counter.active || counter.in_hook {
            return;
        }
        counter.in_hook = true;
        local.set(counter);
        let count = match kind {
            Allocation::Alloc => &mut counter.counts.alloc,
            Allocation::Zeroed => &mut counter.counts.zeroed,
            Allocation::Realloc => &mut counter.counts.realloc,
        };
        *count = count.saturating_add(1);
        counter.in_hook = false;
        local.set(counter);
    });
}

struct Measurement {
    finished: bool,
    // A measurement must finish on the thread where it began.
    _not_send: PhantomData<*mut ()>,
}

impl Measurement {
    fn start() -> Self {
        COUNTER.with(|local| {
            assert!(!local.get().active, "nested allocation measurement");
            local.set(Counter {
                active: true,
                in_hook: false,
                counts: Counts::ZERO,
            });
        });
        Self {
            finished: false,
            _not_send: PhantomData,
        }
    }

    fn finish(mut self) -> Counts {
        let counts = COUNTER.with(|local| {
            let mut counter = local.get();
            counter.active = false;
            local.set(counter);
            counter.counts
        });
        self.finished = true;
        counts
    }
}

impl Drop for Measurement {
    fn drop(&mut self) {
        if !self.finished {
            let _ = COUNTER.try_with(|local| {
                let mut counter = local.get();
                counter.active = false;
                local.set(counter);
            });
        }
    }
}

fn measure<T>(f: impl FnOnce() -> T) -> (T, Counts) {
    let measurement = Measurement::start();
    let result = f();
    (result, measurement.finish())
}

// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(Allocation::Alloc);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(Allocation::Zeroed);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(Allocation::Realloc);
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const RING: usize = 256;

/// A fixed-capacity ring of outstanding packets (no allocation).
struct Ring {
    slots: [(SentPacket, PacketDeliveryState); RING],
    head: usize,
    len: usize,
}

fn step(bbr: &mut Bbrv3, ring: &mut Ring, now: &mut u64, pn: &mut u64, loss_every: u64) {
    // Send while the guard allows, paced by EDT.
    while bbr.can_send() && ring.len < RING {
        let launch = bbr.schedule_edt_ns(1200, *now);
        *now = launch;
        let packet = SentPacket {
            pn: *pn,
            sent_ns: launch,
            bytes: 1200,
            ack_eliciting: true,
            in_flight: true,
        };
        *pn += 1;
        let state = bbr.on_packet_sent(launch, packet).unwrap();
        let tail = (ring.head + ring.len) % RING;
        ring.slots[tail] = (packet, state);
        ring.len += 1;
    }
    // ACK (or lose) the oldest packet one fixed RTT after it was sent.
    let (packet, state) = ring.slots[ring.head];
    ring.head = (ring.head + 1) % RING;
    ring.len -= 1;
    *now = (*now).max(packet.sent_ns + 20_000_000);
    if loss_every != 0 && packet.pn % loss_every == 0 {
        bbr.on_ecn_ce(*now, 1).unwrap();
        bbr.on_packet_lost(
            *now,
            LostPacket {
                pn: packet.pn,
                sent_ns: packet.sent_ns,
                bytes: packet.bytes,
            },
            state,
        )
        .unwrap();
    } else {
        let acked = [Delivered {
            packet: AckedPacket {
                pn: packet.pn,
                sent_ns: packet.sent_ns,
                bytes: packet.bytes,
            },
            state,
            previously_lost: false,
        }];
        let rtt = *now - packet.sent_ns;
        bbr.on_ack(
            *now,
            &acked,
            Some(RttSample {
                latest_ns: rtt,
                min_ns: 20_000_000,
                smoothed_ns: rtt,
                ack_delay_ns: 0,
            }),
        )
        .unwrap();
    }
}

#[test]
fn steady_state_ack_and_pacing_paths_do_not_allocate() {
    let mut bbr = Bbrv3::new(BbrConfig::quic(1200), 0).unwrap();
    let empty = SentPacket {
        pn: 0,
        sent_ns: 0,
        bytes: 0,
        ack_eliciting: false,
        in_flight: false,
    };
    let blank = bbr.on_packet_sent(0, empty).unwrap();
    let mut ring = Ring {
        slots: [(empty, blank); RING],
        head: 0,
        len: 0,
    };
    let (mut now, mut pn) = (1u64, 1u64);
    // Warm-up: Startup, Drain, into ProbeBW.
    for _ in 0..5_000 {
        step(&mut bbr, &mut ring, &mut now, &mut pn, 0);
    }
    assert!(bbr.full_bw_reached());
    // Steady state across ProbeBW phases and ProbeRTT (> 10 s of virtual
    // time), with periodic loss and CE marks.
    let ((), counts) = measure(|| {
        for _ in 0..200_000 {
            step(&mut bbr, &mut ring, &mut now, &mut pn, 97);
        }
    });
    assert!(now > 15_000_000_000, "covered a ProbeRTT interval: {now}");
    assert_eq!(counts.total(), 0, "heap allocations in steady state");
}

fn exercise_allocation_entrypoints() -> Counts {
    let layout = Layout::from_size_align(64, 8).unwrap();
    let grown = Layout::from_size_align(128, 8).unwrap();
    let ((ptr, zeroed), counts) = measure(|| {
        // SAFETY: nonzero valid layouts; successful allocations are read or
        // written only within their sizes, and deallocated after measuring.
        unsafe {
            let ptr = std::alloc::alloc(layout);
            assert!(!ptr.is_null());
            ptr.write(17);
            let zeroed = std::alloc::alloc_zeroed(layout);
            assert!(!zeroed.is_null());
            assert_eq!(zeroed.read(), 0);
            let ptr = std::alloc::realloc(ptr, layout, grown.size());
            assert!(!ptr.is_null());
            assert_eq!(ptr.read(), 17);
            (std::hint::black_box(ptr), std::hint::black_box(zeroed))
        }
    });
    // SAFETY: these are the still-live successful allocations above, with
    // their current layouts. Deallocation is not an allocation event.
    unsafe {
        std::alloc::dealloc(ptr, grown);
        std::alloc::dealloc(zeroed, layout);
    }
    counts
}

#[test]
fn calling_thread_allocations_are_detected_and_each_window_resets() {
    for _ in 0..2 {
        assert_eq!(
            exercise_allocation_entrypoints(),
            Counts {
                alloc: 1,
                zeroed: 1,
                realloc: 1,
            }
        );
    }
    assert_eq!(measure(|| ()).1, Counts::ZERO);
}

#[test]
fn another_threads_real_allocations_are_isolated() {
    let start = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let worker_start = Arc::clone(&start);
    let worker_done = Arc::clone(&done);
    // Spawn/join and all Arc allocation happen outside the measured window.
    let worker = std::thread::spawn(move || {
        struct SignalOnDrop(Arc<AtomicBool>);
        impl Drop for SignalOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _done = SignalOnDrop(worker_done);
        while !worker_start.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        exercise_allocation_entrypoints()
    });
    let ((), calling_thread) = measure(|| {
        start.store(true, Ordering::Release);
        while !done.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
    });
    let other_thread = worker.join().unwrap();
    assert_eq!(calling_thread, Counts::ZERO);
    assert_eq!(
        other_thread.total(),
        3,
        "worker really allocated in the window"
    );
}
