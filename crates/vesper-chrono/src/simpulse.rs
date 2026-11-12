//! Recording a SimPulse session into a flight recorder and rebuilding its
//! journal from a bundle.
//!
//! The scheduler's own transitions are recorded as [`EFFECT_SCHEDULER`]
//! records (one SimPulse `TraceEvent` per record, as JSON). They are seeded
//! and deterministic; recording them lets replay compare the event stream and
//! lets `Simulator::replay` check every transition. The program's external
//! nondeterminism (I/O outcomes) is recorded by the embedder as ordinary
//! effect records. This adapter allocates (JSON) and is not part of the
//! recorder's zero-allocation steady state.

use vesper_simpulse::{Journal, Simulator, TraceEvent};

use crate::bundle::Bundle;
use crate::ring::{FlightRecorder, RecordError};
use crate::wire::EFFECT_SCHEDULER;

/// Copies newly committed SimPulse journal events into a recorder.
#[derive(Debug, Default, Clone, Copy)]
pub struct SimPulseTap {
    copied: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimPulseError {
    Record(RecordError),
    Encode(String),
    /// A scheduler record could not be decoded.
    Decode {
        lamport_seq: u64,
        message: String,
    },
    /// The bundle does not hold the whole run (records were evicted).
    Incomplete {
        evicted_records: u64,
    },
    BadMetadata,
}

impl std::fmt::Display for SimPulseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "chrono simpulse: {self:?}")
    }
}
impl std::error::Error for SimPulseError {}

impl SimPulseTap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record every journal event committed since the last drain. Each is
    /// stamped with the simulator's current virtual time.
    pub fn drain<T>(
        &mut self,
        sim: &Simulator<T>,
        recorder: &mut FlightRecorder,
    ) -> Result<usize, SimPulseError> {
        let events = &sim.journal().events;
        // In replay mode the journal is the whole recording; only the prefix
        // the simulator has consumed is committed. SimPulse exposes that count
        // only through `finish_replay`'s remaining-events error.
        let committed = match sim.finish_replay() {
            Ok(()) => events.len(),
            Err(vesper_simpulse::Error::ReplayUnconsumed { remaining }) => events.len() - remaining,
            Err(other) => return Err(SimPulseError::Encode(other.to_string())),
        };
        let fresh = &events[self.copied.min(committed)..committed];
        for event in fresh {
            let payload =
                serde_json::to_vec(event).map_err(|e| SimPulseError::Encode(e.to_string()))?;
            recorder
                .record_effect_internal(task_of(event), sim.now_ns(), EFFECT_SCHEDULER, &payload)
                .map_err(SimPulseError::Record)?;
        }
        let n = fresh.len();
        self.copied = committed;
        Ok(n)
    }
}

/// The fiber a scheduler event belongs to (0 for clock advances).
fn task_of(event: &TraceEvent) -> u64 {
    match event {
        TraceEvent::Spawn { task, .. }
        | TraceEvent::Dispatch { task, .. }
        | TraceEvent::Yield { task, .. }
        | TraceEvent::Sleep { task, .. }
        | TraceEvent::SubmitIo { task, .. }
        | TraceEvent::TimerElapsed { task, .. }
        | TraceEvent::IoCompleted { task, .. }
        | TraceEvent::CancelRequested { task, .. }
        | TraceEvent::Finish { task, .. } => task.get(),
        TraceEvent::ClockAdvance { .. } => 0,
    }
}

/// Seed metadata: `b"simpulse-seed:"` followed by the seed as `u64le`.
pub fn seed_metadata(seed: u64) -> Vec<u8> {
    let mut out = b"simpulse-seed:".to_vec();
    out.extend_from_slice(&seed.to_le_bytes());
    out
}

pub fn seed_from_metadata(metadata: &[u8]) -> Result<u64, SimPulseError> {
    let rest = metadata
        .strip_prefix(b"simpulse-seed:")
        .ok_or(SimPulseError::BadMetadata)?;
    let bytes: [u8; 8] = rest.try_into().map_err(|_| SimPulseError::BadMetadata)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Rebuild the SimPulse journal of a complete recording.
pub fn journal_from_bundle(bundle: &Bundle) -> Result<Journal, SimPulseError> {
    if bundle.evicted_records != 0 {
        return Err(SimPulseError::Incomplete {
            evicted_records: bundle.evicted_records,
        });
    }
    let seed = seed_from_metadata(&bundle.metadata)?;
    let mut events = Vec::new();
    for record in bundle
        .records
        .iter()
        .filter(|r| r.effect_id == EFFECT_SCHEDULER)
    {
        let event: TraceEvent =
            serde_json::from_slice(&record.payload).map_err(|e| SimPulseError::Decode {
                lamport_seq: record.lamport_seq,
                message: e.to_string(),
            })?;
        events.push(event);
    }
    // Build through the versioned JSON form so the journal version is the one
    // SimPulse itself accepts.
    let probe = Simulator::<()>::new(seed);
    let mut journal = probe.journal().clone();
    journal.events = events;
    Ok(journal)
}
