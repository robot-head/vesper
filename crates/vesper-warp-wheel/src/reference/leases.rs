//! The Rust build of [`RetransmitLeases`] (see `crate::leases`). Frozen
//! from port wave P1 lane 3 (#900): bug fixes only.

use std::fmt;

use super::*;
use crate::wheel::{
    AdvanceError, InsertErrorKind, MAX_TIME_NS, TimerHandle, Wheel, WheelConfig, WheelConfigError,
};

fn tag(space: PacketNumberSpace) -> u64 {
    match space {
        PacketNumberSpace::Initial => 0,
        PacketNumberSpace::Handshake => 1,
        PacketNumberSpace::ApplicationData => 2,
    }
}

struct Parked<L> {
    key: LeaseKey,
    lease: L,
}

struct IndexSlot {
    key: LeaseKey,
    handle: TimerHandle,
}

/// Lease table keyed by `(space, packet number)` over a [`Wheel`].
pub struct RetransmitLeases<L> {
    wheel: Wheel<Parked<L>>,
    index: Vec<Option<IndexSlot>>,
    mask: usize,
}

impl<L> fmt::Debug for RetransmitLeases<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetransmitLeases")
            .field("wheel", &self.wheel)
            .finish_non_exhaustive()
    }
}

/// The packet-number space tag sits above the 62-bit packet number.
const SPACE_TAG_SHIFT: u32 = 62;
/// SplitMix64 finalizer multipliers.
const SPLITMIX_MUL_1: u64 = 0xbf58_476d_1ce4_e5b9;
const SPLITMIX_MUL_2: u64 = 0x94d0_49bb_1331_11eb;

fn mix(key: LeaseKey) -> u64 {
    // SplitMix64 finalizer over the packed key; deterministic by design (see
    // module docs on why flooding needs inserts the peer cannot make).
    let mut z = (tag(key.space) << SPACE_TAG_SHIFT) | key.pn;
    z = (z ^ (z >> 30)).wrapping_mul(SPLITMIX_MUL_1);
    z = (z ^ (z >> 27)).wrapping_mul(SPLITMIX_MUL_2);
    z ^ (z >> 31)
}

impl<L> RetransmitLeases<L> {
    /// Build a table; `config.capacity` bounds the number of parked leases.
    pub fn new(config: WheelConfig) -> Result<Self, WheelConfigError> {
        let wheel = Wheel::new(config)?;
        let slots = (config.capacity as usize)
            .checked_mul(2)
            .and_then(usize::checked_next_power_of_two)
            .ok_or(WheelConfigError::Capacity(config.capacity))?;
        let mut index = Vec::with_capacity(slots);
        index.resize_with(slots, || None);
        Ok(Self {
            wheel,
            index,
            mask: slots - 1,
        })
    }

    /// Number of parked leases.
    pub fn len(&self) -> usize {
        self.wheel.len()
    }

    /// Whether nothing is parked.
    pub fn is_empty(&self) -> bool {
        self.wheel.is_empty()
    }

    /// Whether a lease is parked under `key`.
    pub fn contains(&self, key: LeaseKey) -> bool {
        self.find(key).is_some()
    }

    /// The table's clock.
    pub fn now_ns(&self) -> u64 {
        self.wheel.now_ns()
    }

    /// Lower bound on the next PTO firing time (see
    /// [`Wheel::next_wake_hint_ns`]).
    pub fn next_wake_hint_ns(&self) -> Option<u64> {
        self.wheel.next_wake_hint_ns()
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
        let fail = |kind, lease| Err(ParkError { kind, lease });
        if pn >= MAX_PACKET_NUMBER {
            return fail(ParkErrorKind::PacketNumberOutOfRange, lease);
        }
        let key = LeaseKey { space, pn };
        if self.find(key).is_some() {
            return fail(ParkErrorKind::Duplicate, lease);
        }
        let handle = match self.wheel.insert(pto_deadline_ns, Parked { key, lease }) {
            Ok(handle) => handle,
            Err(err) => {
                let kind = match err.kind {
                    InsertErrorKind::Full => ParkErrorKind::Full,
                    InsertErrorKind::DeadlineOutOfRange => ParkErrorKind::DeadlineOutOfRange,
                };
                return fail(kind, err.payload.lease);
            }
        };
        // Load factor <= 1/2 guarantees a free slot.
        let mut i = (mix(key) as usize) & self.mask;
        while self.index[i].is_some() {
            i = (i + 1) & self.mask;
        }
        self.index[i] = Some(IndexSlot { key, handle });
        Ok(())
    }

    /// An ACK arrived for `(space, pn)`: return its lease.
    pub fn on_ack(&mut self, space: PacketNumberSpace, pn: u64) -> Result<L, LeaseError> {
        let key = LeaseKey { space, pn };
        let i = self.find(key).ok_or(LeaseError::NotOutstanding)?;
        let slot = self.remove_at(i);
        let parked = self
            .wheel
            .cancel(slot.handle)
            .map_err(|_| LeaseError::NotOutstanding)?;
        Ok(parked.lease)
    }

    /// Move a parked lease's PTO deadline.
    pub fn rearm(
        &mut self,
        space: PacketNumberSpace,
        pn: u64,
        pto_deadline_ns: u64,
    ) -> Result<(), LeaseError> {
        if pto_deadline_ns > MAX_TIME_NS {
            return Err(LeaseError::DeadlineOutOfRange);
        }
        let key = LeaseKey { space, pn };
        let i = self.find(key).ok_or(LeaseError::NotOutstanding)?;
        let slot = self.index[i].take().ok_or(LeaseError::NotOutstanding)?;
        let parked = self
            .wheel
            .cancel(slot.handle)
            .map_err(|_| LeaseError::NotOutstanding)?;
        // The cancel freed a slot and the deadline is in range, so this insert
        // cannot fail.
        let handle = match self.wheel.insert(pto_deadline_ns, parked) {
            Ok(handle) => handle,
            Err(_) => unreachable!("re-insert after cancel cannot fail"),
        };
        self.index[i] = Some(IndexSlot { key, handle });
        Ok(())
    }

    /// Advance to `now_ns` and return the earliest lease whose PTO deadline
    /// is `<= now_ns`. Call repeatedly to drain all due leases.
    pub fn poll_pto(&mut self, now_ns: u64) -> Result<Option<PtoExpired<L>>, AdvanceError> {
        let Some(fired) = self.wheel.poll(now_ns)? else {
            return Ok(None);
        };
        let key = fired.payload.key;
        if let Some(i) = self.find(key) {
            // The handle is stale now; dropping it is correct.
            let _stale = self.remove_at(i);
        }
        Ok(Some(PtoExpired {
            key,
            deadline_ns: fired.deadline_ns,
            lease: fired.payload.lease,
        }))
    }

    /// Remove every lease in `space` (e.g. when its keys are discarded),
    /// handing each to `sink` in unspecified order. Does not allocate.
    pub fn discard_space(&mut self, space: PacketNumberSpace, mut sink: impl FnMut(LeaseKey, L)) {
        let mut i = 0;
        while i < self.index.len() {
            match &self.index[i] {
                Some(slot) if slot.key.space == space => {
                    let slot = self.remove_at(i);
                    if let Ok(parked) = self.wheel.cancel(slot.handle) {
                        sink(parked.key, parked.lease);
                    }
                    // Backward shift may have moved another key into `i`.
                }
                _ => i += 1,
            }
        }
    }

    /// Remove every lease, handing each to `sink` in unspecified order.
    pub fn drain(&mut self, mut sink: impl FnMut(LeaseKey, L)) {
        for slot in &mut self.index {
            *slot = None;
        }
        self.wheel.drain(|_, parked| sink(parked.key, parked.lease));
    }

    fn find(&self, key: LeaseKey) -> Option<usize> {
        let mut i = (mix(key) as usize) & self.mask;
        loop {
            match &self.index[i] {
                None => return None,
                Some(slot) if slot.key == key => return Some(i),
                Some(_) => i = (i + 1) & self.mask,
            }
        }
    }

    fn remove_at(&mut self, mut hole: usize) -> IndexSlot {
        let removed = self.index[hole]
            .take()
            .expect("remove_at is only called on occupied slots");
        let mut j = hole;
        loop {
            j = (j + 1) & self.mask;
            let Some(slot) = &self.index[j] else {
                break;
            };
            let home = (mix(slot.key) as usize) & self.mask;
            // Move `j` into the hole unless its home lies cyclically in
            // (hole, j].
            let in_range = if hole <= j {
                hole < home && home <= j
            } else {
                hole < home || home <= j
            };
            if !in_range {
                self.index[hole] = self.index[j].take();
                hole = j;
            }
        }
        removed
    }
}
