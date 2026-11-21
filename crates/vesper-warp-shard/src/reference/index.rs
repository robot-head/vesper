//! Open-addressed CID -> slot index with linear probing and backward-shift
//! deletion (no tombstones).
//!
//! Each entry is 8 bytes: the low 32 bits of the CID's keyed hash and the slot
//! number. The bucket count is the next power of two at or above twice the
//! table capacity, so the load factor never exceeds 1/2 and every probe
//! sequence terminates at an empty bucket. Keys are verified against the
//! tile storage, so hash collisions cost an extra compare, never a wrong hit.
//! The hasher is a caller-chosen [`BuildHasher`]; the default `RandomState`
//! (SipHash-1-3 with random keys) keeps probe lengths independent of
//! attacker-chosen CIDs.

use std::hash::BuildHasher;

const EMPTY: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    hash: u32,
    slot: u32,
}

const VACANT: Entry = Entry {
    hash: 0,
    slot: EMPTY,
};

#[derive(Debug)]
pub(crate) struct CidIndex<S> {
    entries: Box<[Entry]>,
    mask: usize,
    hasher: S,
}

impl<S: BuildHasher> CidIndex<S> {
    /// Index for up to `capacity` keys. `capacity` is at most 2^28, checked by
    /// the table, so the doubled power of two fits comfortably in `usize`.
    pub(crate) fn new(capacity: usize, hasher: S) -> CidIndex<S> {
        let buckets = (capacity * 2).next_power_of_two().max(2);
        CidIndex {
            entries: vec![VACANT; buckets].into_boxed_slice(),
            mask: buckets - 1,
            hasher,
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        self.entries.len() * size_of::<Entry>()
    }

    /// Low 32 bits of the keyed hash; truncation is intended (the bucket mask
    /// is below 2^30).
    pub(crate) fn hash(&self, key: &[u8]) -> u32 {
        self.hasher.hash_one(key) as u32
    }

    fn home(&self, hash: u32) -> usize {
        hash as usize & self.mask
    }

    /// Finds the slot whose key satisfies `is_key`, probing from `hash`.
    pub(crate) fn find(&self, hash: u32, mut is_key: impl FnMut(u32) -> bool) -> Option<u32> {
        let mut i = self.home(hash);
        loop {
            let e = self.entries[i];
            if e.slot == EMPTY {
                return None;
            }
            if e.hash == hash && is_key(e.slot) {
                return Some(e.slot);
            }
            i = (i + 1) & self.mask;
        }
    }

    /// Inserts a key known to be absent.
    pub(crate) fn insert(&mut self, hash: u32, slot: u32) {
        let mut i = self.home(hash);
        while self.entries[i].slot != EMPTY {
            i = (i + 1) & self.mask;
        }
        self.entries[i] = Entry { hash, slot };
    }

    fn position(&self, hash: u32, slot: u32) -> Option<usize> {
        let mut i = self.home(hash);
        loop {
            let e = self.entries[i];
            if e.slot == EMPTY {
                return None;
            }
            if e.slot == slot {
                return Some(i);
            }
            i = (i + 1) & self.mask;
        }
    }

    /// Points an existing entry at a new slot (compaction).
    pub(crate) fn relocate(&mut self, hash: u32, old: u32, new: u32) -> bool {
        match self.position(hash, old) {
            Some(i) => {
                self.entries[i].slot = new;
                true
            }
            None => false,
        }
    }

    /// Removes the entry for `slot` and closes the gap by backward shifting.
    pub(crate) fn remove(&mut self, hash: u32, slot: u32) -> bool {
        let Some(mut hole) = self.position(hash, slot) else {
            return false;
        };
        let mut j = hole;
        loop {
            j = (j + 1) & self.mask;
            let e = self.entries[j];
            if e.slot == EMPTY {
                break;
            }
            let home = self.home(e.hash);
            // Entry `j` may fill the hole unless its home lies cyclically in
            // (hole, j], in which case moving it would skip its home.
            let dist_home = j.wrapping_sub(home) & self.mask;
            let dist_hole = j.wrapping_sub(hole) & self.mask;
            if dist_home >= dist_hole {
                self.entries[hole] = e;
                hole = j;
            }
        }
        self.entries[hole] = VACANT;
        true
    }

    /// Longest probe sequence length currently present (diagnostics).
    pub(crate) fn max_probe(&self) -> usize {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.slot != EMPTY)
            .map(|(i, e)| (i.wrapping_sub(self.home(e.hash)) & self.mask) + 1)
            .max()
            .unwrap_or(0)
    }
}
