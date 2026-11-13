//! `ReplaySession` (§31.2 `linear effect ChronoReplay`): reverse watchpoints,
//! lineage queries and step-back by replay from the nearest snapshot.
//!
//! `ReplaySession` is a non-Clone `#[must_use]` owner whose only exit is
//! [`ReplaySession::close`]. Dropping it unclosed increments
//! [`leaked_sessions`] and panics in debug builds (unless already panicking).
//! This is host-library discipline, not the Vesper linear typing rule.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::bundle::{Bundle, BundleError, BundleKey, BundleLimits};
use crate::lineage::{
    HopKind, LineageError, ObligationHop, ObligationId, ObligationLineage, Provenance,
};
use crate::ring::FlightEvent;
use crate::timeline::{ChronoError, ChronoTimelineIndex, InOutWriteEvent, SnapshotEpoch};
use crate::wire::{
    ChronoLayouts, EFFECT_INOUT_WRITE, EFFECT_OBLIGATION, InoutWriteRecord, OBLIGATION_HOP_BYTES,
    ObligationHopWire, ObligationRecord, TraceCursor,
};

static LEAKED_SESSIONS: AtomicU64 = AtomicU64::new(0);

/// Replay sessions dropped without [`ReplaySession::close`] in this process.
pub fn leaked_sessions() -> u64 {
    LEAKED_SESSIONS.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    Bundle(BundleError),
    Timeline(ChronoError),
    Lineage(LineageError),
    /// No write to this place strictly before the cursor.
    NoEarlierWrite {
        place_hash: u64,
        before_seq: u64,
    },
    /// The cursor cannot move before the first retained record.
    AtStart,
    SeqOutOfRange {
        lamport_seq: u64,
    },
    /// Replay to this point needs records that were overwritten in the ring.
    HistoryEvicted {
        needed_from: u64,
        first_retained: u64,
    },
    /// The step function rejected a record.
    Step {
        lamport_seq: u64,
        message: String,
    },
    /// The step function cannot replay a redacted or digest-only record.
    NotReplayable {
        lamport_seq: u64,
    },
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chrono replay: {self:?}")
    }
}
impl std::error::Error for ReplayError {}

impl From<BundleError> for ReplayError {
    fn from(e: BundleError) -> Self {
        Self::Bundle(e)
    }
}
impl From<ChronoError> for ReplayError {
    fn from(e: ChronoError) -> Self {
        Self::Timeline(e)
    }
}
impl From<LineageError> for ReplayError {
    fn from(e: LineageError) -> Self {
        Self::Lineage(e)
    }
}

/// A deterministic step function over recorded events.
pub trait Replayer {
    type State;
    /// State before the first record (sequence 1).
    fn initial(&self) -> Self::State;
    /// Decode a snapshot image.
    fn restore(&self, image: &[u8]) -> Result<Self::State, String>;
    /// Whether `apply` reads records of this effect. Replay refuses digest
    /// and redacted records of modelled effects; the default models all.
    fn models(&self, effect_id: u16) -> bool {
        let _ = effect_id;
        true
    }
    /// Apply one record. Records of effects the replayer does not model must
    /// be ignored, not rejected.
    fn apply(&self, state: &mut Self::State, event: &FlightEvent<'_>) -> Result<(), String>;
}

#[must_use = "a ReplaySession must be closed with close()"]
pub struct ReplaySession {
    bundle: Bundle,
    timeline: ChronoTimelineIndex,
    lineage: ObligationLineage,
    cursor: TraceCursor,
    closed: bool,
}

impl fmt::Debug for ReplaySession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplaySession")
            .field("records", &self.bundle.records.len())
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl ReplaySession {
    /// `ChronoReplay::open_bundle`: authenticate and index a bundle. The cursor
    /// starts at the end of the recording (`next_seq`).
    pub fn open_bundle(
        bytes: &[u8],
        key: &BundleKey,
        limits: &BundleLimits,
    ) -> Result<Self, ReplayError> {
        let bundle = Bundle::open(bytes, key, limits)?;
        Self::from_bundle(bundle)
    }

    pub fn from_bundle(bundle: Bundle) -> Result<Self, ReplayError> {
        let layouts = ChronoLayouts::new();
        let mut timeline = ChronoTimelineIndex::new();
        let mut lineage = if bundle.evicted_records > 0 {
            ObligationLineage::for_truncated_log()
        } else {
            ObligationLineage::new()
        };
        for (offset, snapshot) in bundle.snapshots.iter().enumerate() {
            timeline.record_snapshot(SnapshotEpoch {
                epoch_id: snapshot.epoch_id,
                start_lamport_seq: snapshot.start_lamport_seq,
                arena_cow_offset: offset as u64,
            })?;
        }
        for record in &bundle.records {
            match record.effect_id {
                EFFECT_INOUT_WRITE => {
                    let write = InoutWriteRecord::decode(&layouts, &record.payload)
                        .map_err(|_| BundleError::Malformed("inout write payload"))?;
                    timeline.record_inout_write(InOutWriteEvent {
                        lamport_seq: record.lamport_seq,
                        fiber_id: record.fiber_id,
                        place_hash: write.place_hash,
                        span_id: write.source_span_id,
                    })?;
                }
                EFFECT_OBLIGATION => {
                    let decoded = ObligationRecord::decode(&layouts, &record.payload)
                        .map_err(|_| BundleError::Malformed("obligation payload"))?;
                    let kind = HopKind::from_record(&decoded)?;
                    lineage.record(
                        decoded.obligation_id,
                        ObligationHop {
                            lamport_seq: record.lamport_seq,
                            fiber_id: record.fiber_id,
                            source_span_id: decoded.source_span_id,
                            kind,
                        },
                    )?;
                }
                _ => {}
            }
        }
        let mut session = Self {
            bundle,
            timeline,
            lineage,
            cursor: TraceCursor {
                epoch_id: 0,
                lamport_seq: 0,
            },
            closed: false,
        };
        session.cursor = session.cursor_at(session.bundle.next_seq);
        Ok(session)
    }

    fn cursor_at(&self, lamport_seq: u64) -> TraceCursor {
        TraceCursor {
            epoch_id: self
                .timeline
                .nearest_snapshot(lamport_seq)
                .map_or(0, |s| s.epoch_id),
            lamport_seq,
        }
    }

    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }
    pub fn timeline(&self) -> &ChronoTimelineIndex {
        &self.timeline
    }
    pub fn lineage(&self) -> &ObligationLineage {
        &self.lineage
    }
    pub fn cursor(&self) -> TraceCursor {
        self.cursor
    }

    /// Move the cursor to a retained record (or to `next_seq`, the end).
    pub fn seek(&mut self, lamport_seq: u64) -> Result<TraceCursor, ReplayError> {
        if lamport_seq < self.bundle.first_seq || lamport_seq > self.bundle.next_seq {
            return Err(ReplayError::SeqOutOfRange { lamport_seq });
        }
        self.cursor = self.cursor_at(lamport_seq);
        Ok(self.cursor)
    }

    /// `ChronoReplay::reverse_watch_inout`: move the cursor back to the last
    /// write to `place_hash` strictly before it. O(log N) in that place's writes.
    pub fn reverse_watch_inout(&mut self, place_hash: u64) -> Result<TraceCursor, ReplayError> {
        let before_seq = self.cursor.lamport_seq;
        let (_, write) = self
            .timeline
            .find_last_writer_before(place_hash, before_seq)
            .ok_or(ReplayError::NoEarlierWrite {
                place_hash,
                before_seq,
            })?;
        self.cursor = self.cursor_at(write.lamport_seq);
        Ok(self.cursor)
    }

    /// `ChronoReplay::query_linear_lineage`: this obligation's own hops.
    pub fn query_linear_lineage(
        &self,
        obligation_id: ObligationId,
    ) -> Result<&[ObligationHop], ReplayError> {
        Ok(self.lineage.chain(obligation_id)?)
    }

    /// Full provenance including ancestors.
    pub fn provenance(&self, obligation_id: ObligationId) -> Result<Provenance, ReplayError> {
        Ok(self.lineage.provenance(obligation_id)?)
    }

    /// The lineage in the design's `ObligationHop` wire form, written into a
    /// caller-provided scratch buffer (the `RegionSlice` of §31.2). Returns the
    /// number of hops written; a short buffer is an error, not a truncation.
    pub fn query_linear_lineage_into(
        &self,
        obligation_id: ObligationId,
        scratch: &mut [u8],
    ) -> Result<usize, ReplayError> {
        let chain = self.lineage.chain(obligation_id)?;
        let needed = chain
            .len()
            .checked_mul(OBLIGATION_HOP_BYTES)
            .ok_or(ReplayError::Bundle(BundleError::LimitExceeded("scratch")))?;
        if scratch.len() < needed {
            return Err(ReplayError::Bundle(BundleError::LimitExceeded("scratch")));
        }
        let layouts = ChronoLayouts::new();
        for (hop, out) in chain
            .iter()
            .zip(scratch.as_chunks_mut::<OBLIGATION_HOP_BYTES>().0)
        {
            ObligationHopWire {
                lamport_seq: hop.lamport_seq,
                fiber_id: hop.fiber_id,
                source_span_id: hop.source_span_id,
                hop_kind_tag: hop.kind.tag(),
            }
            .encode(&layouts, &mut out[..])
            .map_err(|_| ReplayError::Bundle(BundleError::Malformed("hop encode")))?;
        }
        Ok(chain.len())
    }

    /// State after applying every record up to and including `lamport_seq`,
    /// replayed forward from the nearest snapshot at or before it.
    pub fn state_at<R: Replayer>(
        &self,
        replayer: &R,
        lamport_seq: u64,
    ) -> Result<R::State, ReplayError> {
        self.state_at_from(replayer, lamport_seq, true)
    }

    /// As [`Self::state_at`], optionally ignoring snapshots (full replay from
    /// the initial state; only possible when nothing was evicted).
    pub fn state_at_from<R: Replayer>(
        &self,
        replayer: &R,
        lamport_seq: u64,
        use_snapshots: bool,
    ) -> Result<R::State, ReplayError> {
        if lamport_seq >= self.bundle.next_seq {
            return Err(ReplayError::SeqOutOfRange { lamport_seq });
        }
        let snapshot = if use_snapshots {
            self.timeline.nearest_snapshot(lamport_seq)
        } else {
            None
        };
        let (mut state, from) = match snapshot {
            Some(epoch) => {
                let image = &self.bundle.snapshots[epoch.arena_cow_offset as usize];
                if self.bundle.redactions.iter().any(|e| {
                    e.target
                        == crate::redact::RedactionTarget::Snapshot {
                            epoch_id: epoch.epoch_id,
                        }
                }) {
                    return Err(ReplayError::NotReplayable {
                        lamport_seq: epoch.start_lamport_seq,
                    });
                }
                let state =
                    replayer
                        .restore(&image.image)
                        .map_err(|message| ReplayError::Step {
                            lamport_seq: epoch.start_lamport_seq,
                            message,
                        })?;
                // The record at `start_lamport_seq` is the snapshot marker,
                // whose state the image already holds; replay resumes after it,
                // so an evicted marker does not make the snapshot unusable.
                (state, epoch.start_lamport_seq + 1)
            }
            None => (replayer.initial(), 1),
        };
        if from < self.bundle.first_seq {
            return Err(ReplayError::HistoryEvicted {
                needed_from: from,
                first_retained: self.bundle.first_seq,
            });
        }
        if from > lamport_seq {
            return Ok(state);
        }
        for seq in from..=lamport_seq {
            let record = self
                .bundle
                .record(seq)
                .ok_or(ReplayError::SeqOutOfRange { lamport_seq: seq })?;
            // A redacted or digest-only outcome cannot be fed to a step
            // function that models its effect: the original bytes are gone.
            if replayer.models(record.effect_id) && (record.digest || self.bundle.is_redacted(seq))
            {
                return Err(ReplayError::NotReplayable { lamport_seq: seq });
            }
            replayer
                .apply(&mut state, &record.as_event())
                .map_err(|message| ReplayError::Step {
                    lamport_seq: seq,
                    message,
                })?;
        }
        Ok(state)
    }

    /// Move the cursor back one record and reconstruct the state there (after
    /// applying the record at the new cursor).
    pub fn step_back<R: Replayer>(
        &mut self,
        replayer: &R,
    ) -> Result<(TraceCursor, R::State), ReplayError> {
        let target = self
            .cursor
            .lamport_seq
            .checked_sub(1)
            .filter(|seq| *seq >= self.bundle.first_seq)
            .ok_or(ReplayError::AtStart)?;
        let state = self.state_at(replayer, target)?;
        self.cursor = self.cursor_at(target);
        Ok((self.cursor, state))
    }

    /// `ChronoReplay::close_session`.
    pub fn close(mut self) -> Bundle {
        self.closed = true;
        std::mem::replace(
            &mut self.bundle,
            Bundle {
                metadata: Vec::new(),
                first_seq: 1,
                next_seq: 1,
                evicted_records: 0,
                records: Vec::new(),
                snapshots: Vec::new(),
                redactions: Vec::new(),
            },
        )
    }
}

impl Drop for ReplaySession {
    fn drop(&mut self) {
        if !self.closed {
            LEAKED_SESSIONS.fetch_add(1, Ordering::Relaxed);
            if cfg!(debug_assertions) && !std::thread::panicking() {
                panic!("ReplaySession dropped without close()");
            }
        }
    }
}
