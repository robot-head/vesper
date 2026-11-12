//! Real current-thread hardware-event observations, never a clock fallback.
//!
//! The initial provider uses Linux group-read syscalls on little-endian 64-bit
//! x86_64/aarch64. Counts exclude kernel/hypervisor execution. They include the
//! enable return path, harness dispatch, Instant readings and observer boundary
//! work through disable. They are not RDPMC readings or calibrated body-only
//! costs. No affinity, sysctl or privilege settings are changed.
//!
//! Counts are cumulative kernel u64 accounting, not a guessed physical-counter
//! width. A delta requires monotonic values and assumes no full u64 wrap between
//! snapshots, no external reset/reconfiguration, and a fully scheduled interval.
//! No multiplex scaling is performed. Availability alone proves no performance,
//! compiler, allocation or SIMD property. Tokens and groups cannot cross threads;
//! OS process/thread checks additionally reject use after fork. Dropping an
//! unfinished token does not guarantee progress: drop the group to close its FDs.
//!
//! Thread ownership is enforced by the Rust API as well as OS identity checks:
//! ```compile_fail
//! fn require_send<T: Send>() {}
//! require_send::<vesper_bench_pmu::PerfGroup>();
//! ```
//! ```compile_fail
//! fn require_send<T: Send>() {}
//! require_send::<vesper_bench_pmu::Snapshot>();
//! ```
//! ```compile_fail
//! fn require_send<T: Send>() {}
//! require_send::<vesper_bench_pmu::CounterDelta>();
//! ```
#![cfg_attr(
    not(all(
        target_os = "linux",
        target_pointer_width = "64",
        target_endian = "little",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )),
    forbid(unsafe_code)
)]

use std::{fmt, marker::PhantomData, rc::Rc};

/// Structured failure; no fabricated zero count is substituted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    UnsupportedPlatform,
    PermissionDenied,
    OsError,
    InvalidRead,
    InvalidMetadata,
    WrongProcessOrThread,
    InvalidState,
    ForeignOrStaleToken,
    Exhausted,
    CounterRegression,
    NotFullyScheduled,
    NoEvents,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    pub reason: UnavailableReason,
    pub operation: &'static str,
    pub os_error: Option<i32>,
}
impl Unavailable {
    fn new(reason: UnavailableReason, operation: &'static str) -> Self {
        Self {
            reason,
            operation,
            os_error: None,
        }
    }
}
impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PMU unavailable: {:?} at {}",
            self.reason, self.operation
        )?;
        if let Some(errno) = self.os_error {
            write!(f, " (OS error {errno})")?;
        }
        Ok(())
    }
}
impl std::error::Error for Unavailable {}

/// Provider-produced, nonforgeable raw hardware-event delta. No public constructor.
#[derive(Debug)]
pub struct CounterDelta {
    cycles: u64,
    instructions: u64,
    enabled: u64,
    running: u64,
    _thread_bound: PhantomData<Rc<()>>,
}
impl CounterDelta {
    pub fn cycles(&self) -> u64 {
        self.cycles
    }
    pub fn instructions(&self) -> u64 {
        self.instructions
    }
    pub fn enabled_nanos(&self) -> u64 {
        self.enabled
    }
    pub fn running_nanos(&self) -> u64 {
        self.running
    }
    pub fn provider(&self) -> &'static str {
        "linux.perf_event_open.group_read"
    }
    pub fn unit(&self) -> &'static str {
        "hardware events"
    }
}

#[cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux;
#[cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use linux::{PerfGroup, Snapshot};

#[cfg(not(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
mod unsupported {
    use super::*;
    use vesper_bench::{Invocation, WindowCompletion, WindowObserver};
    pub struct PerfGroup {
        _thread_bound: PhantomData<Rc<()>>,
    }
    pub struct Snapshot {
        _thread_bound: PhantomData<Rc<()>>,
    }
    impl PerfGroup {
        pub fn open() -> Result<Self, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "perf_event_open",
            ))
        }
    }
    impl WindowObserver for PerfGroup {
        type Token = Snapshot;
        type Observation = CounterDelta;
        type Error = Unavailable;
        fn before_window(&mut self, _: Invocation, _: usize) -> Result<Snapshot, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "before_window",
            ))
        }
        fn after_window(
            &mut self,
            _: Snapshot,
            _: WindowCompletion,
        ) -> Result<CounterDelta, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "after_window",
            ))
        }
    }
}
#[cfg(not(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
pub use unsupported::{PerfGroup, Snapshot};
