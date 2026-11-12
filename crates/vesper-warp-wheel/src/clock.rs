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
    now_ns: u64,
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

    /// Move to an absolute time; earlier times are rejected.
    pub fn advance_to(&mut self, now_ns: u64) -> Result<u64, ClockError> {
        if now_ns < self.now_ns {
            return Err(ClockError::Backwards {
                now_ns: self.now_ns,
                requested_ns: now_ns,
            });
        }
        if now_ns > crate::MAX_TIME_NS {
            return Err(ClockError::OutOfRange);
        }
        self.now_ns = now_ns;
        Ok(now_ns)
    }

    /// Move forward by `delta_ns` with checked arithmetic.
    pub fn advance_by(&mut self, delta_ns: u64) -> Result<u64, ClockError> {
        let next = self
            .now_ns
            .checked_add(delta_ns)
            .ok_or(ClockError::OutOfRange)?;
        self.advance_to(next)
    }
}

impl Clock for VirtualClock {
    fn now_ns(&self) -> u64 {
        self.now_ns
    }
}
