//! Fixed-capacity connection table over AoSoA tiles.

use crate::cid::{CID_WORDS, ConnectionId, MAX_CID_LEN};
use crate::error::{ConfigError, InsertError, LookupError, RekeyError, SetError, StaleHandle};
use crate::index::CidIndex;
use crate::simd::{IsaPath, Kernels, LANES};
use crate::tile::{ColdTile, HotTile, KeyTile, TILE_BYTES};
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;

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

    fn check_pns(&self) -> Result<(), u64> {
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

/// Per-tile staging lanes for one batch (see `batch.rs`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stage {
    pub(crate) tile: u32,
    pub(crate) touched: u16,
    pub(crate) cwnd_set: u16,
    pub(crate) first: [u8; LANES],
    pub(crate) sent: [u64; LANES],
    pub(crate) released: [u64; LANES],
    pub(crate) max_pn_p1: [u64; LANES],
    pub(crate) result_bif: [u64; LANES],
    pub(crate) cwnd: [u32; LANES],
}

impl Stage {
    pub(crate) const EMPTY: Stage = Stage {
        tile: 0,
        touched: 0,
        cwnd_set: 0,
        first: [0; LANES],
        sent: [0; LANES],
        released: [0; LANES],
        max_pn_p1: [0; LANES],
        result_bif: [0; LANES],
        cwnd: [0; LANES],
    };
}

pub(crate) const NO_STAGE: u8 = u8::MAX;

/// Fixed-capacity table of QUIC connections in 16-lane tiles.
///
/// All storage is allocated by [`ConnTable::new`]; insert, remove, lookup,
/// batch update, idle scan and compaction never allocate.
#[derive(Debug)]
pub struct ConnTable<S = RandomState> {
    pub(crate) capacity: u32,
    pub(crate) cid_len: u8,
    pub(crate) cid_words: usize,
    pub(crate) kernels: Kernels,
    pub(crate) hot: Box<[HotTile]>,
    pub(crate) keys: Box<[KeyTile]>,
    pub(crate) cold: Box<[ColdTile]>,
    free: Box<[u32]>,
    free_len: u32,
    len: u32,
    index: CidIndex<S>,
    pub(crate) stages: Box<[Stage]>,
    pub(crate) tile_stage: Box<[u8]>,
}

impl ConnTable<RandomState> {
    /// Table with a randomly keyed SipHash index.
    pub fn new(config: TableConfig) -> Result<ConnTable<RandomState>, ConfigError> {
        ConnTable::with_hasher(config, RandomState::new())
    }
}

impl<S: BuildHasher> ConnTable<S> {
    /// Table with a caller-supplied index hasher.
    pub fn with_hasher(config: TableConfig, hasher: S) -> Result<ConnTable<S>, ConfigError> {
        if config.capacity == 0 {
            return Err(ConfigError::ZeroCapacity);
        }
        if config.capacity > MAX_CAPACITY {
            return Err(ConfigError::CapacityTooLarge {
                requested: config.capacity,
            });
        }
        if config.cid_len == 0 || usize::from(config.cid_len) > MAX_CID_LEN {
            return Err(ConfigError::CidLength {
                len: config.cid_len,
            });
        }
        let kernels = match config.isa {
            None => Kernels::detect(),
            Some(p) => Kernels::new(p).map_err(ConfigError::Isa)?,
        };
        let capacity = config.capacity as usize;
        let tiles = capacity.div_ceil(LANES);
        // Free stack pops the lowest slot first.
        let free: Box<[u32]> = (0..config.capacity).rev().collect();
        Ok(ConnTable {
            capacity: config.capacity,
            cid_len: config.cid_len,
            cid_words: usize::from(config.cid_len).div_ceil(4),
            kernels,
            hot: vec![HotTile::EMPTY; tiles].into_boxed_slice(),
            keys: vec![KeyTile::EMPTY; tiles].into_boxed_slice(),
            cold: vec![ColdTile::EMPTY; tiles].into_boxed_slice(),
            free,
            free_len: config.capacity,
            len: 0,
            index: CidIndex::new(capacity, hasher),
            stages: vec![Stage::EMPTY; crate::batch::MAX_BATCH].into_boxed_slice(),
            tile_stage: vec![NO_STAGE; tiles].into_boxed_slice(),
        })
    }

    /// Kernel path in use.
    pub fn isa(&self) -> IsaPath {
        self.kernels.path()
    }

    /// Kernel dispatcher in use.
    pub fn kernels(&self) -> Kernels {
        self.kernels
    }

    /// Maximum live connections.
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Live connections.
    pub fn len(&self) -> u32 {
        self.len
    }

    /// Whether no connection is live.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Configured local CID length.
    pub fn cid_len(&self) -> u8 {
        self.cid_len
    }

    /// Number of tiles.
    pub fn tiles(&self) -> usize {
        self.hot.len()
    }

    /// Bytes of heap storage owned by the table (exact for its boxed arrays).
    pub fn memory_bytes(&self) -> usize {
        self.hot.len() * TILE_BYTES
            + self.free.len() * size_of::<u32>()
            + self.index.bytes()
            + self.stages.len() * size_of::<Stage>()
            + self.tile_stage.len()
    }

    /// Longest probe sequence in the CID index.
    pub fn index_max_probe(&self) -> usize {
        self.index.max_probe()
    }

    fn split(index: u32) -> (usize, usize) {
        (index as usize / LANES, index as usize % LANES)
    }

    fn occupied(&self, index: u32) -> bool {
        let (t, l) = Self::split(index);
        self.keys[t].occupied & (1 << l) != 0
    }

    pub(crate) fn check(&self, h: SlotHandle) -> Result<(usize, usize), StaleHandle> {
        if h.index >= self.capacity || !self.occupied(h.index) {
            return Err(StaleHandle);
        }
        let (t, l) = Self::split(h.index);
        if self.cold[t].generation[l] != h.generation {
            return Err(StaleHandle);
        }
        Ok((t, l))
    }

    fn handle_at(&self, index: u32) -> SlotHandle {
        let (t, l) = Self::split(index);
        SlotHandle {
            index,
            generation: self.cold[t].generation[l],
        }
    }

    fn check_len(&self, got: usize) -> Result<(), LookupError> {
        if got == usize::from(self.cid_len) {
            Ok(())
        } else {
            Err(LookupError::CidLength {
                expected: self.cid_len,
                got,
            })
        }
    }

    fn lane_matches(&self, index: u32, key: &[u32; CID_WORDS]) -> bool {
        let (t, l) = Self::split(index);
        let k = &self.keys[t];
        k.occupied & (1 << l) != 0 && (0..self.cid_words).all(|w| k.cid_words[w][l] == key[w])
    }

    /// Inserts a connection under `local_cid`.
    pub fn insert(
        &mut self,
        local_cid: &ConnectionId,
        state: &ConnState,
    ) -> Result<SlotHandle, InsertError> {
        if local_cid.len() != usize::from(self.cid_len) {
            return Err(InsertError::CidLength {
                expected: self.cid_len,
                got: local_cid.len(),
            });
        }
        state
            .check_pns()
            .map_err(|pn| InsertError::PacketNumberRange { pn })?;
        let key = local_cid.words();
        let hash = self.index.hash(local_cid.as_bytes());
        if self
            .index
            .find(hash, |s| self.lane_matches(s, &key))
            .is_some()
        {
            return Err(InsertError::DuplicateCid);
        }
        if self.free_len == 0 {
            return Err(InsertError::Full);
        }
        self.free_len -= 1;
        let index = self.free[self.free_len as usize];
        let (t, l) = Self::split(index);
        for (w, &word) in key.iter().enumerate() {
            self.keys[t].cid_words[w][l] = word;
        }
        self.keys[t].occupied |= 1 << l;
        self.write_state(t, l, state);
        self.index.insert(hash, index);
        self.len += 1;
        Ok(self.handle_at(index))
    }

    fn write_state(&mut self, t: usize, l: usize, s: &ConnState) {
        let hot = &mut self.hot[t];
        // `pn + 1` cannot overflow: pn < 2^62 is checked by every caller.
        hot.largest_rx_pn_p1[l] = s.largest_rx_pn.map_or(0, |pn| pn + 1);
        hot.next_tx_pn[l] = s.next_tx_pn;
        hot.bbr_bw_bytes_per_sec[l] = s.bbr_bw_bytes_per_sec;
        hot.bbr_min_rtt_ns[l] = s.bbr_min_rtt_ns;
        hot.cwnd_bytes[l] = s.cwnd_bytes;
        hot.bytes_in_flight[l] = s.bytes_in_flight;
        hot.next_edt_ns[l] = s.next_edt_ns;
        let cold = &mut self.cold[t];
        cold.idle_deadline_ns[l] = s.idle_deadline_ns;
        cold.peer_cid[l] = [0; 20];
        cold.peer_cid[l][..s.peer_cid.len()].copy_from_slice(s.peer_cid.as_bytes());
        // Cannot truncate: ConnectionId length is at most 20.
        cold.peer_cid_len[l] = s.peer_cid.len() as u8;
    }

    fn read_state(&self, t: usize, l: usize) -> ConnState {
        let hot = &self.hot[t];
        let cold = &self.cold[t];
        let peer_len = usize::from(cold.peer_cid_len[l]);
        ConnState {
            largest_rx_pn: hot.largest_rx_pn_p1[l].checked_sub(1),
            next_tx_pn: hot.next_tx_pn[l],
            bbr_bw_bytes_per_sec: hot.bbr_bw_bytes_per_sec[l],
            bbr_min_rtt_ns: hot.bbr_min_rtt_ns[l],
            cwnd_bytes: hot.cwnd_bytes[l],
            bytes_in_flight: hot.bytes_in_flight[l],
            next_edt_ns: hot.next_edt_ns[l],
            idle_deadline_ns: cold.idle_deadline_ns[l],
            peer_cid: ConnectionId::from_slice(&cold.peer_cid[l][..peer_len])
                .unwrap_or(ConnectionId::EMPTY),
        }
    }

    /// Snapshot of a connection.
    pub fn get(&self, h: SlotHandle) -> Result<ConnState, StaleHandle> {
        let (t, l) = self.check(h)?;
        Ok(self.read_state(t, l))
    }

    /// Local CID of a connection.
    pub fn local_cid(&self, h: SlotHandle) -> Result<ConnectionId, StaleHandle> {
        let (t, l) = self.check(h)?;
        Ok(self.local_cid_at(t, l))
    }

    fn local_cid_at(&self, t: usize, l: usize) -> ConnectionId {
        let mut words = [0u32; CID_WORDS];
        for (w, word) in words.iter_mut().enumerate() {
            *word = self.keys[t].cid_words[w][l];
        }
        ConnectionId::from_words(&words, self.cid_len)
    }

    /// Overwrites a connection's state (the local CID is unchanged).
    pub fn set(&mut self, h: SlotHandle, state: &ConnState) -> Result<(), SetError> {
        let (t, l) = self.check(h).map_err(|_| SetError::Stale)?;
        state
            .check_pns()
            .map_err(|pn| SetError::PacketNumberRange { pn })?;
        self.write_state(t, l, state);
        Ok(())
    }

    /// Replaces a connection's local CID, keeping its slot, handle and
    /// state; returns the old CID. A QUIC endpoint uses this when the peer
    /// retires the connection ID the slot is keyed by (RFC 9000 §5.1.2):
    /// the retired ID stops routing and a live one takes its place. Never
    /// allocates. Rekeying to the current CID is a no-op.
    pub fn rekey(
        &mut self,
        h: SlotHandle,
        new_cid: &ConnectionId,
    ) -> Result<ConnectionId, RekeyError> {
        let (t, l) = self.check(h).map_err(|_| RekeyError::Stale)?;
        if new_cid.len() != usize::from(self.cid_len) {
            return Err(RekeyError::CidLength {
                expected: self.cid_len,
                got: new_cid.len(),
            });
        }
        let old = self.local_cid_at(t, l);
        if old == *new_cid {
            return Ok(old);
        }
        let key = new_cid.words();
        let new_hash = self.index.hash(new_cid.as_bytes());
        if self
            .index
            .find(new_hash, |s| self.lane_matches(s, &key))
            .is_some()
        {
            return Err(RekeyError::DuplicateCid);
        }
        let old_hash = self.index.hash(old.as_bytes());
        let removed = self.index.remove(old_hash, h.index);
        debug_assert!(removed, "index out of sync with tiles");
        for (w, &word) in key.iter().enumerate() {
            self.keys[t].cid_words[w][l] = word;
        }
        self.index.insert(new_hash, h.index);
        Ok(old)
    }

    /// Removes a connection; its handle and all copies become stale.
    pub fn remove(&mut self, h: SlotHandle) -> Result<(ConnectionId, ConnState), StaleHandle> {
        let (t, l) = self.check(h)?;
        let cid = self.local_cid_at(t, l);
        let state = self.read_state(t, l);
        let hash = self.index.hash(cid.as_bytes());
        let removed = self.index.remove(hash, h.index);
        debug_assert!(removed, "index out of sync with tiles");
        self.vacate(t, l);
        self.free[self.free_len as usize] = h.index;
        self.free_len += 1;
        self.len -= 1;
        Ok((cid, state))
    }

    fn vacate(&mut self, t: usize, l: usize) {
        self.keys[t].occupied &= !(1 << l);
        for row in &mut self.keys[t].cid_words {
            row[l] = 0;
        }
        self.write_state(t, l, &ConnState::new(ConnectionId::EMPTY, 0, 0));
        self.hot[t].bbr_min_rtt_ns[l] = 0;
        // Wrapping is intended: a handle could alias only after 2^32 reuses
        // of this one slot while the old handle is still held.
        self.cold[t].generation[l] = self.cold[t].generation[l].wrapping_add(1);
    }

    /// Looks up a DCID through the hash index (O(1) average).
    pub fn lookup(&self, dcid: &[u8]) -> Result<Option<SlotHandle>, LookupError> {
        self.check_len(dcid.len())?;
        // Length checked equal to cid_len <= 20.
        let key = ConnectionId::from_slice(dcid)
            .unwrap_or(ConnectionId::EMPTY)
            .words();
        let hash = self.index.hash(dcid);
        Ok(self
            .index
            .find(hash, |s| self.lane_matches(s, &key))
            .map(|s| self.handle_at(s)))
    }

    /// `lookup_cid_simd`: mask of the lanes of `tile` whose local CID equals
    /// `key`, compared 16 lanes at a time with the table's kernels.
    pub fn tile_match_mask(&self, kernels: Kernels, tile: usize, key: &[u32; CID_WORDS]) -> u16 {
        let k = &self.keys[tile];
        let mut mask = k.occupied;
        for (w, &word) in key.iter().enumerate().take(self.cid_words) {
            if mask == 0 {
                break;
            }
            mask &= kernels.eq_mask_u32(&k.cid_words[w], word);
        }
        mask
    }

    /// Looks up a DCID by scanning every tile with the given kernels. This is
    /// the index-free `lookup_cid_simd` of §35.3 and the reference the hash
    /// index is tested against.
    pub fn lookup_scan_with(
        &self,
        kernels: Kernels,
        dcid: &[u8],
    ) -> Result<Option<SlotHandle>, LookupError> {
        self.check_len(dcid.len())?;
        let key = ConnectionId::from_slice(dcid)
            .unwrap_or(ConnectionId::EMPTY)
            .words();
        for t in 0..self.tiles() {
            let mask = self.tile_match_mask(kernels, t, &key);
            if mask != 0 {
                // Duplicates are rejected at insert, so at most one bit is set.
                let lane = mask.trailing_zeros();
                // Cannot overflow: tiles * 16 <= capacity rounded up < 2^27.
                return Ok(Some(self.handle_at(t as u32 * LANES as u32 + lane)));
            }
        }
        Ok(None)
    }

    /// Index-free tile scan with the table's own kernels.
    pub fn lookup_cid_simd(&self, dcid: &[u8]) -> Result<Option<SlotHandle>, LookupError> {
        self.lookup_scan_with(self.kernels, dcid)
    }

    /// Index-free tile scan with scalar kernels.
    pub fn lookup_cid_scalar(&self, dcid: &[u8]) -> Result<Option<SlotHandle>, LookupError> {
        self.lookup_scan_with(Kernels::scalar(), dcid)
    }

    /// Writes handles of live connections whose idle deadline is `<= now_ns`
    /// into `out`, starting at slot `start`, using the table's kernels.
    pub fn scan_idle(&self, now_ns: u64, start: u32, out: &mut [SlotHandle]) -> IdleScan {
        self.scan_idle_with(self.kernels, now_ns, start, out)
    }

    /// [`ConnTable::scan_idle`] with explicit kernels. An empty `out` returns
    /// immediately with `resume_at = Some(start)` if any expired slot remains.
    pub fn scan_idle_with(
        &self,
        kernels: Kernels,
        now_ns: u64,
        start: u32,
        out: &mut [SlotHandle],
    ) -> IdleScan {
        let mut written = 0;
        let first_tile = start as usize / LANES;
        for t in first_tile..self.tiles() {
            let mut mask =
                kernels.le_mask_u64(&self.cold[t].idle_deadline_ns, now_ns) & self.keys[t].occupied;
            if t == first_tile {
                // Drop lanes before `start`; `start % 16 < 16` so the shift is valid.
                mask &= u16::MAX << (start as usize % LANES);
            }
            while mask != 0 {
                let lane = mask.trailing_zeros();
                let index = t as u32 * LANES as u32 + lane;
                if written == out.len() {
                    return IdleScan {
                        written,
                        resume_at: Some(index),
                    };
                }
                out[written] = self.handle_at(index);
                written += 1;
                mask &= mask - 1;
            }
        }
        IdleScan {
            written,
            resume_at: None,
        }
    }

    /// Moves live connections into the lowest slots so tiles are dense.
    /// `on_move` sees every relocation; old handles become stale. Returns the
    /// number of connections moved.
    pub fn compact(&mut self, mut on_move: impl FnMut(Relocation)) -> usize {
        let mut moved = 0;
        let mut lo = 0u32;
        let mut hi = self.capacity;
        loop {
            while lo < hi && self.occupied(lo) {
                lo += 1;
            }
            while hi > lo && !self.occupied(hi - 1) {
                hi -= 1;
            }
            if lo + 1 >= hi {
                break;
            }
            // `lo` is vacant and `hi - 1 > lo` is live.
            let from = hi - 1;
            let reloc = self.move_slot(from, lo);
            on_move(reloc);
            moved += 1;
        }
        // Live slots are exactly 0..len; rebuild the free stack in place.
        let free_count = self.capacity - self.len;
        for k in 0..free_count {
            self.free[k as usize] = self.capacity - 1 - k;
        }
        self.free_len = free_count;
        moved
    }

    fn move_slot(&mut self, from: u32, to: u32) -> Relocation {
        let old = self.handle_at(from);
        let (ft, fl) = Self::split(from);
        let (tt, tl) = Self::split(to);
        let cid = self.local_cid_at(ft, fl);
        let state = self.read_state(ft, fl);
        for w in 0..CID_WORDS {
            self.keys[tt].cid_words[w][tl] = self.keys[ft].cid_words[w][fl];
        }
        self.keys[tt].occupied |= 1 << tl;
        self.write_state(tt, tl, &state);
        self.vacate(ft, fl);
        let hash = self.index.hash(cid.as_bytes());
        let ok = self.index.relocate(hash, from, to);
        debug_assert!(ok, "index out of sync with tiles");
        Relocation {
            from: old,
            to: self.handle_at(to),
        }
    }

    /// Handles of all live connections in slot order (test/diagnostic helper;
    /// writes into `out` and returns the count written).
    pub fn live_handles(&self, out: &mut [SlotHandle]) -> usize {
        let mut n = 0;
        for t in 0..self.tiles() {
            let mut m = self.keys[t].occupied;
            while m != 0 && n < out.len() {
                let lane = m.trailing_zeros();
                out[n] = self.handle_at(t as u32 * LANES as u32 + lane);
                n += 1;
                m &= m - 1;
            }
        }
        n
    }
}
