//! The types of the export ABI (`--rust-types crate::port::types`), the
//! Rust mirrors of `vesper/src/types.ves`.

/// A local CID as five little-endian 32-bit words, zero padded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CidKey {
    pub w0: u64,
    pub w1: u64,
    pub w2: u64,
    pub w3: u64,
    pub w4: u64,
}

/// `ConnState` as words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreState {
    pub rx_p1: u64,
    pub next_tx_pn: u64,
    pub bbr_bw: u64,
    pub bbr_min_rtt_ns: u64,
    pub cwnd_bytes: u64,
    pub bytes_in_flight: u64,
    pub next_edt_ns: u64,
    pub idle_deadline_ns: u64,
    pub peer0: u64,
    pub peer1: u64,
    pub peer2: u64,
    pub peer_len: u64,
}

/// Why a table call refused its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreFault {
    ZeroCapacity,
    CapacityTooLarge(u64),
    CidLength(u64),
    Full,
    DuplicateCid,
    PacketNumberRange(u64),
    Stale,
}

/// Why a batch was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreBatchFault {
    TooLong(u64),
    StaleHandle(u64),
    PacketNumberRange { index: u64, pn: u64 },
    BytesInFlightUnderflow(u64),
    BytesInFlightOverflow(u64),
}
