//! The Vesper build of [`VirtualClock`]'s moves: the checks run in
//! `vesper/src/clock.ves`, the clock value stays here (pattern d: the wrapper
//! keeps the state and passes it in).

use super::*;
use crate::binding;

impl VirtualClock {
    /// Move to an absolute time; earlier times are rejected.
    pub fn advance_to(&mut self, now_ns: u64) -> Result<u64, ClockError> {
        self.now_ns = binding::clock_advance_to(self.now_ns, now_ns)?;
        Ok(self.now_ns)
    }

    /// Move forward by `delta_ns` with checked arithmetic.
    pub fn advance_by(&mut self, delta_ns: u64) -> Result<u64, ClockError> {
        self.now_ns = binding::clock_advance_by(self.now_ns, delta_ns)?;
        Ok(self.now_ns)
    }
}
