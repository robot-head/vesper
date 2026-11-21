//! Batched per-connection event application.
//!
//! A batch of up to [`MAX_BATCH`] descriptors is applied atomically. For each
//! connection named in the batch:
//!
//! - `largest_rx_pn' = max(largest_rx_pn, every PacketReceived pn)`;
//! - `bytes_in_flight' = bytes_in_flight + sum(sent) - sum(acked) - sum(lost)`,
//!   which must lie in `0..=u32::MAX` (only the net result is checked, so the
//!   order of events within a batch does not matter);
//! - `cwnd' =` the last `CwndUpdate` for that connection, if any.
//!
//! If any descriptor or any connection's net result is invalid, the batch is
//! rejected with a [`BatchError`] and the table is unchanged.
//!
//! [`ConnTable::apply_batch_scalar`] is the reference: it aggregates per
//! connection with plain integer code. [`ConnTable::apply_batch`] scatters the
//! batch into per-tile 16-lane staging arrays and then applies each touched
//! tile with the vector kernels (widened 64-bit add/sub, unsigned compare for
//! the range check, unsigned max for packet numbers). The scatter itself is
//! scalar because descriptors address arbitrary slots.

use crate::table::SlotHandle;

/// Largest batch accepted by [`ConnTable::apply_batch`].
pub const MAX_BATCH: usize = 64;

/// One connection event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnEvent {
    /// A packet with this (fully decoded) number was received.
    PacketReceived {
        /// Packet number, must be below 2^62.
        pn: u64,
    },
    /// An in-flight packet of this size was sent.
    PacketSent {
        /// Bytes.
        bytes: u32,
    },
    /// In-flight bytes were acknowledged.
    BytesAcked {
        /// Bytes.
        bytes: u32,
    },
    /// In-flight bytes were declared lost.
    BytesLost {
        /// Bytes.
        bytes: u32,
    },
    /// The congestion controller set a new window.
    CwndUpdate {
        /// New window.
        cwnd_bytes: u32,
    },
}

/// A (slot, event) descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnEventDesc {
    /// Target connection.
    pub slot: SlotHandle,
    /// Event.
    pub event: ConnEvent,
}
