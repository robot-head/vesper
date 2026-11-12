//! Retransmission / PTO lease table (§35.2 `park_for_ack_or_pto`).
//!
//! A lease is the caller's owner of an in-flight packet (in the integrated
//! system, `InFlightQuicPkt` holding its TX UMEM frame). Parking hands the
//! lease to the table under `(space, packet number)` with a PTO deadline. It
//! leaves by exactly one of:
//!
//! * [`RetransmitLeases::on_ack`]: an ACK for that packet returns the lease
//!   (to recycle its frame);
//! * [`RetransmitLeases::poll_pto`]: the deadline passed, the lease is
//!   returned for retransmission;
//! * [`RetransmitLeases::discard_space`] / [`RetransmitLeases::drain`]:
//!   explicit bulk removal (key discard, connection close).
//!
//! The key index is a fixed-capacity open-addressing table (linear probing,
//! backward-shift deletion, load factor <= 1/2) allocated at construction, so
//! no operation allocates afterwards. Keys are inserted only for packets we
//! sent; an attacker-chosen ACK can only probe, never insert.

use std::fmt;

use crate::wheel::{
    AdvanceError, HandleError, InsertErrorKind, MAX_TIME_NS, TimerHandle, Wheel, WheelConfig,
    WheelConfigError,
};

/// Exclusive upper bound of QUIC packet numbers (RFC 9000 §12.3).
pub const MAX_PACKET_NUMBER: u64 = 1 << 62;

/// QUIC packet-number space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PacketNumberSpace {
    /// Initial packets.
    Initial,
    /// Handshake packets.
    Handshake,
    /// 0-RTT and 1-RTT packets.
    ApplicationData,
}

impl PacketNumberSpace {
    fn tag(self) -> u64 {
        match self {
            Self::Initial => 0,
            Self::Handshake => 1,
            Self::ApplicationData => 2,
        }
    }
}

/// `(space, packet number)` identifying one parked lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeaseKey {
    /// Packet-number space.
    pub space: PacketNumberSpace,
    /// Full packet number, `< 2^62`.
    pub pn: u64,
}

/// Why a park was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParkErrorKind {
    /// The packet number is `>= 2^62`.
    PacketNumberOutOfRange,
    /// A lease is already parked under this key.
    Duplicate,
    /// All lease slots are in use.
    Full,
    /// The PTO deadline exceeds [`MAX_TIME_NS`].
    DeadlineOutOfRange,
}

/// A refused park; the lease is handed back.
#[must_use = "the refused lease is returned and must be handled"]
pub struct ParkError<L> {
    /// The reason.
    pub kind: ParkErrorKind,
    /// The lease that was not parked.
    pub lease: L,
}

impl<L> ParkError<L> {
    /// Recover the lease.
    pub fn into_lease(self) -> L {
        self.lease
    }
}

impl<L> fmt::Debug for ParkError<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParkError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl<L> fmt::Display for ParkError<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            ParkErrorKind::PacketNumberOutOfRange => "packet number is not below 2^62",
            ParkErrorKind::Duplicate => "a lease is already parked for this packet",
            ParkErrorKind::Full => "lease table is at capacity",
            ParkErrorKind::DeadlineOutOfRange => "PTO deadline exceeds MAX_TIME_NS",
        };
        f.write_str(what)
    }
}

impl<L> std::error::Error for ParkError<L> {}

/// Lookup failure for ACK or rearm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseError {
    /// No lease is parked under the key (never sent, already acknowledged,
    /// already fired or discarded). Duplicate ACKs land here.
    NotOutstanding,
    /// The new deadline exceeds [`MAX_TIME_NS`].
    DeadlineOutOfRange,
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOutstanding => f.write_str("no lease is outstanding for this packet"),
            Self::DeadlineOutOfRange => f.write_str("deadline exceeds MAX_TIME_NS"),
        }
    }
}

impl std::error::Error for LeaseError {}

/// A lease whose PTO deadline passed; retransmit its contents.
#[must_use = "a fired lease carries the in-flight packet owner"]
#[derive(Debug, PartialEq, Eq)]
pub struct PtoExpired<L> {
    /// The packet's key.
    pub key: LeaseKey,
    /// The deadline that passed.
    pub deadline_ns: u64,
    /// The owned lease.
    pub lease: L,
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

fn mix(key: LeaseKey) -> u64 {
    // SplitMix64 finalizer over the packed key; deterministic by design (see
    // module docs on why flooding needs inserts the peer cannot make).
    let mut z = (key.space.tag() << 62) | key.pn;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
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

/// Map a wheel handle error into the lease vocabulary.
impl From<HandleError> for LeaseError {
    fn from(_: HandleError) -> Self {
        Self::NotOutstanding
    }
}
