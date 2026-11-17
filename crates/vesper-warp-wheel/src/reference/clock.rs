//! The Rust build of [`VirtualClock`]'s moves (see `crate::clock`). Frozen
//! from port wave P1 lane 3 (#900): bug fixes only.

use super::*;

impl VirtualClock {
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
