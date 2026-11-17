//! The Vesper build frees what it allocates: building, using, draining and
//! dropping every type many times leaves the Vesper heap where it was. The
//! Rust build has nothing to check here.
#![cfg(feature = "vesper-port")]

use vesper_warp_wheel::{
    EdtLaunchQueue, PacketNumberSpace, RetransmitLeases, Wheel, WheelConfig, port_live_allocations,
};

fn config() -> WheelConfig {
    WheelConfig {
        tick_ns: 1_000,
        slot_bits: 4,
        levels: 3,
        capacity: 64,
        start_ns: 0,
    }
}

fn cycle(round: u64) {
    let mut wheel: Wheel<u64> = Wheel::new(config()).unwrap();
    let mut leases: RetransmitLeases<u64> = RetransmitLeases::new(config()).unwrap();
    let mut edt: EdtLaunchQueue<u64> = EdtLaunchQueue::new(config(), 1_000_000).unwrap();
    for i in 0..32 {
        let _ = wheel.insert(i * 997 + round, i).unwrap();
        leases
            .park(PacketNumberSpace::Handshake, i, i * 1_500, i)
            .unwrap();
        edt.submit(0, i * 100, i).unwrap();
    }
    while wheel.poll(10_000).unwrap().is_some() {}
    while leases.poll_pto(10_000).unwrap().is_some() {}
    let mut n = 0;
    leases.discard_space(PacketNumberSpace::Handshake, |_, _| n += 1);
    wheel.drain(|_, _| n += 1);
    leases.drain(|_, _| n += 1);
    edt.drain(|_, _| n += 1);
    assert!(n > 0);
}

#[test]
fn vesper_heap_returns_to_baseline() {
    panicking_drain_sink_keeps_the_slab_consistent();
    cycle(0);
    let before = port_live_allocations();
    for round in 0..2_000 {
        cycle(round);
    }
    assert_eq!(
        port_live_allocations(),
        before,
        "the Vesper build leaked over 2000 cycles"
    );
}

/// A panicking drain sink leaves the wheel consistent: the payloads it did
/// not reach are dropped, and the freed slots take new timers. (Called from
/// the one test: the live counter is process-global, so tests that allocate
/// Vesper memory must not run in parallel with the leak check.)
fn panicking_drain_sink_keeps_the_slab_consistent() {
    let mut wheel: Wheel<String> = Wheel::new(config()).unwrap();
    for i in 0..8u64 {
        let _ = wheel.insert(1_000 + i, i.to_string()).unwrap();
    }
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wheel.drain(|_, _| panic!("sink failed"));
    }));
    assert!(unwound.is_err());
    assert!(wheel.is_empty());
    for i in 0..64u64 {
        let _ = wheel.insert(i, "again".into()).unwrap();
    }
    let mut n = 0;
    wheel.drain(|_, p| {
        assert_eq!(p, "again");
        n += 1;
    });
    assert_eq!(n, 64);
}
