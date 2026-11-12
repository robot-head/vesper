//! AoSoA tile layout: 16 connections per tile, one 16-lane array per field.
//!
//! A table holds three parallel arrays of tiles indexed by tile number, so
//! that each loop only touches the cache lines it needs:
//!
//! | Array | Fields | Size | Cache lines |
//! | --- | --- | --- | --- |
//! | [`HotTile`] | `largest_rx_pn`, `next_tx_pn`, `bbr_bw`, `bbr_min_rtt`, `next_edt` (`u64` x16 each), `cwnd`, `bytes_in_flight` (`u32` x16 each) | 768 B | 12 |
//! | [`KeyTile`] | local CID words (`u32` x16 x5), occupancy mask | 384 B | 6 |
//! | [`ColdTile`] | idle deadline (`u64` x16), generation (`u32` x16), peer CID (20 B x16), peer CID length | 576 B | 9 |
//!
//! Every `u64` field array is exactly two 64-byte lines and every `u32` field
//! array exactly one, all starting on a line boundary (asserted below). An
//! 8-byte local CID lookup reads two key lines (words 0 and 1) plus the line
//! holding the occupancy mask; an idle scan reads two cold lines and the
//! occupancy line. The total is 1728 bytes per 16 connections (108 B each).

use crate::cid::CID_WORDS;
use crate::simd::LANES;
use core::mem::{align_of, offset_of, size_of};

/// Cache-line size assumed by the layout.
pub const CACHE_LINE: usize = 64;

/// Hot per-packet congestion and packet-number state for 16 connections.
#[derive(Clone, Copy, Debug)]
#[repr(C, align(64))]
pub struct HotTile {
    /// Largest received packet number plus one; 0 means none received.
    pub largest_rx_pn_p1: [u64; LANES],
    /// Next packet number to send.
    pub next_tx_pn: [u64; LANES],
    /// BBR bottleneck bandwidth estimate, bytes per second.
    pub bbr_bw_bytes_per_sec: [u64; LANES],
    /// BBR minimum RTT estimate, nanoseconds.
    pub bbr_min_rtt_ns: [u64; LANES],
    /// Earliest departure time for the next packet, nanoseconds.
    pub next_edt_ns: [u64; LANES],
    /// Congestion window, bytes.
    pub cwnd_bytes: [u32; LANES],
    /// Bytes currently in flight.
    pub bytes_in_flight: [u32; LANES],
}

/// Local connection-ID routing keys for 16 connections.
#[derive(Clone, Copy, Debug)]
#[repr(C, align(64))]
pub struct KeyTile {
    /// `cid_words[w][lane]` is little-endian word `w` of the lane's local CID,
    /// zero padded. Only `ceil(cid_len / 4)` rows are compared.
    pub cid_words: [[u32; LANES]; CID_WORDS],
    /// Bit `lane` is set iff the lane holds a live connection.
    pub occupied: u16,
}

/// Rarely touched per-connection state for 16 connections.
#[derive(Clone, Copy, Debug)]
#[repr(C, align(64))]
pub struct ColdTile {
    /// Idle-timeout deadline, nanoseconds.
    pub idle_deadline_ns: [u64; LANES],
    /// Slot generation, bumped every time the slot is vacated.
    pub generation: [u32; LANES],
    /// Peer CID bytes (the DCID we put on outgoing packets), zero padded.
    pub peer_cid: [[u8; 20]; LANES],
    /// Peer CID lengths.
    pub peer_cid_len: [u8; LANES],
}

impl HotTile {
    pub(crate) const EMPTY: HotTile = HotTile {
        largest_rx_pn_p1: [0; LANES],
        next_tx_pn: [0; LANES],
        bbr_bw_bytes_per_sec: [0; LANES],
        bbr_min_rtt_ns: [0; LANES],
        next_edt_ns: [0; LANES],
        cwnd_bytes: [0; LANES],
        bytes_in_flight: [0; LANES],
    };
}

impl KeyTile {
    pub(crate) const EMPTY: KeyTile = KeyTile {
        cid_words: [[0; LANES]; CID_WORDS],
        occupied: 0,
    };
}

impl ColdTile {
    pub(crate) const EMPTY: ColdTile = ColdTile {
        idle_deadline_ns: [0; LANES],
        generation: [0; LANES],
        peer_cid: [[0; 20]; LANES],
        peer_cid_len: [0; LANES],
    };
}

/// Bytes of tile storage per tile (all three arrays).
pub const TILE_BYTES: usize = size_of::<HotTile>() + size_of::<KeyTile>() + size_of::<ColdTile>();

const _: () = {
    assert!(size_of::<HotTile>() == 768);
    assert!(align_of::<HotTile>() == CACHE_LINE);
    assert!(offset_of!(HotTile, largest_rx_pn_p1) == 0);
    assert!(offset_of!(HotTile, next_tx_pn) == 128);
    assert!(offset_of!(HotTile, bbr_bw_bytes_per_sec) == 256);
    assert!(offset_of!(HotTile, bbr_min_rtt_ns) == 384);
    assert!(offset_of!(HotTile, next_edt_ns) == 512);
    assert!(offset_of!(HotTile, cwnd_bytes) == 640);
    assert!(offset_of!(HotTile, bytes_in_flight) == 704);

    assert!(size_of::<KeyTile>() == 384);
    assert!(align_of::<KeyTile>() == CACHE_LINE);
    assert!(offset_of!(KeyTile, cid_words) == 0);
    assert!(offset_of!(KeyTile, occupied) == 320);

    assert!(size_of::<ColdTile>() == 576);
    assert!(align_of::<ColdTile>() == CACHE_LINE);
    assert!(offset_of!(ColdTile, idle_deadline_ns) == 0);
    assert!(offset_of!(ColdTile, generation) == 128);
    assert!(offset_of!(ColdTile, peer_cid) == 192);
    assert!(offset_of!(ColdTile, peer_cid_len) == 512);

    assert!(TILE_BYTES == 1728);
};
