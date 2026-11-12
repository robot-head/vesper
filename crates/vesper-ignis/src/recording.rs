//! Deterministic CPU recording backend for lifecycle tests/embedding examples.
//!
//! Executes no shaders and calls no GPU APIs. Waits/signals and queue ordering
//! are simulated; its completion reports establish only this host model.
use crate::lifecycle::{
    Backend, BackendSubmit, Completion, FencePoint, OwnerViews, PollBatch, QueueCommand,
    Submission, TimelineId,
};
use std::collections::{BTreeMap, VecDeque};
use std::marker::PhantomData;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitMode {
    Accept,
    Reject,
    /// Work is queued, but the caller cannot determine acceptance yet.
    AcceptUncertain,
    /// Only the first queue stream is published; remaining streams and their
    /// terminal signals never execute. Owners must remain in incomplete shutdown.
    AcceptFirstQueueUncertain,
    /// Publication may have happened, and no completion can be established by
    /// this model. Shutdown must return the pending owners indefinitely.
    StallUncertain,
    /// Simulate backend unwinding after it retained submitted work.
    PanicAfterAcceptance,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingError {
    Rejected,
    Uncertain,
    HistoryLimit,
    PendingLimit,
    PollFailure,
}
struct Active {
    submission: Submission,
    cursors: Vec<usize>,
}

type OwnersMarker<A, R, T> = PhantomData<fn() -> (A, R, T)>;

pub struct RecordingBackend<A, R, T> {
    history: Vec<Submission>,
    active: Vec<Active>,
    signals: BTreeMap<TimelineId, u64>,
    modes: VecDeque<SubmitMode>,
    poll_errors: VecDeque<bool>,
    history_limit: usize,
    pending_limit: usize,
    work_per_poll: usize,
    owners: OwnersMarker<A, R, T>,
}
impl<A, R, T> Default for RecordingBackend<A, R, T> {
    fn default() -> Self {
        Self::new(256, 64, 16_384).expect("valid defaults")
    }
}
impl<A, R, T> RecordingBackend<A, R, T> {
    pub fn new(
        history_limit: usize,
        pending_limit: usize,
        work_per_poll: usize,
    ) -> Result<Self, &'static str> {
        if history_limit == 0
            || history_limit > 65_536
            || pending_limit == 0
            || pending_limit > 4096
            || work_per_poll > 1_000_000
        {
            return Err("recording backend bounds");
        }
        Ok(Self {
            history: Vec::new(),
            active: Vec::new(),
            signals: BTreeMap::new(),
            modes: VecDeque::new(),
            poll_errors: VecDeque::new(),
            history_limit,
            pending_limit,
            work_per_poll,
            owners: PhantomData,
        })
    }
    pub fn push_submit_mode(&mut self, mode: SubmitMode) -> Result<(), &'static str> {
        if self.modes.len() >= self.history_limit {
            return Err("submit script bound");
        }
        self.modes.push_back(mode);
        Ok(())
    }
    /// Whether the next poll reports a failure after returning harvested events.
    pub fn push_poll_failure(&mut self, fail: bool) -> Result<(), &'static str> {
        if self.poll_errors.len() >= self.history_limit {
            return Err("poll script bound");
        }
        self.poll_errors.push_back(fail);
        Ok(())
    }
    pub fn history(&self) -> &[Submission] {
        &self.history
    }
    fn step(&mut self) -> Option<Option<FencePoint>> {
        for index in 0..self.active.len() {
            for stream_index in 0..self.active[index].submission.streams.len() {
                let stream = &self.active[index].submission.streams[stream_index];
                let cursor = self.active[index].cursors[stream_index];
                let Some(command) = stream.commands.get(cursor) else {
                    continue;
                };
                // Every earlier submission on the same physical timeline must
                // finish its stream first, independently of other timelines.
                let blocked = self.active[..index].iter().any(|earlier| {
                    earlier.submission.streams.iter().enumerate().any(|(s, q)| {
                        q.timeline == stream.timeline && earlier.cursors[s] < q.commands.len()
                    })
                });
                if blocked {
                    continue;
                }
                if let QueueCommand::Wait(point) = command
                    && self.signals.get(&point.timeline).copied().unwrap_or(0) < point.value
                {
                    continue;
                }
                let signal = if let QueueCommand::Signal(point) = command {
                    Some(*point)
                } else {
                    None
                };
                self.active[index].cursors[stream_index] += 1;
                if let Some(point) = signal {
                    self.signals.insert(point.timeline, point.value);
                }
                return Some(signal);
            }
        }
        None
    }
}
impl<A, R, T> Backend for RecordingBackend<A, R, T> {
    type Allocator = A;
    type Resources = R;
    type Scratch = T;
    type Error = RecordingError;
    fn submit(
        &mut self,
        submission: &Submission,
        _owners: OwnerViews<'_, A, R, T>,
    ) -> BackendSubmit<RecordingError> {
        if self.history.len() >= self.history_limit {
            return BackendSubmit::Rejected(RecordingError::HistoryLimit);
        }
        if self.active.len() >= self.pending_limit {
            return BackendSubmit::Rejected(RecordingError::PendingLimit);
        }
        let mode = self.modes.pop_front().unwrap_or(SubmitMode::Accept);
        if mode == SubmitMode::Reject {
            return BackendSubmit::Rejected(RecordingError::Rejected);
        }
        if mode == SubmitMode::StallUncertain {
            return BackendSubmit::Uncertain(RecordingError::Uncertain);
        }
        self.history.push(submission.clone());
        let accepted = if mode == SubmitMode::AcceptFirstQueueUncertain {
            Submission {
                ticket: submission.ticket,
                streams: submission.streams.iter().take(1).cloned().collect(),
                terminal: submission.terminal.iter().take(1).copied().collect(),
            }
        } else {
            submission.clone()
        };
        let count = accepted.streams.len();
        self.active.push(Active {
            submission: accepted,
            cursors: vec![0; count],
        });
        assert!(
            mode != SubmitMode::PanicAfterAcceptance,
            "recording backend fault after acceptance"
        );
        if matches!(
            mode,
            SubmitMode::AcceptUncertain | SubmitMode::AcceptFirstQueueUncertain
        ) {
            BackendSubmit::Uncertain(RecordingError::Uncertain)
        } else {
            BackendSubmit::Accepted
        }
    }
    fn poll(&mut self, max_events: usize) -> PollBatch<RecordingError> {
        let mut completions = Vec::new();
        for _ in 0..self.work_per_poll {
            if completions.len() >= max_events {
                break;
            }
            match self.step() {
                Some(Some(point)) => completions.push(Completion::reported(point)),
                Some(None) => {}
                None => break,
            }
        }
        self.active.retain(|f| {
            f.submission
                .streams
                .iter()
                .zip(&f.cursors)
                .any(|(s, c)| *c < s.commands.len())
        });
        let error = self
            .poll_errors
            .pop_front()
            .filter(|fail| *fail)
            .map(|_| RecordingError::PollFailure);
        PollBatch { completions, error }
    }
}
