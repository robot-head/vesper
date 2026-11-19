//! The export ABI widening library (#1087) end to end: `vesper/widen.ves`
//! linked through the committed `port/widen.rs`. The expected values are
//! the ones `tests/native_export_*.rs` in the compiler get from the
//! evaluator and the JIT for the same functions. Vesper build only.
#![cfg(feature = "vesper-port")]

use std::sync::Mutex;
use vesper_port_pilot::widen::{self, Bank, Fabric, Graph, Pt, repr};

/// The library's allocation counter is global: tests that allocate take
/// turns, so each can check that it freed everything it made.
static LIVE: Mutex<()> = Mutex::new(());

fn settled<R>(f: impl FnOnce() -> R) -> R {
    let _turn = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    let before = widen::live_allocations();
    let r = f();
    assert_eq!(
        widen::live_allocations(),
        before,
        "Vesper allocations leaked"
    );
    r
}

#[test]
fn f32_scalars_cross_in_float_registers() {
    assert_eq!(widen::scale(1.5, 2.0), 3.5);
    assert_eq!(widen::scale(0.1, 3.0), 0.1f32 * 3.0 + 0.5);
    assert_eq!(widen::scale(-0.0, 1.0), 0.5);
    assert!(widen::scale(f32::NAN, 1.0).is_nan());
    assert_eq!(widen::scale(f32::MAX, 2.0), f32::INFINITY);
    assert_eq!(widen::halve(0.0), None);
    assert_eq!(widen::halve(3.0), Some(1.5));
    assert_eq!(
        widen::halve(f32::MIN_POSITIVE),
        Some(f32::MIN_POSITIVE / 2.0)
    );
}

#[test]
fn f32_slices_and_sequences() {
    settled(|| {
        assert_eq!(widen::total(&[1.0, 2.5, -0.5]), 3.0);
        assert_eq!(widen::total(&[]), 0.0);
        let big = (0..1000).map(|i| i as f32 * 0.125).collect::<Vec<_>>();
        assert_eq!(widen::total(&big), big.iter().sum::<f32>());
        assert_eq!(widen::ramp(4, 0.25), vec![0.0, 0.25, 0.5, 0.75]);
        assert!(widen::ramp(0, 1.0).is_empty());
    });
}

#[test]
fn f32_mirror_fields_are_lent_in_place() {
    let mut p = repr::Pt::from_value(&Pt {
        x: -1.5,
        y: 2.0,
        tag: 7,
    });
    assert_eq!(widen::pt_norm1(&p), 3.5);
    widen::pt_nudge(&mut p, 0.5);
    assert_eq!(
        p.to_value(),
        Pt {
            x: -1.0,
            y: 1.5,
            tag: 8
        }
    );
    assert_eq!(p.x.get(), -1.0);
}

#[test]
fn in_handles_are_lent_without_a_copy() {
    settled(|| {
        let mut b = Bank::new(10, 4);
        assert_eq!(b.read(2), 12);
        assert_eq!(b.read(9), 10, "out of range reads the base");
        b.write(1, 99);
        assert_eq!(widen::bank_read(&b, 1), 99);
        assert_eq!(b.as_ref().regs().to_vec(), vec![10, 99, 12, 13]);
        // Reading in place leaves the handle as it was.
        let words = b.as_ref().regs().len();
        for i in 0..1000 {
            assert_eq!(b.read(i % 4), [10, 99, 12, 13][(i % 4) as usize]);
        }
        assert_eq!(b.as_ref().regs().len(), words);
    });
}

#[test]
fn handles_own_sequences_of_handles() {
    settled(|| {
        let mut f = Fabric::new(3, 4);
        assert_eq!(f.len(), 3);
        assert_eq!(f.as_ref().files().len(), 3);
        assert_eq!(f.as_ref().names().len(), 3);
        let last = f.pop().expect("a file");
        assert_eq!(last.read(1), 201);
        assert_eq!(f.len(), 2);
        drop(last);
        // Dropping the fabric drops both banks still in it, their
        // registers and the names.
        drop(f);
        // An emptied fabric drops nothing more.
        let mut e = Fabric::new(1, 1);
        assert!(e.pop().is_some());
        assert!(e.pop().is_none());
        assert!(e.as_ref().files().is_empty());
    });
    settled(|| {
        for _ in 0..200 {
            let _ = Fabric::new(4, 8);
        }
    });
}

#[test]
fn a_graph_of_boxed_objects_lives_in_vesper_across_calls() {
    settled(|| {
        // gain(2) -> delay(2) -> gain(3): the output is 6x the input of two
        // calls before.
        let mut g = Graph::new(2, 2, true);
        assert_eq!(g.run(1), Ok(0));
        assert_eq!(g.run(2), Ok(0));
        assert_eq!(g.run(5), Ok(6));
        assert_eq!(g.run(7), Ok(12));
        assert_eq!(g.runs(), 4);
        assert_eq!(g.as_ref().procs().len(), 3);
        assert!(g.as_ref().tail().is_some_and(|t| t.is_present()));
        let no_tail = Graph::new(1, 0, false);
        assert!(no_tail.as_ref().tail().is_none());
    });
    settled(|| {
        for n in 0..100 {
            let mut g = Graph::new(3, n, n % 2 == 0);
            for x in 0..n as i64 {
                let _ = g.run(x);
            }
        }
    });
}
