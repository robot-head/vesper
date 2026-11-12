//! PTO lease table and EDT launch queue behaviour.

use std::collections::BTreeSet;

use vesper_warp_wheel::{
    Clock, EdtLaunchQueue, LeaseError, LeaseKey, MAX_PACKET_NUMBER, PacketNumberSpace as Space,
    ParkErrorKind, RetransmitLeases, SubmitErrorKind, VirtualClock, WheelConfig,
};

/// A linear stand-in for an in-flight packet: dropping it un-consumed panics.
#[derive(Debug, PartialEq, Eq)]
struct Lease(u64);
impl Lease {
    fn consume(self) -> u64 {
        let pn = self.0;
        std::mem::forget(self);
        pn
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            panic!("lease {} dropped without being consumed", self.0);
        }
    }
}

fn cfg(capacity: u32) -> WheelConfig {
    WheelConfig {
        tick_ns: 1_000,
        slot_bits: 5,
        levels: 3,
        capacity,
        start_ns: 0,
    }
}

#[test]
fn ack_removes_lease_and_pto_fires_the_rest() {
    let mut t = RetransmitLeases::new(cfg(64)).unwrap();
    let mut clock = VirtualClock::default();
    for pn in 0..10 {
        t.park(Space::ApplicationData, pn, 1_000_000 + pn * 10, Lease(pn))
            .unwrap();
    }
    t.park(Space::Handshake, 3, 2_000_000, Lease(1003)).unwrap();
    for pn in [1, 4, 7] {
        assert_eq!(t.on_ack(Space::ApplicationData, pn).unwrap().consume(), pn);
    }
    // Duplicate ACK and ACK for a never-sent packet are typed errors.
    assert_eq!(
        t.on_ack(Space::ApplicationData, 4).unwrap_err(),
        LeaseError::NotOutstanding
    );
    assert_eq!(
        t.on_ack(Space::Initial, 3).unwrap_err(),
        LeaseError::NotOutstanding
    );
    assert!(t.contains(LeaseKey {
        space: Space::Handshake,
        pn: 3
    }));

    clock.advance_to(999_999).unwrap();
    assert!(t.poll_pto(clock.now_ns()).unwrap().is_none());
    clock.advance_to(1_000_050).unwrap();
    let mut fired = Vec::new();
    while let Some(f) = t.poll_pto(clock.now_ns()).unwrap() {
        assert!(f.deadline_ns <= clock.now_ns());
        fired.push((f.key.pn, f.lease.consume()));
    }
    assert_eq!(fired, vec![(0, 0), (2, 2), (3, 3), (5, 5)]);
    // Fired leases are no longer ACKable.
    assert_eq!(
        t.on_ack(Space::ApplicationData, 2).unwrap_err(),
        LeaseError::NotOutstanding
    );
    // Rearm moves the PTO.
    t.rearm(Space::ApplicationData, 6, 5_000_000).unwrap();
    assert_eq!(
        t.rearm(Space::ApplicationData, 2, 1).unwrap_err(),
        LeaseError::NotOutstanding
    );
    let mut rest = Vec::new();
    while let Some(f) = t.poll_pto(3_000_000).unwrap() {
        rest.push(f.lease.consume());
    }
    assert_eq!(rest, vec![8, 9, 1003]);
    assert_eq!(t.on_ack(Space::ApplicationData, 6).unwrap().consume(), 6);
    assert!(t.is_empty());
}

#[test]
fn park_refusals_return_the_lease() {
    let mut t = RetransmitLeases::new(cfg(2)).unwrap();
    t.park(Space::Initial, 0, 10, Lease(0)).unwrap();
    let e = t.park(Space::Initial, 0, 10, Lease(100)).unwrap_err();
    assert_eq!(e.kind, ParkErrorKind::Duplicate);
    assert_eq!(e.into_lease().consume(), 100);
    let e = t
        .park(Space::Initial, MAX_PACKET_NUMBER, 10, Lease(101))
        .unwrap_err();
    assert_eq!(e.kind, ParkErrorKind::PacketNumberOutOfRange);
    assert_eq!(e.into_lease().consume(), 101);
    t.park(Space::Initial, 1, 10, Lease(1)).unwrap();
    let e = t.park(Space::Initial, 2, 10, Lease(102)).unwrap_err();
    assert_eq!(e.kind, ParkErrorKind::Full);
    assert_eq!(e.into_lease().consume(), 102);
    let mut out = Vec::new();
    t.drain(|_, l| out.push(l.consume()));
    out.sort();
    assert_eq!(out, vec![0, 1]);
}

#[test]
fn discard_space_and_index_stay_consistent_under_churn() {
    let mut t: RetransmitLeases<Lease> = RetransmitLeases::new(cfg(256)).unwrap();
    let mut model = BTreeSet::new();
    let spaces = [Space::Initial, Space::Handshake, Space::ApplicationData];
    let mut x = 1u64;
    for round in 0..2_000u64 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        let space = spaces[(x >> 33) as usize % 3];
        let pn = (x >> 40) % 300;
        let key = LeaseKey { space, pn };
        if model.contains(&key) {
            assert_eq!(t.on_ack(space, pn).unwrap().consume(), pn);
            model.remove(&key);
        } else if t
            .park(space, pn, u64::MAX / 4, Lease(pn))
            .map_err(|e| e.into_lease().consume())
            .is_ok()
        {
            model.insert(key);
        }
        if round % 500 == 499 {
            let mut gone = Vec::new();
            t.discard_space(Space::Handshake, |k, l| {
                assert_eq!(l.consume(), k.pn);
                gone.push(k);
            });
            for k in gone {
                assert!(model.remove(&k));
            }
            assert!(model.iter().all(|k| k.space != Space::Handshake));
        }
        assert_eq!(t.len(), model.len());
        for k in &model {
            assert!(t.contains(*k));
        }
    }
    t.drain(|_, l| {
        l.consume();
    });
}

#[test]
fn edt_queue_releases_in_order_and_never_early() {
    let mut q = EdtLaunchQueue::new(cfg(128), 1_000_000).unwrap();
    let mut x = 9u64;
    let mut now = 0u64;
    let mut last = 0u64;
    let mut pending = 0usize;
    for _ in 0..5_000 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        if (x >> 60) < 10 {
            let edt = now + (x >> 20) % 1_200_000;
            let edt = edt.max(last);
            match q.submit(now, edt, Lease(edt)) {
                Ok(()) => pending += 1,
                Err(e) => {
                    assert!(matches!(
                        e.kind,
                        SubmitErrorKind::BeyondHorizon | SubmitErrorKind::Full
                    ));
                    if e.kind == SubmitErrorKind::BeyondHorizon {
                        assert!(edt > now + 1_000_000);
                    }
                    e.into_packet().consume();
                }
            }
        } else {
            now += (x >> 30) % 50_000;
            while let Some(l) = q.poll_release(now).unwrap() {
                assert!(l.edt_ns <= now, "released early");
                assert!(l.edt_ns >= last, "released out of order");
                last = l.edt_ns;
                assert_eq!(l.packet.consume(), l.edt_ns);
                pending -= 1;
            }
            if let Some(hint) = q.next_wake_hint_ns() {
                assert!(hint >= now, "wake hint in the past after draining");
            }
        }
        assert_eq!(q.len(), pending);
    }
    q.drain(|_, p| {
        p.consume();
    });
}

#[test]
fn edt_horizon_and_order_rejections_return_packet() {
    let mut q = EdtLaunchQueue::new(cfg(8), 500).unwrap();
    let e = q.submit(1_000, 1_501, Lease(1)).unwrap_err();
    assert_eq!(e.kind, SubmitErrorKind::BeyondHorizon);
    assert_eq!(e.into_packet().consume(), 1);
    q.submit(1_000, 1_500, Lease(2)).unwrap();
    q.submit(1_000, 1_200, Lease(3)).unwrap();
    q.submit(1_000, 1_200, Lease(4)).unwrap();
    assert!(q.poll_release(1_199).unwrap().is_none());
    // Equal EDTs leave FIFO.
    assert_eq!(q.poll_release(1_300).unwrap().unwrap().packet.consume(), 3);
    assert_eq!(q.poll_release(1_300).unwrap().unwrap().packet.consume(), 4);
    let e = q.submit(1_300, 1_100, Lease(5)).unwrap_err();
    assert_eq!(e.kind, SubmitErrorKind::BehindReleased);
    assert_eq!(e.into_packet().consume(), 5);
    let e = q.submit(1_299, 1_400, Lease(6)).unwrap_err();
    assert_eq!(e.kind, SubmitErrorKind::ClockBackwards);
    assert_eq!(e.into_packet().consume(), 6);
    // A past-but-not-behind EDT departs on the next poll.
    q.submit(1_300, 1_250, Lease(7)).unwrap();
    assert_eq!(q.poll_release(1_300).unwrap().unwrap().packet.consume(), 7);
    assert_eq!(q.poll_release(1_500).unwrap().unwrap().packet.consume(), 2);
    assert!(q.is_empty());
}
