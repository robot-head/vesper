//! The Vesper build of [`ConnTable`]: the tiles, the CID index and every
//! table operation run in `vesper/src/table*.ves` (`TableCore`). This
//! wrapper hashes CIDs with the caller's `BuildHasher`, keeps the kernel
//! path and converts between the crate's types and words.

use crate::cid::{CID_WORDS, ConnectionId, MAX_CID_LEN};
use crate::error::{ConfigError, InsertError, LookupError, RekeyError, SetError, StaleHandle};
use crate::port::binding::TableCore;
use crate::port::types::{CidKey, CoreFault, CoreState};
use crate::simd::{IsaPath, Kernels};
use crate::table::{ConnState, IdleScan, Relocation, SlotHandle, TableConfig};
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::BuildHasher;

/// Fixed-capacity table of QUIC connections in 16-lane tiles.
///
/// All storage is allocated by [`ConnTable::new`]; insert, remove, lookup,
/// batch update, idle scan and compaction never allocate.
pub struct ConnTable<S = RandomState> {
    pub(crate) core: TableCore,
    capacity: u32,
    cid_len: u8,
    pub(crate) kernels: Kernels,
    hasher: S,
}

impl<S> fmt::Debug for ConnTable<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnTable")
            .field("capacity", &self.capacity)
            .field("cid_len", &self.cid_len)
            .field("len", &self.core.len())
            .field("kernels", &self.kernels)
            .finish_non_exhaustive()
    }
}

pub(crate) fn key(cid: &ConnectionId) -> CidKey {
    let w = cid.words();
    CidKey {
        w0: u64::from(w[0]),
        w1: u64::from(w[1]),
        w2: u64::from(w[2]),
        w3: u64::from(w[3]),
        w4: u64::from(w[4]),
    }
}

fn cid_of(k: CidKey, len: u8) -> ConnectionId {
    // The words hold u32 values (Vesper stores them as u32).
    let words: [u32; CID_WORDS] = [k.w0, k.w1, k.w2, k.w3, k.w4].map(|w| w as u32);
    ConnectionId::from_words(&words, len)
}

fn core_state(s: &ConnState) -> CoreState {
    let mut peer = [0u8; MAX_CID_LEN];
    peer[..s.peer_cid.len()].copy_from_slice(s.peer_cid.as_bytes());
    let (p0, rest) = peer.split_first_chunk::<8>().expect("20 bytes");
    let (p1, p2) = rest.split_first_chunk::<8>().expect("12 bytes");
    let p2: [u8; 4] = *p2.first_chunk::<4>().expect("4 bytes");
    CoreState {
        // `pn + 1` cannot overflow below 2^62; a larger pn is rejected by
        // Vesper (rx_p1 > 2^62), and `u64::MAX` saturates so it still is.
        rx_p1: s.largest_rx_pn.map_or(0, |pn| pn.saturating_add(1)),
        next_tx_pn: s.next_tx_pn,
        bbr_bw: s.bbr_bw_bytes_per_sec,
        bbr_min_rtt_ns: s.bbr_min_rtt_ns,
        cwnd_bytes: u64::from(s.cwnd_bytes),
        bytes_in_flight: u64::from(s.bytes_in_flight),
        next_edt_ns: s.next_edt_ns,
        idle_deadline_ns: s.idle_deadline_ns,
        peer0: u64::from_le_bytes(*p0),
        peer1: u64::from_le_bytes(*p1),
        peer2: u64::from(u32::from_le_bytes(p2)),
        peer_len: s.peer_cid.len() as u64,
    }
}

fn conn_state(c: CoreState) -> ConnState {
    let mut peer = [0u8; MAX_CID_LEN];
    peer[..8].copy_from_slice(&c.peer0.to_le_bytes());
    peer[8..16].copy_from_slice(&c.peer1.to_le_bytes());
    // Vesper stores peer2 as u32.
    peer[16..20].copy_from_slice(&(c.peer2 as u32).to_le_bytes());
    let len = (c.peer_len as usize).min(MAX_CID_LEN);
    ConnState {
        largest_rx_pn: c.rx_p1.checked_sub(1),
        next_tx_pn: c.next_tx_pn,
        bbr_bw_bytes_per_sec: c.bbr_bw,
        bbr_min_rtt_ns: c.bbr_min_rtt_ns,
        // Stored as u32 by Vesper.
        cwnd_bytes: c.cwnd_bytes as u32,
        bytes_in_flight: c.bytes_in_flight as u32,
        next_edt_ns: c.next_edt_ns,
        idle_deadline_ns: c.idle_deadline_ns,
        peer_cid: ConnectionId::from_slice(&peer[..len]).unwrap_or(ConnectionId::EMPTY),
    }
}

/// A handle from Vesper's `(index, generation)` words (both below 2^32).
fn handle((index, generation): (u64, u64)) -> SlotHandle {
    SlotHandle::from_raw(index as u32, generation as u32)
}

fn pn_of(e: CoreFault) -> u64 {
    match e {
        CoreFault::PacketNumberRange(pn) => pn,
        _ => 0,
    }
}

impl ConnTable<RandomState> {
    /// Table with a randomly keyed SipHash index.
    pub fn new(config: TableConfig) -> Result<ConnTable<RandomState>, ConfigError> {
        ConnTable::with_hasher(config, RandomState::new())
    }
}

/// The core's "no slot" index.
const NO_INDEX: u64 = u64::MAX;

impl<S: BuildHasher> ConnTable<S> {
    /// Table with a caller-supplied index hasher.
    pub fn with_hasher(config: TableConfig, hasher: S) -> Result<ConnTable<S>, ConfigError> {
        // The configuration checks come first, as in the Rust build.
        if config.capacity == 0 {
            return Err(ConfigError::ZeroCapacity);
        }
        if config.capacity > crate::table::MAX_CAPACITY {
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
        let core = TableCore::new(u64::from(config.capacity), u64::from(config.cid_len))
            .map_err(|_| ConfigError::ZeroCapacity)?;
        Ok(ConnTable {
            core,
            capacity: config.capacity,
            cid_len: config.cid_len,
            kernels,
            hasher,
        })
    }

    fn hash(&self, bytes: &[u8]) -> u64 {
        // Low 32 bits of the keyed hash, as the Rust build's index keeps.
        u64::from(self.hasher.hash_one(bytes) as u32)
    }

    fn vector(kernels: Kernels) -> bool {
        kernels.path() != IsaPath::Scalar
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
        // At most the capacity, below 2^27.
        self.core.len() as u32
    }

    /// Whether no connection is live.
    pub fn is_empty(&self) -> bool {
        self.core.len() == 0
    }

    /// Configured local CID length.
    pub fn cid_len(&self) -> u8 {
        self.cid_len
    }

    /// Number of tiles.
    pub fn tiles(&self) -> usize {
        self.core.tiles() as usize
    }

    /// Bytes of heap storage owned by the table (its Vesper sequences at
    /// their element strides).
    pub fn memory_bytes(&self) -> usize {
        self.core.memory_bytes() as usize
    }

    /// Longest probe sequence in the CID index.
    pub fn index_max_probe(&self) -> usize {
        self.core.index_max_probe() as usize
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
        let hash = self.hash(local_cid.as_bytes());
        match self.core.insert(hash, key(local_cid), core_state(state)) {
            Ok(h) => Ok(handle(h)),
            Err(CoreFault::DuplicateCid) => Err(InsertError::DuplicateCid),
            Err(CoreFault::Full) => Err(InsertError::Full),
            Err(e) => Err(InsertError::PacketNumberRange { pn: pn_of(e) }),
        }
    }

    /// Snapshot of a connection.
    pub fn get(&self, h: SlotHandle) -> Result<ConnState, StaleHandle> {
        self.core
            .get(u64::from(h.index()), u64::from(h.generation()))
            .map(conn_state)
            .ok_or(StaleHandle)
    }

    /// Local CID of a connection.
    pub fn local_cid(&self, h: SlotHandle) -> Result<ConnectionId, StaleHandle> {
        self.core
            .local_cid(u64::from(h.index()), u64::from(h.generation()))
            .map(|k| cid_of(k, self.cid_len))
            .ok_or(StaleHandle)
    }

    /// Overwrites a connection's state (the local CID is unchanged).
    pub fn set(&mut self, h: SlotHandle, state: &ConnState) -> Result<(), SetError> {
        match self.core.set(
            u64::from(h.index()),
            u64::from(h.generation()),
            core_state(state),
        ) {
            None => Ok(()),
            Some(CoreFault::Stale) => Err(SetError::Stale),
            Some(e) => Err(SetError::PacketNumberRange { pn: pn_of(e) }),
        }
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
        let old = self.local_cid(h).map_err(|_| RekeyError::Stale)?;
        if new_cid.len() != usize::from(self.cid_len) {
            return Err(RekeyError::CidLength {
                expected: self.cid_len,
                got: new_cid.len(),
            });
        }
        if old == *new_cid {
            return Ok(old);
        }
        let new_hash = self.hash(new_cid.as_bytes());
        let old_hash = self.hash(old.as_bytes());
        match self
            .core
            .rekey(u64::from(h.index()), new_hash, key(new_cid), old_hash)
        {
            None => Ok(old),
            Some(_) => Err(RekeyError::DuplicateCid),
        }
    }

    /// Removes a connection; its handle and all copies become stale.
    pub fn remove(&mut self, h: SlotHandle) -> Result<(ConnectionId, ConnState), StaleHandle> {
        let cid = self.local_cid(h)?;
        let hash = self.hash(cid.as_bytes());
        self.core
            .remove(u64::from(h.index()), u64::from(h.generation()), hash)
            .map(|s| (cid, conn_state(s)))
            .ok_or(StaleHandle)
    }

    /// Looks up a DCID through the hash index (O(1) average).
    pub fn lookup(&self, dcid: &[u8]) -> Result<Option<SlotHandle>, LookupError> {
        self.check_len(dcid.len())?;
        // Length checked equal to cid_len <= 20.
        let k = key(&ConnectionId::from_slice(dcid).unwrap_or(ConnectionId::EMPTY));
        Ok(self.core.lookup(self.hash(dcid), k).map(handle))
    }

    /// `lookup_cid_simd`: mask of the lanes of `tile` whose local CID equals
    /// `key`, compared 16 lanes at a time with the table's kernels.
    pub fn tile_match_mask(&self, kernels: Kernels, tile: usize, key: &[u32; CID_WORDS]) -> u16 {
        let k = CidKey {
            w0: u64::from(key[0]),
            w1: u64::from(key[1]),
            w2: u64::from(key[2]),
            w3: u64::from(key[3]),
            w4: u64::from(key[4]),
        };
        // A 16-lane mask.
        self.core
            .tile_match_mask(tile as u64, k, Self::vector(kernels)) as u16
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
        let k = key(&ConnectionId::from_slice(dcid).unwrap_or(ConnectionId::EMPTY));
        Ok(self.core.lookup_scan(k, Self::vector(kernels)).map(handle))
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
        let vector = Self::vector(kernels);
        // The core writes packed handles into a stack buffer lent in place,
        // a chunk of `out` at a time.
        let mut buf = [0u64; 64];
        let mut written = 0;
        let mut from = u64::from(start);
        loop {
            let room = (out.len() - written).min(buf.len());
            let (n, resume) = self.core.scan_idle(now_ns, from, &mut buf[..room], vector);
            let n = n as usize;
            for (dst, &packed) in out[written..written + n].iter_mut().zip(&buf[..n]) {
                // Indices and generations are below 2^32.
                *dst = SlotHandle::from_raw((packed >> 32) as u32, packed as u32);
            }
            written += n;
            if resume == NO_INDEX {
                return IdleScan {
                    written,
                    resume_at: None,
                };
            }
            if written == out.len() {
                return IdleScan {
                    written,
                    // Slot indices are below 2^32.
                    resume_at: Some(resume as u32),
                };
            }
            from = resume;
        }
    }

    /// Moves live connections into the lowest slots so tiles are dense.
    /// `on_move` sees every relocation; old handles become stale. Returns the
    /// number of connections moved.
    pub fn compact(&mut self, mut on_move: impl FnMut(Relocation)) -> usize {
        let mut moved = 0;
        self.core.compact_begin();
        while let Some((from, to)) = self.core.compact_next() {
            let cid = cid_of(self.core.key_at(from), self.cid_len);
            let hash = self.hash(cid.as_bytes());
            let (old_gen, new_gen) = self.core.compact_move(from, to, hash);
            on_move(Relocation {
                from: handle((from, old_gen)),
                to: handle((to, new_gen)),
            });
            moved += 1;
        }
        self.core.compact_finish();
        moved
    }

    /// Handles of all live connections in slot order (test/diagnostic helper;
    /// writes into `out` and returns the count written).
    pub fn live_handles(&self, out: &mut [SlotHandle]) -> usize {
        let mut n = 0;
        let mut from = 0;
        while n < out.len() {
            let Some(hit) = self.core.next_live(from) else {
                break;
            };
            out[n] = handle(hit);
            n += 1;
            from = hit.0 + 1;
        }
        n
    }
}
