//! Cross-thread stress tests of the wait-free retirement sink and mailbox.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use vesper_resonance::spsc::{SpscError, mailbox, retirement_channel};

static RT_THREAD: OnceLock<thread::ThreadId> = OnceLock::new();
static DROPS_ON_RT: AtomicU64 = AtomicU64::new(0);
static DROPS: AtomicU64 = AtomicU64::new(0);

/// A heap value whose destructor records the thread it ran on.
struct Tracked(Box<u64>);

impl Drop for Tracked {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::SeqCst);
        if RT_THREAD.get() == Some(&thread::current().id()) {
            DROPS_ON_RT.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[test]
fn zero_capacity_is_rejected() {
    assert_eq!(
        retirement_channel::<u8>(0).err(),
        Some(SpscError::ZeroCapacity)
    );
    assert_eq!(mailbox::<u8>(0).err(), Some(SpscError::ZeroCapacity));
}

#[test]
fn full_sink_hands_the_value_back() {
    let (mut sink, mut janitor) = retirement_channel::<u32>(2).unwrap();
    assert_eq!(sink.retire(1), Ok(()));
    assert_eq!(sink.retire(2), Ok(()));
    assert_eq!(sink.retire(3), Err(3));
    assert_eq!(janitor.pop(), Some(1));
    assert_eq!(sink.retire(3), Ok(()));
    assert_eq!(janitor.drain(), 2);
    assert_eq!((sink.retired(), janitor.dropped()), (3, 3));
    drop(janitor);
    assert!(sink.janitor_gone());
}

#[test]
fn swaps_per_call_are_bounded_by_entry_snapshot() {
    let (mut tx, mut rx) = mailbox::<u32>(8).unwrap();
    let (mut sink, mut janitor) = retirement_channel::<u32>(2).unwrap();
    for i in 1..=5 {
        tx.send(i).unwrap();
    }
    let mut current = 0;
    let out = rx.try_swap_in_latest(&mut current, &mut sink);
    assert_eq!((out.swapped, out.deferred, current), (2, 3, 2));
    // Sink full: nothing swapped until the janitor drains.
    assert_eq!(rx.try_swap_in_latest(&mut current, &mut sink).swapped, 0);
    assert_eq!(janitor.drain(), 2);
    let out = rx.try_swap_in_latest(&mut current, &mut sink);
    assert_eq!((out.swapped, current), (2, 4));
}

/// The realtime thread retires a million heap values through a small ring
/// while the janitor drains them. Every value arrives once, in order, and no
/// destructor runs on the realtime thread.
#[test]
fn retirement_stress_moves_every_drop_off_the_realtime_thread() {
    const N: u64 = 1_000_000;
    let (mut sink, mut janitor) = retirement_channel::<Tracked>(64).unwrap();
    // Preallocate the values on this (non-realtime) thread.
    let values: Vec<Tracked> = (0..N).map(|i| Tracked(Box::new(i))).collect();
    let rt = thread::spawn(move || {
        RT_THREAD.set(thread::current().id()).unwrap();
        let mut full_retries = 0u64;
        for mut v in values {
            loop {
                match sink.retire(v) {
                    Ok(()) => break,
                    Err(back) => {
                        v = back;
                        full_retries += 1;
                        std::hint::spin_loop();
                    }
                }
            }
        }
        (sink.retired(), full_retries)
    });
    let mut expect = 0u64;
    while expect < N {
        match janitor.pop() {
            Some(v) => {
                assert_eq!(*v.0, expect);
                expect += 1;
            }
            None => thread::yield_now(),
        }
    }
    let (retired, retries) = rt.join().unwrap();
    assert_eq!(retired, N);
    assert_eq!(janitor.dropped(), N);
    assert!(janitor.sink_gone());
    assert_eq!(DROPS_ON_RT.load(Ordering::SeqCst), 0);
    assert!(DROPS.load(Ordering::SeqCst) >= N);
    eprintln!("retirement stress: {N} values, {retries} full-ring retries");
}

/// UI thread sends numbered states; the realtime thread swaps them in and
/// retires the superseded ones. The realtime view is monotone and every
/// state is accounted for exactly once.
#[test]
fn mailbox_stress_swaps_monotonically_and_loses_nothing() {
    const N: u64 = 200_000;
    let (mut tx, mut rx) = mailbox::<Box<u64>>(8).unwrap();
    let (mut sink, mut janitor) = retirement_channel::<Box<u64>>(4).unwrap();
    let ui = thread::spawn(move || {
        for i in 1..=N {
            let mut s = Box::new(i);
            loop {
                match tx.send(s) {
                    Ok(()) => break,
                    Err(back) => {
                        s = back;
                        thread::yield_now();
                    }
                }
            }
        }
    });
    let collector = thread::spawn(move || {
        let mut seen = Vec::new();
        loop {
            match janitor.pop() {
                Some(v) => seen.push(*v),
                None if janitor.sink_gone() && janitor.pending() == 0 => break,
                None => thread::yield_now(),
            }
        }
        seen
    });
    let mut current = Box::new(0u64);
    let mut swaps = 0usize;
    while *current < N {
        let before = *current;
        let out = rx.try_swap_in_latest(&mut current, &mut sink);
        assert!(*current >= before);
        swaps += out.swapped;
    }
    ui.join().unwrap();
    drop(sink);
    let mut retired = collector.join().unwrap();
    assert_eq!(swaps as u64, N);
    retired.sort_unstable();
    assert_eq!(retired, (0..N).collect::<Vec<_>>());
    assert_eq!(*current, N);
}
