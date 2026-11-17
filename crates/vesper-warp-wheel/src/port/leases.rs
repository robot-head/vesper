//! The Vesper build of [`RetransmitLeases`]: the key index and the wheel
//! run in `vesper/src/leases.ves` (`LeaseCore`), and this wrapper keeps the
//! leases with their keys.

use std::fmt;

use super::*;
use crate::binding::LeaseCore;
use crate::slab::Slab;
use crate::wheel::{AdvanceError, WheelConfig, WheelConfigError, report_dropped};

/// Lease table keyed by `(space, packet number)` over a [`crate::Wheel`].
pub struct RetransmitLeases<L> {
    core: LeaseCore,
    leases: Slab<(LeaseKey, L)>,
}

impl<L> fmt::Debug for RetransmitLeases<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetransmitLeases")
            .field("len", &self.len())
            .field("now_ns", &self.now_ns())
            .finish_non_exhaustive()
    }
}

impl<L> RetransmitLeases<L> {
    /// Build a table; `config.capacity` bounds the number of parked leases.
    pub fn new(config: WheelConfig) -> Result<Self, WheelConfigError> {
        let core = LeaseCore::new(config)?;
        Ok(Self {
            core,
            leases: Slab::new(config.capacity),
        })
    }

    /// Number of parked leases.
    pub fn len(&self) -> usize {
        self.core.len() as usize
    }

    /// Whether nothing is parked.
    pub fn is_empty(&self) -> bool {
        self.core.len() == 0
    }

    /// Whether a lease is parked under `key`.
    pub fn contains(&self, key: LeaseKey) -> bool {
        self.core.contains(key.space, key.pn)
    }

    /// The table's clock.
    pub fn now_ns(&self) -> u64 {
        self.core.now_ns()
    }

    /// Lower bound on the next PTO firing time (see
    /// [`crate::Wheel::next_wake_hint_ns`]).
    pub fn next_wake_hint_ns(&self) -> Option<u64> {
        self.core.next_wake_hint_ns()
    }

    /// Park `lease` for `(space, pn)` until it is acknowledged or
    /// `pto_deadline_ns` passes.
    pub fn park(
        &mut self,
        space: PacketNumberSpace,
        pn: u64,
        pto_deadline_ns: u64,
        lease: L,
    ) -> Result<(), ParkError<L>> {
        match self.core.park(space, pn, pto_deadline_ns) {
            Ok(index) => {
                self.leases.put(index, (LeaseKey { space, pn }, lease));
                Ok(())
            }
            Err(kind) => Err(ParkError { kind, lease }),
        }
    }

    /// An ACK arrived for `(space, pn)`: return its lease.
    pub fn on_ack(&mut self, space: PacketNumberSpace, pn: u64) -> Result<L, LeaseError> {
        let index = self.core.on_ack(space, pn)?;
        Ok(self.leases.take(index).1)
    }

    /// Move a parked lease's PTO deadline.
    pub fn rearm(
        &mut self,
        space: PacketNumberSpace,
        pn: u64,
        pto_deadline_ns: u64,
    ) -> Result<(), LeaseError> {
        let (from, to) = self.core.rearm(space, pn, pto_deadline_ns)?;
        self.leases.relocate(from, to);
        Ok(())
    }

    /// Advance to `now_ns` and return the earliest lease whose PTO deadline
    /// is `<= now_ns`. Call repeatedly to drain all due leases.
    pub fn poll_pto(&mut self, now_ns: u64) -> Result<Option<PtoExpired<L>>, AdvanceError> {
        Ok(self.core.poll_pto(now_ns)?.map(|(deadline_ns, index)| {
            let (key, lease) = self.leases.take(index);
            PtoExpired {
                key,
                deadline_ns,
                lease,
            }
        }))
    }

    /// Remove every lease in `space` (e.g. when its keys are discarded),
    /// handing each to `sink` in unspecified order. Vesper returns the
    /// removed entries as one sequence, so this allocates once.
    pub fn discard_space(&mut self, space: PacketNumberSpace, mut sink: impl FnMut(LeaseKey, L)) {
        // Take every lease before calling the sink (see `Wheel::drain`).
        let removed: Vec<_> = self
            .core
            .discard_space(space)
            .into_iter()
            .map(|index| self.leases.take(index))
            .collect();
        for (key, lease) in removed {
            sink(key, lease);
        }
    }

    /// Remove every lease, handing each to `sink` in unspecified order.
    pub fn drain(&mut self, mut sink: impl FnMut(LeaseKey, L)) {
        // Take every lease before calling the sink (see `Wheel::drain`).
        let removed: Vec<_> = self
            .core
            .drain()
            .into_iter()
            .map(|index| self.leases.take(index))
            .collect();
        for (key, lease) in removed {
            sink(key, lease);
        }
    }
}

impl<L> Drop for RetransmitLeases<L> {
    fn drop(&mut self) {
        report_dropped(self.core.len());
    }
}
