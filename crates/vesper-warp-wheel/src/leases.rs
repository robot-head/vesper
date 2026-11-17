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

use crate::wheel::HandleError;

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

/// Map a wheel handle error into the lease vocabulary.
impl From<HandleError> for LeaseError {
    fn from(_: HandleError) -> Self {
        Self::NotOutstanding
    }
}

// The Rust build (the frozen reference) or the Vesper build (a thin wrapper
// over the Vesper port in vesper/src/).
#[cfg(not(feature = "vesper-port"))]
#[path = "reference/leases.rs"]
mod imp;
#[cfg(feature = "vesper-port")]
#[path = "port/leases.rs"]
mod imp;

pub use imp::RetransmitLeases;
