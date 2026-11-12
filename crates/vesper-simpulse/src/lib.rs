#![doc = include_str!("../README.md")]
//! Deterministic scheduling metadata and owned continuation tokens for SimPulse.
//!
//! This kernel does not execute Vesper, emulate an OS, or explore DPOR schedules.
//! The embedder runs each returned token and explicitly yields, parks or finishes
//! it. Cancellation is a request flag; only actual completion releases parked
//! payloads. Journal replay validates transitions before ownership is moved.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

const JOURNAL_VERSION: u32 = 1;
static NEXT_SIMULATOR: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TaskId(u64);
impl TaskId {
    pub fn get(self) -> u64 {
        self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CompletionId(u64);
impl CompletionId {
    pub fn get(self) -> u64 {
        self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum IoResult {
    Success { bytes: u64 },
    Error { code: i32 },
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum WakeReason {
    Start,
    Yield,
    Timer(CompletionId),
    Io {
        completion: CompletionId,
        result: IoResult,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum TraceEvent {
    Spawn {
        task: TaskId,
        at_ns: u64,
    },
    Dispatch {
        task: TaskId,
        at_ns: u64,
        round: u64,
    },
    Yield {
        task: TaskId,
        at_ns: u64,
    },
    Sleep {
        task: TaskId,
        completion: CompletionId,
        deadline_ns: u64,
    },
    SubmitIo {
        task: TaskId,
        completion: CompletionId,
        scheduled: Option<(u64, IoResult)>,
    },
    ClockAdvance {
        from_ns: u64,
        to_ns: u64,
    },
    TimerElapsed {
        task: TaskId,
        completion: CompletionId,
        at_ns: u64,
    },
    IoCompleted {
        task: TaskId,
        completion: CompletionId,
        at_ns: u64,
        result: IoResult,
    },
    CancelRequested {
        task: TaskId,
        at_ns: u64,
    },
    Finish {
        task: TaskId,
        at_ns: u64,
    },
}
/// Version 1 fixes SplitMix64, rejection sampling, and sorted task-ID selection.
/// Payloads, pointers, and simulator instance IDs never appear in a journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub version: u32,
    pub seed: u64,
    pub events: Vec<TraceEvent>,
}
impl Journal {
    pub fn to_json(&self) -> Result<String, Error> {
        serde_json::to_string(self).map_err(|e| Error::InvalidJournal(e.to_string()))
    }
    pub fn from_json(text: &str) -> Result<Self, Error> {
        let journal: Self =
            serde_json::from_str(text).map_err(|e| Error::InvalidJournal(e.to_string()))?;
        journal.validate_version()?;
        Ok(journal)
    }
    fn validate_version(&self) -> Result<(), Error> {
        if self.version != JOURNAL_VERSION {
            return Err(Error::UnsupportedVersion(self.version));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    UnknownTask(TaskId),
    UnknownCompletion(CompletionId),
    TaskFinished(TaskId),
    TaskStillRunning(TaskId),
    InvalidToken(TaskId),
    ForeignToken,
    IdExhausted,
    ClockOverflow,
    RoundOverflow,
    UnsupportedVersion(u32),
    InvalidJournal(String),
    ReplayMismatch {
        index: usize,
        expected: Option<Box<TraceEvent>>,
        actual: Box<TraceEvent>,
    },
    ReplayUnconsumed {
        remaining: usize,
    },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// Not Clone. Its private identity cannot be changed or forged by safe callers.
/// The token must be returned to the simulator to yield, park, or finish.
pub struct RunToken<T> {
    instance: u64,
    task: TaskId,
    payload: T,
    reason: WakeReason,
    cancel_requested: bool,
}
impl<T> RunToken<T> {
    pub fn task(&self) -> TaskId {
        self.task
    }
    pub fn payload(&self) -> &T {
        &self.payload
    }
    pub fn payload_mut(&mut self) -> &mut T {
        &mut self.payload
    }
    pub fn reason(&self) -> WakeReason {
        self.reason
    }
    pub fn cancel_requested(&self) -> bool {
        self.cancel_requested
    }
}
/// Failed ownership transitions return the still-owned token unchanged.
pub struct TransitionError<T> {
    pub error: Error,
    pub token: RunToken<T>,
}
impl<T> fmt::Debug for TransitionError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransitionError")
            .field("error", &self.error)
            .field("task", &self.token.task)
            .finish_non_exhaustive()
    }
}
pub struct SpawnError<T> {
    pub error: Error,
    pub payload: T,
}
impl<T> fmt::Debug for SpawnError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpawnError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Sleeping {
        completion: CompletionId,
        deadline_ns: u64,
    },
    WaitingIo {
        completion: CompletionId,
    },
    Finished,
}
struct Task<T> {
    state: TaskState,
    payload: Option<T>,
    reason: WakeReason,
    cancel_requested: bool,
}
#[derive(Clone, Copy)]
enum Scheduled {
    Timer(CompletionId),
    Io(CompletionId, IoResult),
}
#[derive(Clone, Copy)]
struct Pending {
    task: TaskId,
    scheduled_key: Option<(u64, CompletionId)>,
}
enum History {
    Record(Journal),
    Replay { journal: Journal, cursor: usize },
}
/// A single-threaded simulator permits at most one checked-out running token.
/// With a fixed finite runnable set, each task is dispatched once per round.
pub struct Simulator<T> {
    instance: u64,
    now_ns: u64,
    rng: u64,
    round: u64,
    seen: BTreeSet<TaskId>,
    next_task: u64,
    next_completion: u64,
    tasks: BTreeMap<TaskId, Task<T>>,
    pending: BTreeMap<CompletionId, Pending>,
    scheduled: BTreeMap<(u64, CompletionId), Scheduled>,
    running: Option<TaskId>,
    history: History,
}
impl<T> Simulator<T> {
    pub fn new(seed: u64) -> Self {
        Self::with_history(
            seed,
            History::Record(Journal {
                version: JOURNAL_VERSION,
                seed,
                events: vec![],
            }),
        )
    }
    pub fn replay(journal: Journal) -> Result<Self, Error> {
        journal.validate_version()?;
        Ok(Self::with_history(
            journal.seed,
            History::Replay { journal, cursor: 0 },
        ))
    }
    fn with_history(seed: u64, history: History) -> Self {
        let mut instance = NEXT_SIMULATOR.load(Ordering::Relaxed);
        loop {
            let next = instance
                .checked_add(1)
                .expect("simulator instance IDs exhausted");
            match NEXT_SIMULATOR.compare_exchange_weak(
                instance,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => instance = actual,
            }
        }
        Self {
            instance,
            now_ns: 0,
            rng: seed,
            round: 0,
            seen: BTreeSet::new(),
            next_task: 1,
            next_completion: 1,
            tasks: BTreeMap::new(),
            pending: BTreeMap::new(),
            scheduled: BTreeMap::new(),
            running: None,
            history,
        }
    }
    pub fn now_ns(&self) -> u64 {
        self.now_ns
    }
    pub fn task_state(&self, task: TaskId) -> Option<TaskState> {
        self.tasks.get(&task).map(|t| t.state)
    }
    pub fn cancel_requested(&self, task: TaskId) -> Option<bool> {
        self.tasks.get(&task).map(|t| t.cancel_requested)
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn is_complete(&self) -> bool {
        self.tasks.values().all(|t| t.state == TaskState::Finished)
    }
    pub fn journal(&self) -> &Journal {
        match &self.history {
            History::Record(j) | History::Replay { journal: j, .. } => j,
        }
    }
    /// Verify that the full recorded prefix was consumed. Separately check
    /// `is_complete()` if the recording represents a complete program run.
    pub fn finish_replay(&self) -> Result<(), Error> {
        if let History::Replay { journal, cursor } = &self.history
            && *cursor != journal.events.len()
        {
            return Err(Error::ReplayUnconsumed {
                remaining: journal.events.len() - cursor,
            });
        }
        Ok(())
    }
    fn check(&self, events: &[TraceEvent]) -> Result<(), Error> {
        if let History::Replay { journal, cursor } = &self.history {
            for (offset, actual) in events.iter().enumerate() {
                let index = cursor + offset;
                let expected = journal.events.get(index);
                if expected != Some(actual) {
                    return Err(Error::ReplayMismatch {
                        index,
                        expected: expected.cloned().map(Box::new),
                        actual: Box::new(actual.clone()),
                    });
                }
            }
        }
        Ok(())
    }
    fn commit(&mut self, events: Vec<TraceEvent>) {
        match &mut self.history {
            History::Record(journal) => journal.events.extend(events),
            History::Replay { cursor, .. } => *cursor += events.len(),
        }
    }
    fn check_token(&self, token: &RunToken<T>) -> Result<(), Error> {
        if token.instance != self.instance {
            return Err(Error::ForeignToken);
        }
        if self.running != Some(token.task)
            || self.task_state(token.task) != Some(TaskState::Running)
        {
            return Err(Error::InvalidToken(token.task));
        }
        Ok(())
    }
    pub fn spawn(&mut self, payload: T) -> Result<TaskId, SpawnError<T>> {
        let task = TaskId(self.next_task);
        let event = TraceEvent::Spawn {
            task,
            at_ns: self.now_ns,
        };
        let result = self
            .next_task
            .checked_add(1)
            .ok_or(Error::IdExhausted)
            .and_then(|next| self.check(std::slice::from_ref(&event)).map(|()| next));
        let next = match result {
            Ok(next) => next,
            Err(error) => return Err(SpawnError { error, payload }),
        };
        self.tasks.insert(
            task,
            Task {
                state: TaskState::Ready,
                payload: Some(payload),
                reason: WakeReason::Start,
                cancel_requested: false,
            },
        );
        self.next_task = next;
        self.commit(vec![event]);
        Ok(task)
    }
    pub fn yield_task(&mut self, token: RunToken<T>) -> Result<(), TransitionError<T>> {
        let event = TraceEvent::Yield {
            task: token.task,
            at_ns: self.now_ns,
        };
        if let Err(error) = self
            .check_token(&token)
            .and_then(|()| self.check(std::slice::from_ref(&event)))
        {
            return Err(TransitionError { error, token });
        }
        let task = self.tasks.get_mut(&token.task).unwrap();
        task.state = TaskState::Ready;
        task.payload = Some(token.payload);
        task.reason = WakeReason::Yield;
        self.running = None;
        self.commit(vec![event]);
        Ok(())
    }
    pub fn finish(&mut self, token: RunToken<T>) -> Result<T, TransitionError<T>> {
        let event = TraceEvent::Finish {
            task: token.task,
            at_ns: self.now_ns,
        };
        if let Err(error) = self
            .check_token(&token)
            .and_then(|()| self.check(std::slice::from_ref(&event)))
        {
            return Err(TransitionError { error, token });
        }
        self.tasks.get_mut(&token.task).unwrap().state = TaskState::Finished;
        self.running = None;
        self.commit(vec![event]);
        Ok(token.payload)
    }
    pub fn sleep(
        &mut self,
        token: RunToken<T>,
        duration_ns: u64,
    ) -> Result<CompletionId, TransitionError<T>> {
        let deadline = match self.now_ns.checked_add(duration_ns) {
            Some(n) => n,
            None => {
                return Err(TransitionError {
                    error: Error::ClockOverflow,
                    token,
                });
            }
        };
        self.park(token, Some((deadline, None)))
    }
    pub fn park_io(&mut self, token: RunToken<T>) -> Result<CompletionId, TransitionError<T>> {
        self.park(token, None)
    }
    /// Inject a completion at `now + delay`, including failures and completion
    /// cancellation. Requesting cancellation never modifies this outcome.
    pub fn schedule_io(
        &mut self,
        token: RunToken<T>,
        delay_ns: u64,
        result: IoResult,
    ) -> Result<CompletionId, TransitionError<T>> {
        let deadline = match self.now_ns.checked_add(delay_ns) {
            Some(n) => n,
            None => {
                return Err(TransitionError {
                    error: Error::ClockOverflow,
                    token,
                });
            }
        };
        self.park(token, Some((deadline, Some(result))))
    }
    fn park(
        &mut self,
        token: RunToken<T>,
        schedule: Option<(u64, Option<IoResult>)>,
    ) -> Result<CompletionId, TransitionError<T>> {
        let completion = CompletionId(self.next_completion);
        let next = match self.next_completion.checked_add(1) {
            Some(n) => n,
            None => {
                return Err(TransitionError {
                    error: Error::IdExhausted,
                    token,
                });
            }
        };
        let event = match schedule {
            Some((deadline_ns, None)) => TraceEvent::Sleep {
                task: token.task,
                completion,
                deadline_ns,
            },
            _ => TraceEvent::SubmitIo {
                task: token.task,
                completion,
                scheduled: schedule.map(|(time, result)| (time, result.unwrap())),
            },
        };
        if let Err(error) = self
            .check_token(&token)
            .and_then(|()| self.check(std::slice::from_ref(&event)))
        {
            return Err(TransitionError { error, token });
        }
        let key = schedule.map(|(time, _)| (time, completion));
        let task = self.tasks.get_mut(&token.task).unwrap();
        task.state = match schedule {
            Some((deadline_ns, None)) => TaskState::Sleeping {
                completion,
                deadline_ns,
            },
            _ => TaskState::WaitingIo { completion },
        };
        task.payload = Some(token.payload);
        self.pending.insert(
            completion,
            Pending {
                task: token.task,
                scheduled_key: key,
            },
        );
        if let Some((time, result)) = schedule {
            self.scheduled.insert(
                (time, completion),
                match result {
                    None => Scheduled::Timer(completion),
                    Some(result) => Scheduled::Io(completion, result),
                },
            );
        }
        self.next_completion = next;
        self.running = None;
        self.commit(vec![event]);
        Ok(completion)
    }
    pub fn request_cancel(&mut self, id: TaskId) -> Result<bool, Error> {
        let task = self.tasks.get(&id).ok_or(Error::UnknownTask(id))?;
        if task.state == TaskState::Finished {
            return Err(Error::TaskFinished(id));
        }
        if task.cancel_requested {
            return Ok(false);
        }
        let event = TraceEvent::CancelRequested {
            task: id,
            at_ns: self.now_ns,
        };
        self.check(std::slice::from_ref(&event))?;
        self.tasks.get_mut(&id).unwrap().cancel_requested = true;
        self.commit(vec![event]);
        Ok(true)
    }
    pub fn complete_io(&mut self, completion: CompletionId, result: IoResult) -> Result<(), Error> {
        let pending = *self
            .pending
            .get(&completion)
            .ok_or(Error::UnknownCompletion(completion))?;
        if self.task_state(pending.task) != Some(TaskState::WaitingIo { completion }) {
            return Err(Error::UnknownCompletion(completion));
        }
        let event = TraceEvent::IoCompleted {
            task: pending.task,
            completion,
            at_ns: self.now_ns,
            result,
        };
        self.check(std::slice::from_ref(&event))?;
        self.pending.remove(&completion);
        if let Some(key) = pending.scheduled_key {
            self.scheduled.remove(&key);
        }
        let task = self.tasks.get_mut(&pending.task).unwrap();
        task.state = TaskState::Ready;
        task.reason = WakeReason::Io { completion, result };
        self.commit(vec![event]);
        Ok(())
    }
    /// Return one runnable owner. If all tasks are parked, jump to the earliest
    /// scheduled deadline. External pending I/O with no scheduled event returns
    /// None without advancing time; that is quiescence, not program completion.
    pub fn next_task(&mut self) -> Result<Option<RunToken<T>>, Error> {
        if let Some(task) = self.running {
            return Err(Error::TaskStillRunning(task));
        }
        let has_ready = self
            .tasks
            .values()
            .any(|task| task.state == TaskState::Ready);
        let now = if has_ready {
            self.now_ns
        } else {
            self.scheduled
                .first_key_value()
                .map_or(self.now_ns, |((time, _), _)| *time)
        };
        let due = self
            .scheduled
            .range(..=(now, CompletionId(u64::MAX)))
            .map(|(key, event)| (*key, *event))
            .collect::<Vec<_>>();
        let mut events = vec![];
        if now != self.now_ns {
            events.push(TraceEvent::ClockAdvance {
                from_ns: self.now_ns,
                to_ns: now,
            });
        }
        let mut waking = BTreeMap::new();
        for (_, event) in &due {
            let (completion, result) = match event {
                Scheduled::Timer(id) => (*id, None),
                Scheduled::Io(id, result) => (*id, Some(*result)),
            };
            let pending = self.pending[&completion];
            match result {
                None => {
                    events.push(TraceEvent::TimerElapsed {
                        task: pending.task,
                        completion,
                        at_ns: now,
                    });
                    waking.insert(pending.task, WakeReason::Timer(completion));
                }
                Some(result) => {
                    events.push(TraceEvent::IoCompleted {
                        task: pending.task,
                        completion,
                        at_ns: now,
                        result,
                    });
                    waking.insert(pending.task, WakeReason::Io { completion, result });
                }
            }
        }
        let ready = self
            .tasks
            .iter()
            .filter(|(id, task)| task.state == TaskState::Ready || waking.contains_key(id))
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        let mut round = self.round;
        let mut candidates = ready
            .iter()
            .copied()
            .filter(|id| !self.seen.contains(id))
            .collect::<Vec<_>>();
        let reset = !ready.is_empty() && candidates.is_empty();
        if reset {
            round = round.checked_add(1).ok_or(Error::RoundOverflow)?;
            candidates = ready;
        }
        let mut rng = self.rng;
        let selected = if candidates.is_empty() {
            None
        } else {
            let task = candidates[sample_index(&mut rng, candidates.len())];
            events.push(TraceEvent::Dispatch {
                task,
                at_ns: now,
                round,
            });
            Some(task)
        };
        self.check(&events)?;
        for (key, event) in due {
            self.scheduled.remove(&key);
            let completion = match event {
                Scheduled::Timer(id) | Scheduled::Io(id, _) => id,
            };
            self.pending.remove(&completion);
        }
        for (id, reason) in waking {
            let task = self.tasks.get_mut(&id).unwrap();
            task.state = TaskState::Ready;
            task.reason = reason;
        }
        self.now_ns = now;
        self.rng = rng;
        self.round = round;
        if reset {
            self.seen.clear();
        }
        self.commit(events);
        Ok(selected.map(|id| {
            self.seen.insert(id);
            self.running = Some(id);
            let task = self.tasks.get_mut(&id).unwrap();
            task.state = TaskState::Running;
            RunToken {
                instance: self.instance,
                task: id,
                payload: task.payload.take().unwrap(),
                reason: task.reason,
                cancel_requested: task.cancel_requested,
            }
        }))
    }
}
fn random(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}
fn sample_index(state: &mut u64, length: usize) -> usize {
    let bound = length as u64;
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let value = random(state);
        if value >= threshold {
            return (value % bound) as usize;
        }
    }
}
