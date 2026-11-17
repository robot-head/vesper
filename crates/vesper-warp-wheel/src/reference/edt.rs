//! The Rust build of [`EdtLaunchQueue`] (see `crate::edt`). Frozen from
//! port wave P1 lane 3 (#900): bug fixes only.

use super::*;
use crate::wheel::{AdvanceError, InsertErrorKind, Wheel, WheelConfig, WheelConfigError};

/// EDT launch queue over a [`Wheel`].
#[derive(Debug)]
pub struct EdtLaunchQueue<P> {
    wheel: Wheel<P>,
    horizon_ns: u64,
    last_released_ns: Option<u64>,
}

impl<P> EdtLaunchQueue<P> {
    /// Build a queue with the given wheel shape and horizon.
    pub fn new(config: WheelConfig, horizon_ns: u64) -> Result<Self, WheelConfigError> {
        Ok(Self {
            wheel: Wheel::new(config)?,
            horizon_ns,
            last_released_ns: None,
        })
    }

    /// Number of queued packets.
    pub fn len(&self) -> usize {
        self.wheel.len()
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.wheel.is_empty()
    }

    /// Configured horizon.
    pub fn horizon_ns(&self) -> u64 {
        self.horizon_ns
    }

    /// The queue's clock.
    pub fn now_ns(&self) -> u64 {
        self.wheel.now_ns()
    }

    /// Departure time of the most recently released packet.
    pub fn last_released_ns(&self) -> Option<u64> {
        self.last_released_ns
    }

    /// Lower bound on the next release time.
    pub fn next_wake_hint_ns(&self) -> Option<u64> {
        self.wheel.next_wake_hint_ns()
    }

    /// Queue `packet` to depart at `edt_ns`, observing time `now_ns`. An EDT
    /// at or before `now_ns` (but not before the last release) departs on
    /// the next poll.
    pub fn submit(&mut self, now_ns: u64, edt_ns: u64, packet: P) -> Result<(), SubmitError<P>> {
        let fail = |kind, packet| Err(SubmitError { kind, packet });
        match self.wheel.advance(now_ns) {
            Ok(()) => {}
            Err(AdvanceError::Backwards { .. }) => {
                return fail(SubmitErrorKind::ClockBackwards, packet);
            }
            Err(AdvanceError::OutOfRange) => return fail(SubmitErrorKind::OutOfRange, packet),
        }
        if edt_ns > now_ns.saturating_add(self.horizon_ns) {
            return fail(SubmitErrorKind::BeyondHorizon, packet);
        }
        if self.last_released_ns.is_some_and(|last| edt_ns < last) {
            return fail(SubmitErrorKind::BehindReleased, packet);
        }
        match self.wheel.insert(edt_ns, packet) {
            // Packets are not individually cancellable; the queue keeps no
            // handle and every packet leaves by release or drain.
            Ok(handle) => {
                drop(handle);
                Ok(())
            }
            Err(err) => {
                let kind = match err.kind {
                    InsertErrorKind::Full => SubmitErrorKind::Full,
                    InsertErrorKind::DeadlineOutOfRange => SubmitErrorKind::OutOfRange,
                };
                fail(kind, err.payload)
            }
        }
    }

    /// Advance to `now_ns` and release the earliest packet with
    /// `edt_ns <= now_ns`. Call repeatedly to release all due packets.
    pub fn poll_release(&mut self, now_ns: u64) -> Result<Option<Launch<P>>, AdvanceError> {
        let Some(fired) = self.wheel.poll(now_ns)? else {
            return Ok(None);
        };
        debug_assert!(self.last_released_ns.is_none_or(|l| l <= fired.deadline_ns));
        self.last_released_ns = Some(fired.deadline_ns);
        Ok(Some(Launch {
            edt_ns: fired.deadline_ns,
            packet: fired.payload,
        }))
    }

    /// Remove every queued packet, handing `(edt_ns, packet)` to `sink` in
    /// unspecified order.
    pub fn drain(&mut self, sink: impl FnMut(u64, P)) {
        self.wheel.drain(sink);
    }
}
