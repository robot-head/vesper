//! Typed errors. Every rejected input maps to one of these; nothing panics on
//! caller- or wire-supplied values.

use crate::simd::UnsupportedIsa;
use core::fmt;

/// Invalid [`crate::TableConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Capacity of zero connections.
    ZeroCapacity,
    /// Capacity above [`crate::MAX_CAPACITY`].
    CapacityTooLarge {
        /// Requested capacity.
        requested: u32,
    },
    /// Local CID length outside 1..=20 (zero-length CIDs cannot be routed).
    CidLength {
        /// Requested length.
        len: u8,
    },
    /// The requested ISA path is not available.
    Isa(UnsupportedIsa),
}

/// Rejected [`crate::ConnTable::insert`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertError {
    /// Every slot is occupied.
    Full,
    /// Another live connection already uses this local CID.
    DuplicateCid,
    /// The local CID length differs from the table's configured length.
    CidLength {
        /// Configured length.
        expected: u8,
        /// Supplied length.
        got: usize,
    },
    /// A packet number in the initial state is not below 2^62.
    PacketNumberRange {
        /// Offending value.
        pn: u64,
    },
}

/// A slot handle that is out of range, vacant or from an older generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaleHandle;

/// Rejected [`crate::ConnTable::set`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetError {
    /// Stale handle.
    Stale,
    /// A packet number is not below 2^62.
    PacketNumberRange {
        /// Offending value.
        pn: u64,
    },
}

/// Rejected CID lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupError {
    /// The DCID length differs from the table's configured length.
    CidLength {
        /// Configured length.
        expected: u8,
        /// Supplied length.
        got: usize,
    },
}

/// Rejected event batch. The table is unchanged after any error.
///
/// `index` is a descriptor position in the batch. Descriptor errors
/// (`StaleHandle`, `PacketNumberRange`) are reported for the first bad
/// descriptor. Range errors are reported for the connection whose first
/// descriptor appears earliest among all connections whose net update is out
/// of range, and `index` is that first descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchError {
    /// More than [`crate::MAX_BATCH`] descriptors.
    TooLong {
        /// Supplied length.
        len: usize,
    },
    /// Descriptor names a stale or vacant slot.
    StaleHandle {
        /// Descriptor position.
        index: usize,
    },
    /// Received packet number not below 2^62.
    PacketNumberRange {
        /// Descriptor position.
        index: usize,
        /// Offending value.
        pn: u64,
    },
    /// Acked plus lost bytes exceed bytes in flight plus sent bytes.
    BytesInFlightUnderflow {
        /// First descriptor of the offending connection.
        index: usize,
    },
    /// Resulting bytes in flight exceed `u32::MAX`.
    BytesInFlightOverflow {
        /// First descriptor of the offending connection.
        index: usize,
    },
}

macro_rules! display_via_debug {
    ($($t:ty),*) => {$(
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(self, f)
            }
        }
        impl std::error::Error for $t {}
    )*};
}

display_via_debug!(
    ConfigError,
    InsertError,
    StaleHandle,
    SetError,
    LookupError,
    BatchError
);
