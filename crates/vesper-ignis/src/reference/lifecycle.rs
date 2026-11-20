//! Rust build of the lifecycle fence ledger: limit checks, translation of a
//! plan into a fenced submission, reservations, and completion validation.
//! The shared `Runtime` in `crate::lifecycle` owns the payloads.

use crate::lifecycle::{
    BatchView, FencePoint, FrameTicket, LifecycleError, Limits, MAX_FENCE_VALUE, QueueCommand,
    QueueConfig, Submission, SubmissionStream, TimelineId,
};
use crate::{Command, PassId, Plan, QueueKind};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

// Ceilings on `Limits` (each limit but `max_scratch` and `max_drain_polls`
// must also be nonzero).
const MAX_QUEUES: usize = 64;
const MAX_INFLIGHT: usize = 4096;
const MAX_COMMANDS: usize = 1_000_000;
const MAX_SCRATCH: usize = 4096;
const MAX_POLL_EVENTS: usize = 65_536;
const MAX_DRAIN_POLLS: usize = 65_536;

fn fence_add(base: u64, increment: u64) -> Result<u64, LifecycleError> {
    base.checked_add(increment)
        .filter(|value| *value <= MAX_FENCE_VALUE)
        .ok_or(LifecycleError::FenceOverflow)
}

fn validate_limits(limits: Limits) -> Result<(), LifecycleError> {
    if limits.max_queues == 0
        || limits.max_queues > MAX_QUEUES
        || limits.max_inflight == 0
        || limits.max_inflight > MAX_INFLIGHT
        || limits.max_commands == 0
        || limits.max_commands > MAX_COMMANDS
        || limits.max_scratch > MAX_SCRATCH
        || limits.max_poll_events == 0
        || limits.max_poll_events > MAX_POLL_EVENTS
        || limits.max_drain_polls > MAX_DRAIN_POLLS
    {
        return Err(LifecycleError::Limit("invalid lifecycle limits"));
    }
    Ok(())
}

/// Checks a scratch lease for `pass` against the plan and the per-frame bound.
pub(crate) fn scratch_check(
    plan: &Plan,
    pass: PassId,
    leases: usize,
    max_scratch: usize,
) -> Result<(), LifecycleError> {
    let stream = plan
        .queues
        .iter()
        .find(|q| q.commands.contains(&Command::Execute(pass)))
        .ok_or(LifecycleError::ForeignPass)?;
    if stream.kind == QueueKind::Copy {
        return Err(LifecycleError::ScratchOnCopyQueue);
    }
    if leases >= max_scratch {
        return Err(LifecycleError::Limit("scratch per frame"));
    }
    Ok(())
}

/// How the backend answered the submission most recently begun.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Settle {
    Rejected,
    Accepted,
}

struct Timeline {
    kind: QueueKind,
    reserved: u64,
    observed: u64,
}

/// The waits and signals of one in-flight stream, in order.
struct FlightStream {
    timeline: TimelineId,
    points: Vec<(bool, FencePoint)>,
}
struct Flight {
    ticket: FrameTicket,
    uncertain: bool,
    streams: Vec<FlightStream>,
    terminal: Vec<FencePoint>,
}

pub(crate) struct Ledger {
    id: u64,
    limits: Limits,
    queues: Vec<Timeline>,
    next_frame: u64,
    inflight: VecDeque<Flight>,
}

impl Ledger {
    pub(crate) fn check(queues: &[QueueConfig], limits: Limits) -> Result<(), LifecycleError> {
        validate_limits(limits)?;
        if queues.is_empty() || queues.len() > limits.max_queues {
            return Err(LifecycleError::Limit("queues"));
        }
        if queues.iter().any(|q| q.initial_fence > MAX_FENCE_VALUE) {
            return Err(LifecycleError::FenceOverflow);
        }
        Ok(())
    }
    pub(crate) fn new(id: u64, queues: &[QueueConfig], limits: Limits) -> Self {
        Self {
            id,
            limits,
            queues: queues
                .iter()
                .map(|q| Timeline {
                    kind: q.kind,
                    reserved: q.initial_fence,
                    observed: q.initial_fence,
                })
                .collect(),
            next_frame: 1,
            inflight: VecDeque::new(),
        }
    }
    pub(crate) fn id(&self) -> u64 {
        self.id
    }
    pub(crate) fn queue_count(&self) -> usize {
        self.queues.len()
    }
    pub(crate) fn max_scratch(&self) -> usize {
        self.limits.max_scratch
    }
    pub(crate) fn poll_allowed(&self, max_events: usize) -> bool {
        max_events != 0 && max_events <= self.limits.max_poll_events
    }
    pub(crate) fn drain_allowed(&self, polls: usize) -> bool {
        polls <= self.limits.max_drain_polls
    }
    fn queue(&self, timeline: TimelineId) -> Result<&Timeline, LifecycleError> {
        if timeline.runtime != self.id {
            return Err(LifecycleError::ForeignTimeline);
        }
        self.queues
            .get(timeline.index)
            .ok_or(LifecycleError::ForeignTimeline)
    }
    pub(crate) fn observed(&self, timeline: TimelineId) -> Result<u64, LifecycleError> {
        Ok(self.queue(timeline)?.observed)
    }
    pub(crate) fn validate(&self, batch: &BatchView<'_>) -> Result<(), LifecycleError> {
        let plan = batch.plan;
        if plan.queues.is_empty() || plan.queues.len() > self.limits.max_queues {
            return Err(LifecycleError::Limit("plan queues"));
        }
        if plan.queues.len() != batch.bindings.len()
            || batch.bindings.len() != batch.allocators.len()
        {
            return Err(LifecycleError::QueueBinding);
        }
        let mut graph_queues = BTreeSet::new();
        let mut timelines = BTreeSet::new();
        let mut count = plan.queues.len(); // terminal signals
        for (binding, allocator) in batch.bindings.iter().zip(&batch.allocators) {
            if !graph_queues.insert(binding.queue) || !timelines.insert(binding.timeline) {
                return Err(LifecycleError::QueueBinding);
            }
            let queue = self.queue(binding.timeline)?;
            let stream = plan
                .queues
                .iter()
                .find(|q| q.queue == binding.queue)
                .ok_or(LifecycleError::QueueBinding)?;
            if stream.kind != queue.kind {
                return Err(LifecycleError::QueueKindMismatch);
            }
            if *allocator != binding.timeline {
                return Err(LifecycleError::AllocatorBinding);
            }
            count = count
                .checked_add(stream.commands.len())
                .ok_or(LifecycleError::Limit("commands"))?;
            for command in &stream.commands {
                if let Command::Barriers(barriers) = command {
                    count = count
                        .checked_add(barriers.len())
                        .ok_or(LifecycleError::Limit("barriers"))?;
                }
            }
        }
        if count > self.limits.max_commands {
            return Err(LifecycleError::Limit("commands"));
        }
        Ok(())
    }
    fn translate(
        &self,
        batch: &BatchView<'_>,
        ticket: FrameTicket,
    ) -> Result<Submission, LifecycleError> {
        if batch.runtime != self.id {
            return Err(LifecycleError::ForeignTimeline);
        }
        self.validate(batch)?;
        let plan = batch.plan;
        let mut work = plan
            .queues
            .len()
            .checked_add(batch.scratch.len())
            .ok_or(LifecycleError::Limit("commands"))?;
        for stream in &plan.queues {
            work = work
                .checked_add(stream.commands.len())
                .ok_or(LifecycleError::Limit("commands"))?;
            for command in &stream.commands {
                if let Command::Barriers(barriers) = command {
                    work = work
                        .checked_add(barriers.len())
                        .ok_or(LifecycleError::Limit("barriers"))?;
                }
            }
        }
        if work > self.limits.max_commands {
            return Err(LifecycleError::Limit("commands"));
        }
        let mut map = BTreeMap::new();
        for binding in batch.bindings {
            map.insert(
                binding.queue,
                (binding.timeline, self.queue(binding.timeline)?.reserved),
            );
        }
        let mut streams = Vec::with_capacity(plan.queues.len());
        let mut terminal = Vec::with_capacity(plan.queues.len());
        let mut total: usize = 0;
        for stream in &plan.queues {
            let (timeline, base) = map[&stream.queue];
            let mut commands = Vec::new();
            let mut max = base;
            for command in &stream.commands {
                match command {
                    Command::Wait { queue, value } => {
                        let (other, other_base) = map[queue];
                        let value = fence_add(other_base, *value)?;
                        commands.push(QueueCommand::Wait(FencePoint {
                            timeline: other,
                            value,
                        }));
                    }
                    Command::Signal { value } => {
                        let value = fence_add(base, *value)?;
                        if value <= max {
                            return Err(LifecycleError::FenceOverflow);
                        }
                        max = value;
                        commands.push(QueueCommand::Signal(FencePoint { timeline, value }));
                    }
                    Command::Barriers(barriers) => {
                        total = total
                            .checked_add(barriers.len())
                            .ok_or(LifecycleError::Limit("barriers"))?;
                        commands.push(QueueCommand::Barriers(barriers.clone()));
                    }
                    Command::Execute(pass) => {
                        for (index, (_, action, program)) in batch
                            .scratch
                            .iter()
                            .enumerate()
                            .filter(|(_, s)| s.0 == *pass)
                        {
                            commands.push(QueueCommand::Scratch {
                                index,
                                action: *action,
                                program: *program,
                            });
                        }
                        commands.push(QueueCommand::Execute(*pass));
                    }
                }
            }
            let last = FencePoint {
                timeline,
                value: fence_add(max, 1)?,
            };
            commands.push(QueueCommand::Signal(last));
            total = total
                .checked_add(commands.len())
                .ok_or(LifecycleError::Limit("commands"))?;
            if total > self.limits.max_commands {
                return Err(LifecycleError::Limit("commands"));
            }
            terminal.push(last);
            streams.push(SubmissionStream {
                timeline,
                kind: stream.kind,
                commands,
            });
        }
        Ok(Submission {
            ticket,
            streams,
            terminal,
        })
    }
    /// Preflight, translate and reserve: the flight stays uncertain until
    /// [`Ledger::settle`].
    pub(crate) fn begin(&mut self, batch: &BatchView<'_>) -> Result<Submission, LifecycleError> {
        if self.inflight.len() >= self.limits.max_inflight {
            return Err(LifecycleError::Limit("inflight frames"));
        }
        if self.inflight.iter().any(|f| f.uncertain) {
            return Err(LifecycleError::SubmissionUncertain);
        }
        if self.next_frame == u64::MAX {
            return Err(LifecycleError::FrameIdsExhausted);
        }
        let ticket = FrameTicket {
            runtime: self.id,
            sequence: self.next_frame,
        };
        let submission = self.translate(batch, ticket)?;
        for point in &submission.terminal {
            self.queues[point.timeline.index].reserved = point.value;
        }
        self.next_frame += 1;
        self.inflight.push_back(Flight {
            ticket,
            uncertain: true,
            streams: submission
                .streams
                .iter()
                .map(|s| FlightStream {
                    timeline: s.timeline,
                    points: s
                        .commands
                        .iter()
                        .filter_map(|c| match c {
                            QueueCommand::Wait(p) => Some((false, *p)),
                            QueueCommand::Signal(p) => Some((true, *p)),
                            _ => None,
                        })
                        .collect(),
                })
                .collect(),
            terminal: submission.terminal.clone(),
        });
        Ok(submission)
    }
    pub(crate) fn settle(&mut self, outcome: Settle) {
        match outcome {
            Settle::Rejected => {
                self.inflight.pop_back();
            }
            Settle::Accepted => {
                if let Some(flight) = self.inflight.back_mut() {
                    flight.uncertain = false;
                }
            }
        }
    }
    pub(crate) fn check_completion(&self, point: FencePoint) -> Result<(), LifecycleError> {
        let queue = self.queue(point.timeline)?;
        if point.value <= queue.observed {
            return Err(LifecycleError::StaleCompletion);
        }
        if point.value > queue.reserved {
            return Err(LifecycleError::UnknownCompletion);
        }
        for flight in &self.inflight {
            for stream in flight
                .streams
                .iter()
                .filter(|s| s.timeline == point.timeline)
            {
                for (signal, other) in &stream.points {
                    if *signal {
                        if *other == point {
                            return Ok(());
                        }
                    } else if self.queue(other.timeline)?.observed < other.value {
                        return Err(LifecycleError::WaitNotObserved(*other));
                    }
                }
            }
        }
        Err(LifecycleError::UnknownCompletion)
    }
    /// Records a validated completion and returns the tickets it retires,
    /// oldest first.
    pub(crate) fn complete(&mut self, point: FencePoint) -> Vec<FrameTicket> {
        self.queues[point.timeline.index].observed = point.value;
        let queues = &self.queues;
        let mut retired = Vec::new();
        self.inflight.retain(|flight| {
            let complete = flight
                .terminal
                .iter()
                .all(|p| queues[p.timeline.index].observed >= p.value);
            if complete {
                retired.push(flight.ticket);
            }
            !complete
        });
        retired
    }
}
