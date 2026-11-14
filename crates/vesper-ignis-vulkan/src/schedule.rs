//! Splitting a lifecycle [`Submission`] into Vulkan queue batches and ordering
//! them so that no batch is submitted before the signal it waits for.
//!
//! Each queue stream is cut at every `Wait` that follows work and after every
//! run of `Signal`s, giving segments of the form *waits → body → signals*.
//! Each segment becomes one `vkQueueSubmit2` batch with timeline-semaphore
//! wait and signal infos. Several Ignis queues may share one `VkQueue` (Mesa
//! lavapipe exposes a single queue), and a batch that waits for a signal
//! submitted *later* to the same in-order queue would never run, so segments
//! are submitted in an order in which every wait's signal has already been
//! submitted. The planner's output is acyclic, so such an order exists for
//! valid plans; otherwise [`ScheduleError::Stalled`] is returned before any
//! work is submitted.

use std::collections::BTreeMap;
use vesper_ignis::lifecycle::{FencePoint, QueueCommand, Submission, TimelineId};

/// One `vkQueueSubmit2` batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Index into `Submission::streams`.
    pub stream: usize,
    pub waits: Vec<FencePoint>,
    /// Indices into the stream's commands: only `Barriers` and `Execute`.
    pub body: Vec<usize>,
    pub signals: Vec<FencePoint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    /// Work-graph scratch is a D3D12 work-graph concept with no Vulkan
    /// equivalent here.
    ScratchUnsupported { stream: usize, command: usize },
    /// A wait or signal names a timeline this backend does not own.
    UnknownTimeline(TimelineId),
    /// A signal does not exceed the timeline's last submitted value.
    NonMonotonicSignal(FencePoint),
    /// No remaining segment can be submitted without waiting for a signal that
    /// is never submitted first.
    Stalled { remaining: usize },
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScratchUnsupported { stream, command } => write!(
                f,
                "work-graph scratch (stream {stream}, command {command}) is not supported on Vulkan"
            ),
            Self::UnknownTimeline(t) => write!(f, "timeline {} is not owned here", t.index()),
            Self::NonMonotonicSignal(p) => write!(
                f,
                "signal {} on timeline {} does not advance it",
                p.value,
                p.timeline.index()
            ),
            Self::Stalled { remaining } => {
                write!(
                    f,
                    "{remaining} batches wait for signals never submitted first"
                )
            }
        }
    }
}
impl std::error::Error for ScheduleError {}

fn segments(submission: &Submission) -> Result<Vec<Vec<Segment>>, ScheduleError> {
    let mut out = Vec::with_capacity(submission.streams.len());
    for (s, stream) in submission.streams.iter().enumerate() {
        let mut list = Vec::new();
        let mut cur = Segment {
            stream: s,
            waits: Vec::new(),
            body: Vec::new(),
            signals: Vec::new(),
        };
        let flush = |cur: &mut Segment, list: &mut Vec<Segment>| {
            if !(cur.waits.is_empty() && cur.body.is_empty() && cur.signals.is_empty()) {
                list.push(std::mem::replace(
                    cur,
                    Segment {
                        stream: s,
                        waits: Vec::new(),
                        body: Vec::new(),
                        signals: Vec::new(),
                    },
                ));
            }
        };
        for (i, command) in stream.commands.iter().enumerate() {
            match command {
                QueueCommand::Wait(point) => {
                    if !cur.body.is_empty() || !cur.signals.is_empty() {
                        flush(&mut cur, &mut list);
                    }
                    cur.waits.push(*point);
                }
                QueueCommand::Barriers(_) | QueueCommand::Execute(_) => {
                    if !cur.signals.is_empty() {
                        flush(&mut cur, &mut list);
                    }
                    cur.body.push(i);
                }
                QueueCommand::Signal(point) => cur.signals.push(*point),
                QueueCommand::Scratch { .. } => {
                    return Err(ScheduleError::ScratchUnsupported {
                        stream: s,
                        command: i,
                    });
                }
            }
        }
        flush(&mut cur, &mut list);
        out.push(list);
    }
    Ok(out)
}

/// Orders the submission's segments for in-order submission.
///
/// `submitted` holds, for every timeline the backend owns, the highest value
/// already submitted for signalling (work from earlier frames); it is not
/// modified. Waits on values at or below it are satisfied.
pub fn schedule(
    submission: &Submission,
    submitted: &BTreeMap<TimelineId, u64>,
) -> Result<Vec<Segment>, ScheduleError> {
    let mut queues = segments(submission)?;
    let mut reached = submitted.clone();
    for stream in &submission.streams {
        if !reached.contains_key(&stream.timeline) {
            return Err(ScheduleError::UnknownTimeline(stream.timeline));
        }
    }
    let total: usize = queues.iter().map(Vec::len).sum();
    let mut cursors = vec![0usize; queues.len()];
    let mut order = Vec::with_capacity(total);
    while order.len() < total {
        let mut progressed = false;
        for s in 0..queues.len() {
            let Some(segment) = queues[s].get(cursors[s]) else {
                continue;
            };
            let mut ready = true;
            for wait in &segment.waits {
                match reached.get(&wait.timeline) {
                    None => return Err(ScheduleError::UnknownTimeline(wait.timeline)),
                    Some(value) if *value < wait.value => ready = false,
                    Some(_) => {}
                }
            }
            if !ready {
                continue;
            }
            for signal in &segment.signals {
                let value = reached
                    .get_mut(&signal.timeline)
                    .ok_or(ScheduleError::UnknownTimeline(signal.timeline))?;
                if signal.value <= *value {
                    return Err(ScheduleError::NonMonotonicSignal(*signal));
                }
                *value = signal.value;
            }
            let segment = std::mem::replace(
                &mut queues[s][cursors[s]],
                Segment {
                    stream: s,
                    waits: Vec::new(),
                    body: Vec::new(),
                    signals: Vec::new(),
                },
            );
            cursors[s] += 1;
            order.push(segment);
            progressed = true;
            // Restart from the first stream so lower streams go first.
            break;
        }
        if !progressed {
            return Err(ScheduleError::Stalled {
                remaining: total - order.len(),
            });
        }
    }
    Ok(order)
}
