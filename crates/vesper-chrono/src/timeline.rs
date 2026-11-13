//! Snapshot-epoch timeline index and O(log N) reverse `inout` watchpoints
//! (§31.5), with the ordering requirement made explicit.
//!
//! Binary search is only correct over ordered events (expansion-37 review), so
//! the index either rejects out-of-order input or sorts it and records why.

use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEpoch {
    pub epoch_id: u32,
    /// The image is the state after every record with a smaller sequence.
    pub start_lamport_seq: u64,
    /// Offset of the image in the bundle's snapshot table (the design's CoW
    /// arena offset; no copy-on-write arena is implemented here).
    pub arena_cow_offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InOutWriteEvent {
    pub lamport_seq: u64,
    pub fiber_id: u64,
    pub place_hash: u64,
    pub span_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChronoError {
    /// Writes must arrive in strictly increasing `lamport_seq`.
    OutOfOrder {
        previous: u64,
        got: u64,
    },
    DuplicateSeq {
        lamport_seq: u64,
    },
    /// Snapshots must have strictly increasing epoch ids and start sequences.
    SnapshotOutOfOrder {
        previous: SnapshotEpoch,
        got: SnapshotEpoch,
    },
}

impl fmt::Display for ChronoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chrono timeline: {self:?}")
    }
}
impl std::error::Error for ChronoError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOrder {
    /// Reject the first write whose sequence does not exceed its predecessor.
    RequireOrdered,
    /// Stable-sort by sequence first and record an [`OrderingNote`].
    /// Duplicate sequences are still rejected.
    SortWithReason(&'static str),
}

/// Why and how much input was reordered before indexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderingNote {
    pub reason: &'static str,
    /// Input positions whose event moved.
    pub moved: usize,
    pub total: usize,
}

#[derive(Debug, Default, Clone)]
pub struct ChronoTimelineIndex {
    snapshots: Vec<SnapshotEpoch>,
    writes_by_place: HashMap<u64, Vec<InOutWriteEvent>>,
    last_write_seq: Option<u64>,
    notes: Vec<OrderingNote>,
    write_count: usize,
}

impl ChronoTimelineIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from a batch, rejecting or sorting per `order`.
    pub fn from_writes(
        mut writes: Vec<InOutWriteEvent>,
        order: IngestOrder,
    ) -> Result<Self, ChronoError> {
        let mut index = Self::new();
        if let IngestOrder::SortWithReason(reason) = order {
            let before: Vec<u64> = writes.iter().map(|w| w.lamport_seq).collect();
            writes.sort_by_key(|w| w.lamport_seq);
            let moved = before
                .iter()
                .zip(&writes)
                .filter(|(seq, w)| **seq != w.lamport_seq)
                .count();
            if moved > 0 {
                index.notes.push(OrderingNote {
                    reason,
                    moved,
                    total: writes.len(),
                });
            }
        }
        for write in writes {
            index.record_inout_write(write)?;
        }
        Ok(index)
    }

    /// Record one write. Sequences must be strictly increasing across all places.
    pub fn record_inout_write(&mut self, ev: InOutWriteEvent) -> Result<(), ChronoError> {
        if let Some(previous) = self.last_write_seq {
            if ev.lamport_seq == previous {
                return Err(ChronoError::DuplicateSeq {
                    lamport_seq: previous,
                });
            }
            if ev.lamport_seq < previous {
                return Err(ChronoError::OutOfOrder {
                    previous,
                    got: ev.lamport_seq,
                });
            }
        }
        self.writes_by_place
            .entry(ev.place_hash)
            .or_default()
            .push(ev);
        self.last_write_seq = Some(ev.lamport_seq);
        self.write_count += 1;
        Ok(())
    }

    pub fn record_snapshot(&mut self, epoch: SnapshotEpoch) -> Result<(), ChronoError> {
        if let Some(previous) = self.snapshots.last()
            && (epoch.epoch_id <= previous.epoch_id
                || epoch.start_lamport_seq <= previous.start_lamport_seq)
        {
            return Err(ChronoError::SnapshotOutOfOrder {
                previous: *previous,
                got: epoch,
            });
        }
        self.snapshots.push(epoch);
        Ok(())
    }

    pub fn snapshots(&self) -> &[SnapshotEpoch] {
        &self.snapshots
    }
    pub fn ordering_notes(&self) -> &[OrderingNote] {
        &self.notes
    }
    pub fn write_count(&self) -> usize {
        self.write_count
    }
    pub fn writes_to(&self, place_hash: u64) -> &[InOutWriteEvent] {
        self.writes_by_place
            .get(&place_hash)
            .map_or(&[], Vec::as_slice)
    }

    /// Latest write to `place_hash` strictly before `before_seq`, paired with
    /// the nearest snapshot at or before it (to bound forward replay).
    pub fn find_last_writer_before(
        &self,
        place_hash: u64,
        before_seq: u64,
    ) -> Option<(Option<SnapshotEpoch>, InOutWriteEvent)> {
        self.find_last_writer_before_probed(place_hash, before_seq)
            .0
    }

    /// As [`Self::find_last_writer_before`], also returning how many
    /// sequence comparisons the two binary searches made (for complexity tests).
    pub fn find_last_writer_before_probed(
        &self,
        place_hash: u64,
        before_seq: u64,
    ) -> (Option<(Option<SnapshotEpoch>, InOutWriteEvent)>, u32) {
        let mut probes = 0u32;
        let Some(writes) = self.writes_by_place.get(&place_hash) else {
            return (None, probes);
        };
        let idx = writes.partition_point(|w| {
            probes += 1;
            w.lamport_seq < before_seq
        });
        if idx == 0 {
            return (None, probes);
        }
        let target = writes[idx - 1];
        let snapshot = self.nearest_snapshot_probed(target.lamport_seq, &mut probes);
        (Some((snapshot, target)), probes)
    }

    /// The last snapshot whose start is at or before `seq`.
    pub fn nearest_snapshot(&self, seq: u64) -> Option<SnapshotEpoch> {
        self.nearest_snapshot_probed(seq, &mut 0)
    }

    fn nearest_snapshot_probed(&self, seq: u64, probes: &mut u32) -> Option<SnapshotEpoch> {
        let idx = self.snapshots.partition_point(|s| {
            *probes += 1;
            s.start_lamport_seq <= seq
        });
        (idx > 0).then(|| self.snapshots[idx - 1])
    }
}
