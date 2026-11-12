//! SPSC ring semantics: power-of-two sizes, cached indices, u32 wrap-around,
//! all-or-nothing reserve, partial peek, and window checks.

use vesper_warp_umem::{ConsumerRing, ProducerRing, Ring, RingError, pop_one, push_one};

#[test]
fn rejects_non_power_of_two_sizes() {
    for size in [0, 3, 6, 100] {
        assert_eq!(Ring::<u64>::new(size).unwrap_err(), RingError::InvalidSize);
    }
    assert!(Ring::<u64>::new(1).is_ok());
    assert_eq!(Ring::<u64>::new(16).unwrap().capacity(), 16);
}

#[test]
fn reserve_is_all_or_nothing_and_peek_is_partial() {
    let mut ring = Ring::<u64>::new(8).unwrap();
    let idx = ProducerRing::reserve(&mut ring, 5).unwrap();
    for i in 0..5 {
        ring.write(idx.wrapping_add(i), u64::from(i) * 10).unwrap();
    }
    ProducerRing::submit(&mut ring, 5).unwrap();
    assert_eq!(ProducerRing::reserve(&mut ring, 4), None, "only 3 free");
    let idx = ProducerRing::reserve(&mut ring, 3).unwrap();
    ProducerRing::unreserve(&mut ring, 3).unwrap();
    assert_eq!(ring.len(), 5);
    let _ = idx;

    let (n, first) = ring.peek(2);
    assert_eq!(n, 2);
    assert_eq!(ring.read(first).unwrap(), 0);
    assert_eq!(ring.read(first.wrapping_add(1)).unwrap(), 10);
    assert!(matches!(
        ring.read(first.wrapping_add(2)),
        Err(RingError::IndexOutsideWindow { .. })
    ));
    ConsumerRing::release(&mut ring, 2).unwrap();
    // Peek returns what remains of the cached batch first, then refreshes.
    let (n, _) = ring.peek(10);
    assert_eq!(n, 3);
    ConsumerRing::unpeek(&mut ring, 3).unwrap();
    let (n, first) = ring.peek(10);
    assert_eq!(n, 3);
    assert_eq!(ring.read(first).unwrap(), 20);
    ConsumerRing::release(&mut ring, 3).unwrap();
    assert!(ring.is_empty());
}

#[test]
fn window_errors_are_typed() {
    let mut ring = Ring::<u64>::new(4).unwrap();
    assert!(matches!(
        ring.write(0, 1),
        Err(RingError::IndexOutsideWindow { idx: 0 })
    ));
    let idx = ProducerRing::reserve(&mut ring, 2).unwrap();
    assert_eq!(
        ProducerRing::submit(&mut ring, 3),
        Err(RingError::CountExceedsWindow {
            requested: 3,
            outstanding: 2
        })
    );
    ring.write(idx, 1).unwrap();
    // Partial submit publishes the first reserved slot only.
    ProducerRing::submit(&mut ring, 1).unwrap();
    assert_eq!(ring.len(), 1);
    ring.write(idx.wrapping_add(1), 2).unwrap();
    ProducerRing::submit(&mut ring, 1).unwrap();
    assert_eq!(ring.committed().collect::<Vec<_>>(), [1, 2]);
    assert_eq!(
        ConsumerRing::release(&mut ring, 1),
        Err(RingError::CountExceedsWindow {
            requested: 1,
            outstanding: 0
        })
    );
}

#[test]
fn indices_wrap_modulo_two_to_the_32() {
    for start in [u32::MAX - 2, u32::MAX, 0x8000_0000 - 1] {
        let mut ring = Ring::<u64>::with_initial_index(4, start).unwrap();
        let mut next = 0u64;
        let mut expect = 0u64;
        for round in 0..50u64 {
            let burst = (round % 4) + 1;
            for _ in 0..burst {
                if push_one(&mut ring, next) {
                    next += 1;
                }
            }
            assert!(ring.len() <= 4);
            for _ in 0..(round % 3) + 1 {
                if let Some(v) = pop_one(&mut ring) {
                    assert_eq!(v, expect, "FIFO order across the wrap");
                    expect += 1;
                }
            }
        }
        while let Some(v) = pop_one(&mut ring) {
            assert_eq!(v, expect);
            expect += 1;
        }
        assert_eq!(expect, next);
        assert!(
            ring.producer_index() != start,
            "indices advanced past the wrap"
        );
    }
}

#[test]
fn producer_cache_refreshes_only_when_short() {
    let mut ring = Ring::<u64>::new(4).unwrap();
    assert_eq!(ProducerRing::free_entries(&mut ring, 4), 4);
    for v in 0..4 {
        assert!(push_one(&mut ring, v));
    }
    assert_eq!(ProducerRing::free_entries(&mut ring, 1), 0);
    assert_eq!(pop_one(&mut ring), Some(0));
    // A cached view saying "0 free" is refreshed from the shared consumer.
    assert_eq!(ProducerRing::free_entries(&mut ring, 1), 1);
    assert!(push_one(&mut ring, 4));
    assert!(!push_one(&mut ring, 5));
}
