//! Handle, capacity, clock and leak behaviour of the wheel.

use vesper_warp_wheel::{
    AdvanceError, HandleError, InsertErrorKind, MAX_TIME_NS, VirtualClock, Wheel, WheelConfig,
    WheelConfigError, leaked_timer_entries,
};

fn small(capacity: u32) -> Wheel<String> {
    Wheel::new(WheelConfig::with_capacity(capacity)).unwrap()
}

#[test]
fn stale_handles_after_cancel_and_fire_are_rejected() {
    let mut w = small(2);
    let a = w.insert(10_000, "a".into()).unwrap();
    let b = w.insert(20_000, "b".into()).unwrap();
    assert_eq!(w.cancel(a).unwrap(), "a");
    // Slot reuse does not revive the old handle: generations differ.
    let c = w.insert(5_000, "c".into()).unwrap();
    assert_eq!(w.poll(30_000).unwrap().unwrap().payload, "c");
    assert_eq!(w.poll(30_000).unwrap().unwrap().payload, "b");
    assert_eq!(w.cancel(b), Err(HandleError::Stale));
    assert_eq!(w.deadline(&c), Err(HandleError::Stale));
    assert_eq!(w.cancel(c), Err(HandleError::Stale));
    assert!(w.is_empty());
}

#[test]
fn foreign_handles_are_rejected() {
    let mut w1 = small(4);
    let mut w2 = small(4);
    let h1 = w1.insert(1, "one".into()).unwrap();
    let h2 = w2.insert(1, "two".into()).unwrap();
    assert_eq!(w2.cancel(h1), Err(HandleError::Foreign));
    assert_eq!(w2.cancel(h2).unwrap(), "two");
    assert_eq!(w1.len(), 1);
    w1.drain(|_, _| {});
}

#[test]
fn capacity_exhaustion_returns_payload() {
    let mut w = small(1);
    let _h = w.insert(1, "kept".into()).unwrap();
    let err = w.insert(2, "refused".into()).unwrap_err();
    assert_eq!(err.kind, InsertErrorKind::Full);
    assert_eq!(err.into_payload(), "refused");
    let err = small(1).insert(MAX_TIME_NS + 1, "far".into()).unwrap_err();
    assert_eq!(err.kind, InsertErrorKind::DeadlineOutOfRange);
    w.drain(|_, _| {});
}

#[test]
fn clock_never_moves_backwards() {
    let mut w = small(1);
    w.advance(100).unwrap();
    assert_eq!(
        w.advance(99),
        Err(AdvanceError::Backwards {
            now_ns: 100,
            requested_ns: 99
        })
    );
    assert_eq!(w.advance(u64::MAX), Err(AdvanceError::OutOfRange));
    w.advance(MAX_TIME_NS).unwrap();
    let mut clock = VirtualClock::starting_at(5);
    assert!(clock.advance_to(4).is_err());
    assert_eq!(clock.advance_by(10), Ok(15));
    assert!(clock.advance_by(u64::MAX).is_err());
}

#[test]
fn invalid_configs_are_rejected() {
    let base = WheelConfig::with_capacity(4);
    let bad = |c: WheelConfig| Wheel::<u8>::new(c).unwrap_err();
    assert_eq!(
        bad(WheelConfig { tick_ns: 0, ..base }),
        WheelConfigError::ZeroTick
    );
    assert_eq!(
        bad(WheelConfig {
            slot_bits: 7,
            ..base
        }),
        WheelConfigError::SlotBits(7)
    );
    assert_eq!(
        bad(WheelConfig { levels: 11, ..base }),
        WheelConfigError::Levels(11)
    );
    assert_eq!(
        bad(WheelConfig {
            capacity: 0,
            ..base
        }),
        WheelConfigError::Capacity(0)
    );
    assert_eq!(
        bad(WheelConfig {
            start_ns: u64::MAX,
            ..base
        }),
        WheelConfigError::StartOutOfRange
    );
}

#[test]
fn far_jump_with_overflow_entry_is_cheap_and_exact() {
    // One entry ~2^60 ticks away: advance must skip, not walk, the ticks.
    let mut w: Wheel<u8> = Wheel::new(WheelConfig {
        tick_ns: 1,
        slot_bits: 6,
        levels: 2,
        capacity: 2,
        start_ns: 0,
    })
    .unwrap();
    let _h = w.insert(1 << 60, 1).unwrap();
    assert!(w.poll((1 << 60) - 1).unwrap().is_none());
    assert_eq!(w.next_wake_hint_ns(), Some(1 << 60));
    assert_eq!(w.poll(1 << 60).unwrap().unwrap().deadline_ns, 1 << 60);
}

/// The leak counter is process-global, so both cases live in one test: a
/// parallel test dropping a non-empty wheel would otherwise race with it.
#[test]
fn leak_reporting_on_drop() {
    let before = leaked_timer_entries();
    let mut w = small(4);
    let _ = w.insert(1, "x".into()).unwrap();
    let mut got = Vec::new();
    w.drain(|d, p| got.push((d, p)));
    drop(w);
    assert_eq!(got, vec![(1, "x".to_string())]);
    assert_eq!(
        leaked_timer_entries(),
        before,
        "drained wheel reported a leak"
    );

    let mut w = small(4);
    let _ = w.insert(1, "leak".into()).unwrap();
    let _ = w.insert(2, "leak".into()).unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(w)));
    assert_eq!(leaked_timer_entries(), before + 2);
    if cfg!(debug_assertions) {
        let err = outcome.expect_err("debug builds panic on a non-empty drop");
        let msg = err
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or("");
        assert!(msg.contains("dropped with 2 pending entries"), "{msg}");
    } else {
        assert!(outcome.is_ok());
    }
}
