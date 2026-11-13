//! Reverse watchpoints against a linear-scan reference, with the number of
//! comparisons bounded by the binary searches' logarithmic depth; ordering
//! rejection and sorting with a recorded reason.

use vesper_chrono::{
    ChronoError, ChronoTimelineIndex, InOutWriteEvent, IngestOrder, SnapshotEpoch,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn ceil_log2(n: usize) -> u32 {
    usize::BITS - n.saturating_sub(1).leading_zeros()
}

fn linear(
    writes: &[InOutWriteEvent],
    snaps: &[SnapshotEpoch],
    place: u64,
    before: u64,
) -> Option<(Option<SnapshotEpoch>, InOutWriteEvent)> {
    let hit = writes
        .iter()
        .filter(|w| w.place_hash == place && w.lamport_seq < before)
        .max_by_key(|w| w.lamport_seq)
        .copied()?;
    let snap = snaps
        .iter()
        .filter(|s| s.start_lamport_seq <= hit.lamport_seq)
        .max_by_key(|s| s.start_lamport_seq)
        .copied();
    Some((snap, hit))
}

#[test]
fn reverse_watch_agrees_with_linear_scan_in_logarithmic_probes() {
    for (seed, n) in [(1u64, 10usize), (2, 1_000), (3, 50_000)] {
        let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d));
        let places = 8;
        let mut index = ChronoTimelineIndex::new();
        let mut writes = Vec::new();
        let mut snaps = Vec::new();
        let mut seq = 0u64;
        for i in 0..n {
            seq += 1 + rng.next() % 3;
            if i % 97 == 0 {
                let epoch = SnapshotEpoch {
                    epoch_id: snaps.len() as u32 + 1,
                    start_lamport_seq: seq,
                    arena_cow_offset: 0,
                };
                index.record_snapshot(epoch).unwrap();
                snaps.push(epoch);
                seq += 1;
            }
            let w = InOutWriteEvent {
                lamport_seq: seq,
                fiber_id: rng.next() % 4,
                place_hash: rng.next() % places,
                span_id: i as u32,
            };
            index.record_inout_write(w).unwrap();
            writes.push(w);
        }
        let bound = 2 * (ceil_log2(n + 1) + 1);
        let mut max_probes = 0;
        for _ in 0..2_000 {
            let place = rng.next() % (places + 1); // includes a never-written place
            let before = rng.next() % (seq + 2);
            let (got, probes) = index.find_last_writer_before_probed(place, before);
            assert_eq!(
                got,
                linear(&writes, &snaps, place, before),
                "place {place} before {before}"
            );
            max_probes = max_probes.max(probes);
        }
        assert!(
            max_probes <= bound,
            "n={n}: {max_probes} comparisons exceed 2(ceil(log2(n+1))+1) = {bound}"
        );
        eprintln!("n={n}: at most {max_probes} comparisons (bound {bound}; linear scan uses {n})");
    }
}

fn w(seq: u64, place: u64) -> InOutWriteEvent {
    InOutWriteEvent {
        lamport_seq: seq,
        fiber_id: 0,
        place_hash: place,
        span_id: 0,
    }
}

#[test]
fn unordered_input_is_rejected_or_sorted_with_a_reason() {
    let unordered = vec![w(5, 1), w(2, 1), w(9, 2), w(7, 1)];
    assert_eq!(
        ChronoTimelineIndex::from_writes(unordered.clone(), IngestOrder::RequireOrdered)
            .unwrap_err(),
        ChronoError::OutOfOrder {
            previous: 5,
            got: 2
        }
    );
    let index = ChronoTimelineIndex::from_writes(
        unordered,
        IngestOrder::SortWithReason("merged from two shard rings"),
    )
    .unwrap();
    let notes = index.ordering_notes();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].reason, "merged from two shard rings");
    assert_eq!((notes[0].moved, notes[0].total), (4, 4));
    assert_eq!(
        index.find_last_writer_before(1, 7).unwrap().1.lamport_seq,
        5
    );
    assert_eq!(
        ChronoTimelineIndex::from_writes(vec![w(3, 1), w(3, 2)], IngestOrder::SortWithReason("x"))
            .unwrap_err(),
        ChronoError::DuplicateSeq { lamport_seq: 3 }
    );
    let ordered =
        ChronoTimelineIndex::from_writes(vec![w(1, 1), w(2, 1)], IngestOrder::SortWithReason("x"))
            .unwrap();
    assert!(
        ordered.ordering_notes().is_empty(),
        "no note when nothing moved"
    );

    let mut index = ChronoTimelineIndex::new();
    let s = |epoch_id, start| SnapshotEpoch {
        epoch_id,
        start_lamport_seq: start,
        arena_cow_offset: 0,
    };
    index.record_snapshot(s(1, 10)).unwrap();
    assert!(matches!(
        index.record_snapshot(s(2, 10)),
        Err(ChronoError::SnapshotOutOfOrder { .. })
    ));
    assert!(matches!(
        index.record_snapshot(s(1, 11)),
        Err(ChronoError::SnapshotOutOfOrder { .. })
    ));
    assert_eq!(index.find_last_writer_before(1, 100), None);
}
