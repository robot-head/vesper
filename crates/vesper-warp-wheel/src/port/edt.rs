//! The Vesper build of [`EdtLaunchQueue`]: the queue logic runs in
//! `vesper/src/edt.ves` (`EdtCore`), and this wrapper keeps the packets.

use std::fmt;

use super::*;
use crate::binding::EdtCore;
use crate::slab::Slab;
use crate::wheel::{AdvanceError, WheelConfig, WheelConfigError, report_dropped};

/// EDT launch queue over a [`crate::Wheel`].
pub struct EdtLaunchQueue<P> {
    core: EdtCore,
    packets: Slab<P>,
}

impl<P> fmt::Debug for EdtLaunchQueue<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EdtLaunchQueue")
            .field("len", &self.len())
            .field("horizon_ns", &self.horizon_ns())
            .field("last_released_ns", &self.last_released_ns())
            .finish_non_exhaustive()
    }
}

impl<P> EdtLaunchQueue<P> {
    /// Build a queue with the given wheel shape and horizon.
    pub fn new(config: WheelConfig, horizon_ns: u64) -> Result<Self, WheelConfigError> {
        let core = EdtCore::new(config, horizon_ns)?;
        Ok(Self {
            core,
            packets: Slab::new(config.capacity),
        })
    }

    /// Number of queued packets.
    pub fn len(&self) -> usize {
        self.core.len() as usize
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.core.len() == 0
    }

    /// Configured horizon.
    pub fn horizon_ns(&self) -> u64 {
        self.core.horizon_ns()
    }

    /// The queue's clock.
    pub fn now_ns(&self) -> u64 {
        self.core.now_ns()
    }

    /// Departure time of the most recently released packet.
    pub fn last_released_ns(&self) -> Option<u64> {
        self.core.last_released_ns()
    }

    /// Lower bound on the next release time.
    pub fn next_wake_hint_ns(&self) -> Option<u64> {
        self.core.next_wake_hint_ns()
    }

    /// Queue `packet` to depart at `edt_ns`, observing time `now_ns`. An EDT
    /// at or before `now_ns` (but not before the last release) departs on
    /// the next poll.
    pub fn submit(&mut self, now_ns: u64, edt_ns: u64, packet: P) -> Result<(), SubmitError<P>> {
        match self.core.submit(now_ns, edt_ns) {
            Ok(index) => {
                self.packets.put(index, packet);
                Ok(())
            }
            Err(kind) => Err(SubmitError { kind, packet }),
        }
    }

    /// Advance to `now_ns` and release the earliest packet with
    /// `edt_ns <= now_ns`. Call repeatedly to release all due packets.
    pub fn poll_release(&mut self, now_ns: u64) -> Result<Option<Launch<P>>, AdvanceError> {
        Ok(self
            .core
            .poll_release(now_ns)?
            .map(|(edt_ns, index)| Launch {
                edt_ns,
                packet: self.packets.take(index),
            }))
    }

    /// Remove every queued packet, handing `(edt_ns, packet)` to `sink` in
    /// unspecified order.
    pub fn drain(&mut self, mut sink: impl FnMut(u64, P)) {
        // Take every payload before calling the sink, so a panicking sink
        // cannot leave payloads in slots Vesper has already freed.
        let drained: Vec<_> = self
            .core
            .drain()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[deadline, index]| (deadline, self.packets.take(index)))
            .collect();
        for (deadline, value) in drained {
            sink(deadline, value);
        }
    }
}

impl<P> Drop for EdtLaunchQueue<P> {
    fn drop(&mut self) {
        report_dropped(self.core.len());
    }
}
