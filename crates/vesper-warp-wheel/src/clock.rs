//! Deterministic virtual time.
//!
//! Library code in this crate never reads a real clock: every operation takes
//! `now_ns: u64` from the caller (wave-four shared vocabulary). [`VirtualClock`]
//! is a convenience source of such values for tests and simulations. It never
//! sleeps, spawns threads or consults the operating system.

use std::fmt;

/// A source of monotonic nanosecond timestamps.
pub trait Clock {
    /// Current time in nanoseconds on this clock's own epoch.
    fn now_ns(&self) -> u64;
}

/// A manually advanced, strictly monotonic (non-decreasing) virtual clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VirtualClock {
    pub(crate) now_ns: u64,
}

/// Rejected virtual-clock movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockError {
    /// The requested time is earlier than the clock's current time.
    Backwards {
        /// Current time.
        now_ns: u64,
        /// Requested (earlier) time.
        requested_ns: u64,
    },
    /// The requested movement overflows `u64` nanoseconds or exceeds the
    /// supported range ([`crate::MAX_TIME_NS`]).
    OutOfRange,
}

impl fmt::Display for ClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backwards {
                now_ns,
                requested_ns,
            } => write!(
                f,
                "clock moved backwards from {now_ns} ns to {requested_ns} ns"
            ),
            Self::OutOfRange => f.write_str("time is outside the supported nanosecond range"),
        }
    }
}

impl std::error::Error for ClockError {}

impl VirtualClock {
    /// A clock starting at `start_ns`.
    pub const fn starting_at(start_ns: u64) -> Self {
        Self { now_ns: start_ns }
    }
}

impl Clock for VirtualClock {
    fn now_ns(&self) -> u64 {
        self.now_ns
    }
}

// The Rust build (the frozen reference) or the Vesper build (forwarding the
// checks to the Vesper port in vesper/src/). Either adds `advance_to` and `advance_by`.
#[cfg(not(feature = "vesper-port"))]
#[path = "reference/clock.rs"]
mod imp;
#[cfg(feature = "vesper-port")]
#[path = "port/clock.rs"]
mod imp;
