//! Fixed-capacity connection table over AoSoA tiles.

use crate::cid::ConnectionId;
use crate::simd::{IsaPath, LANES};

#[cfg(feature = "vesper-port")]
pub use crate::port::table::ConnTable;
#[cfg(not(feature = "vesper-port"))]
pub use crate::reference::table::ConnTable;

/// Largest supported table capacity (2^26 connections).
pub const MAX_CAPACITY: u32 = 1 << 26;

/// Exclusive upper bound for QUIC packet numbers (RFC 9000 §12.3).
pub const PN_LIMIT: u64 = 1 << 62;

/// Table construction parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableConfig {
    /// Maximum number of live connections.
    pub capacity: u32,
    /// Length in bytes of every local CID (1..=20). QUIC short headers do not
    /// carry the DCID length, so a server routes with one fixed length.
    pub cid_len: u8,
    /// Kernel path; `None` selects [`IsaPath::detect`].
    pub isa: Option<IsaPath>,
}

impl TableConfig {
    /// Config with runtime ISA detection.
    pub fn new(capacity: u32, cid_len: u8) -> TableConfig {
        TableConfig {
            capacity,
            cid_len,
            isa: None,
        }
    }

    /// Same config with an explicit ISA path.
    pub fn with_isa(self, isa: IsaPath) -> TableConfig {
        TableConfig {
            isa: Some(isa),
            ..self
        }
    }
}

/// Generation-checked reference to a table slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotHandle {
    index: u32,
    generation: u32,
}

impl SlotHandle {
    /// Slot number (`tile * 16 + lane`).
    pub fn index(self) -> u32 {
        self.index
    }

    /// Generation the handle was minted with.
    pub fn generation(self) -> u32 {
        self.generation
    }

    /// Tile number.
    pub fn tile(self) -> usize {
        self.index as usize / LANES
    }

    /// Lane within the tile.
    pub fn lane(self) -> usize {
        self.index as usize % LANES
    }

    /// A handle from raw parts (for tests and adapters); it is checked on use.
    pub fn from_raw(index: u32, generation: u32) -> SlotHandle {
        SlotHandle { index, generation }
    }
}

/// Value snapshot of one connection's tile fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnState {
    /// Largest received packet number, if any.
    pub largest_rx_pn: Option<u64>,
    /// Next packet number to send (at most 2^62, meaning exhausted).
    pub next_tx_pn: u64,
    /// BBR bandwidth estimate, bytes per second.
    pub bbr_bw_bytes_per_sec: u64,
    /// BBR min-RTT estimate, nanoseconds.
    pub bbr_min_rtt_ns: u64,
    /// Congestion window, bytes.
    pub cwnd_bytes: u32,
    /// Bytes in flight.
    pub bytes_in_flight: u32,
    /// Earliest departure time for the next packet, nanoseconds.
    pub next_edt_ns: u64,
    /// Idle-timeout deadline, nanoseconds.
    pub idle_deadline_ns: u64,
    /// Peer CID (the DCID of packets we send).
    pub peer_cid: ConnectionId,
}

impl ConnState {
    /// A fresh connection: nothing received or in flight.
    pub fn new(peer_cid: ConnectionId, cwnd_bytes: u32, idle_deadline_ns: u64) -> ConnState {
        ConnState {
            largest_rx_pn: None,
            next_tx_pn: 0,
            bbr_bw_bytes_per_sec: 0,
            bbr_min_rtt_ns: u64::MAX,
            cwnd_bytes,
            bytes_in_flight: 0,
            next_edt_ns: 0,
            idle_deadline_ns,
            peer_cid,
        }
    }

    #[cfg(not(feature = "vesper-port"))]
    pub(crate) fn check_pns(&self) -> Result<(), u64> {
        if let Some(pn) = self.largest_rx_pn
            && pn >= PN_LIMIT
        {
            return Err(pn);
        }
        if self.next_tx_pn > PN_LIMIT {
            return Err(self.next_tx_pn);
        }
        Ok(())
    }
}

/// A connection moved by [`ConnTable::compact`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// Handle that is now stale.
    pub from: SlotHandle,
    /// Replacement handle.
    pub to: SlotHandle,
}

/// Result of one [`ConnTable::scan_idle`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleScan {
    /// Number of handles written to the output buffer.
    pub written: usize,
    /// Slot to resume from when the buffer filled up; `None` when the scan
    /// reached the end of the table.
    pub resume_at: Option<u32>,
}
