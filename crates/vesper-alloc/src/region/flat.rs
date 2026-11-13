//! Region-backed scalar open addressing with sixteen-byte control groups.
#![forbid(unsafe_code)]

use super::{Region, RegionId, RegionVec};
use crate::{AllocError, ChunkBacking, HandleError, OwnerError, RegionError};
use std::alloc::Layout;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::iter::FusedIterator;
use std::mem::{align_of, size_of};

pub const FLAT_GROUP_WIDTH: usize = 16;
pub const HARD_MAX_FLAT_GROUPS: usize = 4096;
pub const HARD_MAX_FLAT_GROUP_BYTES: usize = 64 * 1024;
const EMPTY: u8 = 0x80;
const DELETED: u8 = 0xfe;
const LIVE_PER_GROUP: usize = 14;

/// A positive power-of-two group limit, at most [`HARD_MAX_FLAT_GROUPS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatConfig {
    pub max_groups: usize,
}

impl Default for FlatConfig {
    fn default() -> Self {
        Self { max_groups: 1024 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatError {
    InvalidMaxGroups { max_groups: usize },
    UnsupportedEntryLayout,
    CapacityLimit { requested: usize, max: usize },
    Region(RegionError),
}

impl fmt::Display for FlatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for FlatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Region(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RegionError> for FlatError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}

impl From<AllocError> for FlatError {
    fn from(error: AllocError) -> Self {
        Self::Region(error.into())
    }
}

/// A refused insertion returns the actual incoming key and value unchanged.
pub struct FlatInsertError<K, V> {
    pub value: (K, V),
    pub error: FlatError,
}

impl<K, V> fmt::Debug for FlatInsertError<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlatInsertError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl<K, V> fmt::Display for FlatInsertError<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl<K, V> std::error::Error for FlatInsertError<K, V> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

struct Entry<K, V> {
    hash: u64,
    key: K,
    value: V,
}

#[repr(C)]
struct Header {
    len: usize,
    max_groups: usize,
}

#[repr(C, align(16))]
struct Group<K, V> {
    control: [u8; FLAT_GROUP_WIDTH],
    entries: [Option<Entry<K, V>>; FLAT_GROUP_WIDTH],
    header: Header,
}

impl<K, V> Group<K, V> {
    // Full values are created only after constructor layout validation.
    #[inline(never)]
    fn empty() -> Self {
        Self {
            control: [EMPTY; FLAT_GROUP_WIDTH],
            entries: std::array::from_fn(|_| None),
            header: Header {
                len: 0,
                max_groups: 0,
            },
        }
    }
}

/// Move-only owner of inline region-resident entries and scalar control groups.
///
/// Hash/Eq must remain stable while keys are stored. Caller-supplied payloads
/// may own indirect heap allocations. Dropping this handle does not release
/// entries: region release/rewind/reset/drop performs their normal affine Drop.
/// There is no SIMD, source linearity, static brand or hash-persistence claim.
///
/// The owner cannot be cloned:
/// ```compile_fail
/// use vesper_alloc::{FlatConfig, Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let t = r.flat_new::<u64, String>(FlatConfig::default()).unwrap();
/// let alias = t.clone();
/// ```
/// Payloads must be static:
/// ```compile_fail
/// use vesper_alloc::{FlatConfig, Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let local = String::from("temporary");
/// let mut t = r.flat_new::<u64, &str>(FlatConfig::default()).unwrap();
/// r.flat_insert(&mut t, 1, local.as_str()).unwrap();
/// ```
/// A real function-pointer coercion cannot change the owner's value type:
/// ```compile_fail
/// use vesper_alloc::RegionFlatTable;
/// fn narrow(t: RegionFlatTable<u64, fn(&str)>)
///     -> RegionFlatTable<u64, fn(&'static str)> { t }
/// ```
/// Ordinary function pointers do support that coercion:
/// ```
/// fn general(_: &str) {}
/// let _: fn(&'static str) = general as fn(&str);
/// ```
/// Keys obtained through iteration are immutable:
/// ```compile_fail
/// use vesper_alloc::{FlatConfig, Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let mut t = r.flat_new::<u64, u64>(FlatConfig::default()).unwrap();
/// r.flat_insert(&mut t, 1, 2).unwrap();
/// let (key, _) = r.flat_iter(&t).unwrap().next().unwrap();
/// *key = 99;
/// ```
/// A value view excludes growth while it is used:
/// ```compile_fail
/// use vesper_alloc::{FlatConfig, Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let mut t = r.flat_new::<u64, u64>(FlatConfig::default()).unwrap();
/// r.flat_insert(&mut t, 1, 2).unwrap();
/// let value = r.flat_get(&t, &1).unwrap().unwrap();
/// r.flat_reserve(&mut t, 100).unwrap();
/// assert_eq!(*value, 2);
/// ```
/// A control view excludes rewind while it is used:
/// ```compile_fail
/// use vesper_alloc::{FlatConfig, Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let t = r.flat_new::<u64, u64>(FlatConfig::default()).unwrap();
/// let mark = r.checkpoint().unwrap();
/// let controls = r.flat_control_group(&t, 0).unwrap().unwrap();
/// r.rewind(mark).unwrap();
/// assert_eq!(controls.len(), 16);
/// ```
pub struct RegionFlatTable<K: Hash + Eq + 'static, V: 'static> {
    groups: RegionVec<Group<K, V>>,
}

impl<K: Hash + Eq, V> RegionFlatTable<K, V> {
    pub fn region(&self) -> RegionId {
        self.groups.region()
    }
}

impl<K: Hash + Eq, V> fmt::Debug for RegionFlatTable<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegionFlatTable")
            .field("groups", &self.groups)
            .finish()
    }
}

/// Shared ascending group/lane traversal, borrowing Region rather than its owner.
pub struct FlatIter<'r, K: 'static, V: 'static> {
    groups: &'r [Group<K, V>],
    group: usize,
    lane: usize,
    remaining: usize,
}

impl<'r, K, V> Iterator for FlatIter<'r, K, V> {
    type Item = (&'r K, &'r V);

    fn next(&mut self) -> Option<Self::Item> {
        while self.group < self.groups.len() {
            if self.lane == FLAT_GROUP_WIDTH {
                self.group += 1;
                self.lane = 0;
                continue;
            }
            let entry = self.groups[self.group].entries[self.lane].as_ref();
            self.lane += 1;
            if let Some(entry) = entry {
                self.remaining -= 1;
                return Some((&entry.key, &entry.value));
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<K, V> ExactSizeIterator for FlatIter<'_, K, V> {}
impl<K, V> FusedIterator for FlatIter<'_, K, V> {}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Slot {
    group: usize,
    lane: usize,
}

fn hash_key<K: Hash>(key: &K) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

fn tag(hash: u64) -> u8 {
    (hash >> 57) as u8
}

fn find<K: Eq, V>(groups: &[Group<K, V>], key: &K, hash: u64) -> Option<Slot> {
    let mask = groups.len() - 1;
    let start = hash as usize & mask;
    for step in 0..groups.len() {
        let group = (start + step) & mask;
        for lane in 0..FLAT_GROUP_WIDTH {
            if groups[group].control[lane] == tag(hash) {
                let entry = groups[group].entries[lane].as_ref().expect("occupied slot");
                if entry.hash == hash && entry.key == *key {
                    return Some(Slot { group, lane });
                }
            }
        }
        // Every matching candidate in this group precedes EMPTY termination.
        if groups[group].control.contains(&EMPTY) {
            return None;
        }
    }
    None
}

// A second, callback-free bounded pass finds the earliest reusable vacancy.
// Equality search has already finished before insertion calls this helper.
fn vacancy<K, V>(groups: &[Group<K, V>], hash: u64) -> Slot {
    let mask = groups.len() - 1;
    let start = hash as usize & mask;
    for step in 0..groups.len() {
        let group = (start + step) & mask;
        if let Some(lane) = groups[group]
            .control
            .iter()
            .position(|&control| control >= EMPTY)
        {
            return Slot { group, lane };
        }
    }
    unreachable!("live threshold leaves a vacant lane")
}

fn groups_for(entries: usize, max_groups: usize) -> Result<usize, FlatError> {
    let max = LIVE_PER_GROUP * max_groups;
    if entries > max {
        return Err(FlatError::CapacityLimit {
            requested: entries,
            max,
        });
    }
    let groups = (entries / LIVE_PER_GROUP + usize::from(!entries.is_multiple_of(LIVE_PER_GROUP)))
        .max(1)
        .next_power_of_two();
    Ok(groups)
}

fn validate_layout<K, V>() -> Result<(), FlatError> {
    let bytes = size_of::<Group<K, V>>();
    if bytes == 0
        || bytes > HARD_MAX_FLAT_GROUP_BYTES
        || Layout::from_size_align(bytes, align_of::<Group<K, V>>()).is_err()
    {
        return Err(FlatError::UnsupportedEntryLayout);
    }
    Ok(())
}

fn rehash<K, V>(groups: &mut [Group<K, V>]) {
    for group in groups.iter_mut() {
        group.control.fill(EMPTY);
    }
    for group in 0..groups.len() {
        for lane in 0..FLAT_GROUP_WIDTH {
            if groups[group].control[lane] < EMPTY {
                continue;
            }
            let mut pending = groups[group].entries[lane].take();
            while let Some(entry) = pending {
                let slot = vacancy(groups, entry.hash);
                let control = tag(entry.hash);
                // An EMPTY slot may hold an unplaced entry. Moving it into the
                // pending variable preserves sole ownership without cloning or
                // dropping. Every placement consumes one unplaced entry; the
                // aggregate placement count is the original resident length.
                pending = groups[slot.group].entries[slot.lane].replace(entry);
                groups[slot.group].control[slot.lane] = control;
            }
        }
    }
}

// Keep the complete Group stack frame out of generic constructor preflight,
// including optimized builds of representable but refused huge field types.
#[inline(never)]
fn initialize<K: 'static, V: 'static, R: ChunkBacking>(
    region: &mut Region<R>,
    groups: &mut RegionVec<Group<K, V>>,
    count: usize,
) {
    let old = region.vec_len(groups).expect("live pre-reserved owner");
    for _ in old..count {
        // Exact reservation already succeeded; each push is within capacity
        // and executes no allocation or user callback. A returned resource Err
        // here would contradict the checked RegionVec invariants.
        region.vec_push(groups, Group::empty()).unwrap_or_else(|_| {
            unreachable!("empty group push stays within pre-reserved capacity")
        });
    }
}

fn entry_mut<K, V>(groups: &mut [Group<K, V>], slot: Slot) -> &mut Entry<K, V> {
    groups[slot.group].entries[slot.lane]
        .as_mut()
        .expect("resolved occupied slot")
}

fn pair_mut<K, V>(groups: &mut [Group<K, V>], a: Slot, b: Slot) -> [&mut V; 2] {
    if a.group == b.group {
        let entries = &mut groups[a.group].entries;
        if a.lane < b.lane {
            let (low, high) = entries.split_at_mut(b.lane);
            [
                &mut low[a.lane].as_mut().expect("occupied").value,
                &mut high[0].as_mut().expect("occupied").value,
            ]
        } else {
            let (low, high) = entries.split_at_mut(a.lane);
            [
                &mut high[0].as_mut().expect("occupied").value,
                &mut low[b.lane].as_mut().expect("occupied").value,
            ]
        }
    } else if a.group < b.group {
        let (low, high) = groups.split_at_mut(b.group);
        [
            &mut entry_mut(low, a).value,
            &mut entry_mut(
                high,
                Slot {
                    group: 0,
                    lane: b.lane,
                },
            )
            .value,
        ]
    } else {
        let (low, high) = groups.split_at_mut(a.group);
        [
            &mut entry_mut(
                high,
                Slot {
                    group: 0,
                    lane: a.lane,
                },
            )
            .value,
            &mut entry_mut(low, b).value,
        ]
    }
}

impl<R: ChunkBacking> Region<R> {
    pub fn flat_new<K: Hash + Eq + 'static, V: 'static>(
        &mut self,
        config: FlatConfig,
    ) -> Result<RegionFlatTable<K, V>, FlatError> {
        self.flat_with_capacity(0, config)
    }

    pub fn flat_with_capacity<K: Hash + Eq + 'static, V: 'static>(
        &mut self,
        entries: usize,
        config: FlatConfig,
    ) -> Result<RegionFlatTable<K, V>, FlatError> {
        if !config.max_groups.is_power_of_two() || config.max_groups > HARD_MAX_FLAT_GROUPS {
            return Err(FlatError::InvalidMaxGroups {
                max_groups: config.max_groups,
            });
        }
        validate_layout::<K, V>()?;
        let count = groups_for(entries, config.max_groups)?;
        count
            .checked_mul(size_of::<Group<K, V>>())
            .filter(|&bytes| bytes <= isize::MAX as usize)
            .ok_or(AllocError::SizeOverflow)?;
        let mut groups = self.vec_with_capacity(count)?;
        initialize(self, &mut groups, count);
        self.vec_as_mut_slice(&groups)
            .expect("new initialized owner")[0]
            .header
            .max_groups = config.max_groups;
        Ok(RegionFlatTable { groups })
    }

    pub fn flat_len<K: Hash + Eq, V>(
        &self,
        owner: &RegionFlatTable<K, V>,
    ) -> Result<usize, HandleError> {
        Ok(self.vec_as_slice(&owner.groups)?[0].header.len)
    }

    pub fn flat_is_empty<K: Hash + Eq, V>(
        &self,
        owner: &RegionFlatTable<K, V>,
    ) -> Result<bool, HandleError> {
        Ok(self.flat_len(owner)? == 0)
    }

    pub fn flat_capacity<K: Hash + Eq, V>(
        &self,
        owner: &RegionFlatTable<K, V>,
    ) -> Result<usize, HandleError> {
        Ok(self.flat_group_count(owner)? * LIVE_PER_GROUP)
    }

    pub fn flat_group_count<K: Hash + Eq, V>(
        &self,
        owner: &RegionFlatTable<K, V>,
    ) -> Result<usize, HandleError> {
        self.vec_len(&owner.groups)
    }

    pub fn flat_get<'r, K: Hash + Eq, V>(
        &'r self,
        owner: &RegionFlatTable<K, V>,
        key: &K,
    ) -> Result<Option<&'r V>, HandleError> {
        let groups = self.vec_as_slice(&owner.groups)?;
        Ok(find(groups, key, hash_key(key)).map(|slot| {
            &groups[slot.group].entries[slot.lane]
                .as_ref()
                .expect("occupied")
                .value
        }))
    }

    pub fn flat_get_mut<'r, K: Hash + Eq, V>(
        &'r mut self,
        owner: &RegionFlatTable<K, V>,
        key: &K,
    ) -> Result<Option<&'r mut V>, HandleError> {
        let groups = self.vec_as_slice(&owner.groups)?;
        let slot = find(groups, key, hash_key(key));
        let groups = self.vec_as_mut_slice(&owner.groups)?;
        Ok(slot.map(|slot| &mut entry_mut(groups, slot).value))
    }

    pub fn flat_get_disjoint_mut<'r, K: Hash + Eq, V>(
        &'r mut self,
        owner: &RegionFlatTable<K, V>,
        keys: [&K; 2],
    ) -> Result<[Option<&'r mut V>; 2], HandleError> {
        let groups = self.vec_as_slice(&owner.groups)?;
        let first = find(groups, keys[0], hash_key(keys[0]));
        let second = find(groups, keys[1], hash_key(keys[1]));
        if first.is_some() && first == second {
            return Err(HandleError::Aliased);
        }
        let groups = self.vec_as_mut_slice(&owner.groups)?;
        Ok(match (first, second) {
            (Some(a), Some(b)) => {
                let [a, b] = pair_mut(groups, a, b);
                [Some(a), Some(b)]
            }
            (Some(a), None) => [Some(&mut entry_mut(groups, a).value), None],
            (None, Some(b)) => [None, Some(&mut entry_mut(groups, b).value)],
            (None, None) => [None, None],
        })
    }

    pub fn flat_iter<'r, K: Hash + Eq, V>(
        &'r self,
        owner: &RegionFlatTable<K, V>,
    ) -> Result<FlatIter<'r, K, V>, HandleError> {
        let groups = self.vec_as_slice(&owner.groups)?;
        Ok(FlatIter {
            groups,
            group: 0,
            lane: 0,
            remaining: groups[0].header.len,
        })
    }

    pub fn flat_control_group<'r, K: Hash + Eq, V>(
        &'r self,
        owner: &RegionFlatTable<K, V>,
        group: usize,
    ) -> Result<Option<&'r [u8; FLAT_GROUP_WIDTH]>, HandleError> {
        Ok(self
            .vec_as_slice(&owner.groups)?
            .get(group)
            .map(|group| &group.control))
    }

    pub fn flat_insert<K: Hash + Eq, V>(
        &mut self,
        owner: &mut RegionFlatTable<K, V>,
        key: K,
        value: V,
    ) -> Result<Option<(K, V)>, FlatInsertError<K, V>> {
        let groups = match self.vec_as_slice(&owner.groups) {
            Ok(groups) => groups,
            Err(error) => {
                return Err(FlatInsertError {
                    value: (key, value),
                    error: RegionError::from(error).into(),
                });
            }
        };
        let hash = hash_key(&key);
        if let Some(slot) = find(groups, &key, hash) {
            let groups = self
                .vec_as_mut_slice(&owner.groups)
                .expect("checked owner, no intervening mutation");
            let old = groups[slot.group].entries[slot.lane]
                .replace(Entry { hash, key, value })
                .expect("occupied");
            return Ok(Some((old.key, old.value)));
        }
        if let Err(error) = self.flat_reserve(owner, 1) {
            return Err(FlatInsertError {
                value: (key, value),
                error,
            });
        }
        let groups = self
            .vec_as_mut_slice(&owner.groups)
            .expect("checked reserved owner");
        let slot = vacancy(groups, hash);
        groups[slot.group].entries[slot.lane] = Some(Entry { hash, key, value });
        groups[slot.group].control[slot.lane] = tag(hash);
        groups[0].header.len += 1;
        Ok(None)
    }

    pub fn flat_remove<K: Hash + Eq, V>(
        &mut self,
        owner: &RegionFlatTable<K, V>,
        key: &K,
    ) -> Result<Option<(K, V)>, HandleError> {
        let groups = self.vec_as_slice(&owner.groups)?;
        let Some(slot) = find(groups, key, hash_key(key)) else {
            return Ok(None);
        };
        let groups = self.vec_as_mut_slice(&owner.groups)?;
        let entry = groups[slot.group].entries[slot.lane]
            .take()
            .expect("occupied");
        groups[slot.group].control[slot.lane] = DELETED;
        groups[0].header.len -= 1;
        Ok(Some((entry.key, entry.value)))
    }

    pub fn flat_reserve<K: Hash + Eq, V>(
        &mut self,
        owner: &mut RegionFlatTable<K, V>,
        additional: usize,
    ) -> Result<(), FlatError> {
        let groups = self
            .vec_as_slice(&owner.groups)
            .map_err(RegionError::from)?;
        let required = groups[0]
            .header
            .len
            .checked_add(additional)
            .ok_or(AllocError::SizeOverflow)?;
        let count = groups_for(required, groups[0].header.max_groups)?;
        if count <= groups.len() {
            return Ok(());
        }
        count
            .checked_mul(size_of::<Group<K, V>>())
            .filter(|&bytes| bytes <= isize::MAX as usize)
            .ok_or(AllocError::SizeOverflow)?;
        let additional_groups = count - groups.len();
        self.vec_reserve_exact(&mut owner.groups, additional_groups)?;
        initialize(self, &mut owner.groups, count);
        rehash(
            self.vec_as_mut_slice(&owner.groups)
                .expect("initialized reserved owner"),
        );
        Ok(())
    }

    pub fn flat_clear<K: Hash + Eq, V>(
        &mut self,
        owner: &RegionFlatTable<K, V>,
    ) -> Result<(), HandleError> {
        let groups = self.vec_as_mut_slice(&owner.groups)?;
        for group in 0..groups.len() {
            for lane in 0..FLAT_GROUP_WIDTH {
                if let Some(entry) = groups[group].entries[lane].take() {
                    // Preserve stable surviving probe chains even if Drop panics.
                    groups[group].control[lane] = DELETED;
                    groups[0].header.len -= 1;
                    drop(entry);
                }
            }
        }
        for group in groups {
            group.control.fill(EMPTY);
        }
        Ok(())
    }

    pub fn flat_release<K: Hash + Eq, V>(
        &mut self,
        owner: RegionFlatTable<K, V>,
    ) -> Result<(), OwnerError<RegionFlatTable<K, V>>> {
        self.vec_release(owner.groups).map_err(|error| OwnerError {
            owner: RegionFlatTable {
                groups: error.owner,
            },
            error: error.error,
        })
    }
}
