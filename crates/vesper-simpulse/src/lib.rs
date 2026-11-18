#![doc = include_str!("../README.md")]
//! Deterministic scheduling metadata and owned continuation tokens for SimPulse.
//!
//! This kernel does not execute Vesper, emulate an OS, or explore DPOR schedules.
//! The embedder runs each returned token and explicitly yields, parks or finishes
//! it. Cancellation is a request flag; only actual completion releases parked
//! payloads. Journal replay validates transitions before ownership is moved.
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

const JOURNAL_VERSION: u32 = 1;
static NEXT_SIMULATOR: AtomicU64 = AtomicU64::new(1);

// The seed package builds the Rust code for the compiler's closure; the
// package `vesper-simpulse` is the port-backed build, whose build
// script links the Vesper port in vesper/ and sets cfg(vesper_port). See
// "Crates the compiler links" in docs/maintenance/porting-a-crate.md.
#[cfg(not(vesper_port))]
mod reference;
#[cfg(not(vesper_port))]
pub use reference::Simulator;

#[cfg(vesper_port)]
#[allow(clippy::all, dead_code)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod binding;
#[cfg(vesper_port)]
mod ported;
#[cfg(vesper_port)]
pub use ported::Simulator;

/// Which build this is: `"vesper"` for the port, `"rust"` for the seed.
pub const IMPLEMENTATION: &str = if cfg!(vesper_port) { "vesper" } else { "rust" };

/// Allocations the Vesper library holds now (leak tests only).
#[cfg(vesper_port)]
#[doc(hidden)]
pub fn port_live_allocations() -> i64 {
    binding::live_allocations()
}

/// A fresh simulator instance identity; tokens carry it so that a token of
/// another simulator is foreign.
fn next_instance() -> u64 {
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
            Ok(_) => break instance,
            Err(actual) => instance = actual,
        }
    }
}

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
