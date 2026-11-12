//! Zero-allocation evidence: after construction and warm-up, steady-state
//! insert / cancel / advance / poll on the wheel, the lease table and the EDT
//! queue perform no heap allocation on this thread.
//!
//! The counting allocator is test-only and needs `unsafe impl GlobalAlloc`;
//! the library itself forbids unsafe code.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use vesper_warp_wheel::{EdtLaunchQueue, PacketNumberSpace, RetransmitLeases, Wheel, WheelConfig};

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: delegates every call to `System`; only bumps a thread-local counter.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: forwarded with the caller's layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocs() -> u64 {
    ALLOCS.with(Cell::get)
}

fn config(capacity: u32) -> WheelConfig {
    WheelConfig {
        tick_ns: 1_000,
        slot_bits: 4,
        levels: 3,
        capacity,
        start_ns: 0,
    }
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
}

#[test]
fn steady_state_wheel_does_not_allocate() {
    // The counter observes this thread's allocations.
    let probe = allocs();
    drop(std::hint::black_box(Vec::<u8>::with_capacity(16)));
    assert!(allocs() > probe, "counting allocator is not active");
    let mut wheel: Wheel<[u64; 4]> = Wheel::new(config(1024)).unwrap();
    let mut handles = Vec::with_capacity(1024);
    let mut rng = Lcg(7);
    let mut now = 0u64;
    let mut step = |wheel: &mut Wheel<[u64; 4]>, handles: &mut Vec<_>, now: &mut u64| {
        for _ in 0..64 {
            // Near, mid and far (overflow: span is 2^12 ticks) deadlines.
            let delta = match rng.next() % 4 {
                0 => rng.next() % 5_000,
                1 => rng.next() % 500_000,
                2 => rng.next() % 50_000_000,
                _ => 0,
            };
            if let Ok(h) = wheel.insert(*now + delta, [delta; 4])
                && handles.len() < handles.capacity()
            {
                handles.push(h);
            }
        }
        for _ in 0..16 {
            if let Some(h) = handles.pop() {
                let _ = wheel.cancel(h);
            }
        }
        *now += rng.next() % 3_000_000;
        while let Some(e) = wheel.poll(*now).unwrap() {
            drop(std::hint::black_box(e));
        }
    };
    for _ in 0..200 {
        step(&mut wheel, &mut handles, &mut now);
    }
    let before = allocs();
    for _ in 0..2_000 {
        step(&mut wheel, &mut handles, &mut now);
    }
    assert_eq!(allocs() - before, 0, "steady-state wheel path allocated");
    assert_eq!(wheel.capacity(), 1024);
    handles.clear();
    wheel.drain(|_, p| {
        std::hint::black_box(p);
    });
}

#[test]
fn steady_state_leases_and_edt_do_not_allocate() {
    let mut leases: RetransmitLeases<u64> = RetransmitLeases::new(config(512)).unwrap();
    let mut edt: EdtLaunchQueue<u64> = EdtLaunchQueue::new(config(512), 10_000_000).unwrap();
    let mut pn = 0u64;
    let mut now = 0u64;
    let mut edt_next = 0u64;
    let mut rng = Lcg(11);
    let mut step = |leases: &mut RetransmitLeases<u64>, edt: &mut EdtLaunchQueue<u64>| {
        for _ in 0..8 {
            let _ = leases.park(PacketNumberSpace::ApplicationData, pn, now + 25_000_000, pn);
            edt_next = edt_next.max(now) + rng.next() % 20_000;
            if let Err(e) = edt.submit(now, edt_next, pn) {
                std::hint::black_box(e.into_packet());
            }
            pn += 1;
        }
        for _ in 0..6 {
            let acked = pn.saturating_sub(1 + rng.next() % 40);
            let _ = leases.on_ack(PacketNumberSpace::ApplicationData, acked);
        }
        now += rng.next() % 100_000;
        while let Some(f) = leases.poll_pto(now).unwrap() {
            drop(std::hint::black_box(f));
        }
        while let Some(l) = edt.poll_release(now).unwrap() {
            drop(std::hint::black_box(l));
        }
    };
    for _ in 0..300 {
        step(&mut leases, &mut edt);
    }
    let before = allocs();
    for _ in 0..3_000 {
        step(&mut leases, &mut edt);
    }
    assert_eq!(
        allocs() - before,
        0,
        "steady-state lease/EDT path allocated"
    );
    leases.drain(|_, l| {
        std::hint::black_box(l);
    });
    edt.drain(|_, p| {
        std::hint::black_box(p);
    });
}
