//! Region-backed affine slot arenas with checked generational keys.
//!
//! Keys provide runtime validation, not a source-language region brand. Entry
//! values and every arena count/free-list/generation live in one RegionVec.
#![forbid(unsafe_code)]

use super::{Region, RegionId, RegionVec};
use crate::{AllocError, ChunkBacking, HandleError, OwnerError, RegionError};
use std::alloc::Layout;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem::{replace, size_of};
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum actual entry enum size, including its header/occupied variants.
pub const HARD_MAX_ARENA_ENTRY_BYTES: usize = 64 * 1024;
static NEXT_ARENA_ID: AtomicU64 = AtomicU64::new(1);

/// Process-unique arena identity. Inspection is not authorization to access it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArenaId(u64);

impl ArenaId {
    pub fn bits(self) -> u64 {
        self.0
    }
}

/// Copyable checked weak reference to one arena entry, without owning its T.
/// Private identity/generation fields cannot be reconstructed from diagnostics.
///
/// Keys are invariant even between valid static function-pointer field types:
/// ```compile_fail
/// use vesper_alloc::ArenaKey;
/// fn narrow(key: ArenaKey<fn(&str)>) -> ArenaKey<fn(&'static str)> { key }
/// ```
/// Ordinary function pointers do permit this coercion:
/// ```
/// fn narrow(value: fn(&str)) -> fn(&'static str) { value }
/// fn inspect(_: &str) {}
/// narrow(inspect)("example");
/// ```
pub struct ArenaKey<T: 'static> {
    arena: ArenaId,
    slot: u32,
    generation: u64,
    _payload: PhantomData<fn(T) -> T>,
}

impl<T> ArenaKey<T> {
    pub fn arena(self) -> ArenaId {
        self.arena
    }
    pub fn slot(self) -> u32 {
        self.slot
    }
    pub fn generation(self) -> u64 {
        self.generation
    }
}

impl<T> Copy for ArenaKey<T> {}
impl<T> Clone for ArenaKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> PartialEq for ArenaKey<T> {
    fn eq(&self, other: &Self) -> bool {
        (self.arena, self.slot, self.generation) == (other.arena, other.slot, other.generation)
    }
}
impl<T> Eq for ArenaKey<T> {}
impl<T> Hash for ArenaKey<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.arena.hash(state);
        self.slot.hash(state);
        self.generation.hash(state);
    }
}
impl<T> fmt::Debug for ArenaKey<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArenaKey")
            .field("arena", &self.arena)
            .field("slot", &self.slot)
            .field("generation", &self.generation)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArenaError {
    Region(RegionError),
    UnsupportedEntryLayout,
    ExhaustedArenaIds,
    ExhaustedGenerations,
    ForeignArena { expected: ArenaId, found: ArenaId },
    StaleKey,
    AliasedKey,
}

impl fmt::Display for ArenaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Region(error) => write!(f, "arena region operation: {error}"),
            Self::UnsupportedEntryLayout => f.write_str("unsupported arena entry layout"),
            Self::ExhaustedArenaIds => f.write_str("arena identities exhausted"),
            Self::ExhaustedGenerations => f.write_str("arena generations exhausted"),
            Self::ForeignArena { expected, found } => {
                write!(f, "expected arena {expected:?}, found {found:?}")
            }
            Self::StaleKey => f.write_str("stale arena key"),
            Self::AliasedKey => f.write_str("arena projections refer to the same entry"),
        }
    }
}
impl std::error::Error for ArenaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Region(error) => Some(error),
            _ => None,
        }
    }
}
impl From<RegionError> for ArenaError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}
impl From<HandleError> for ArenaError {
    fn from(error: HandleError) -> Self {
        Self::Region(error.into())
    }
}
impl From<AllocError> for ArenaError {
    fn from(error: AllocError) -> Self {
        Self::Region(error.into())
    }
}

/// Failed insertion, preserving the original payload and its allocations.
pub struct ArenaInsertError<T> {
    pub value: T,
    pub error: ArenaError,
}
impl<T> fmt::Debug for ArenaInsertError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArenaInsertError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl<T> fmt::Display for ArenaInsertError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl<T> std::error::Error for ArenaInsertError<T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy)]
struct Header {
    id: ArenaId,
    live: usize,
    free_head: Option<u32>,
    next_generation: u64,
}

enum Entry<T> {
    Header(Header),
    Vacant { next_free: Option<u32> },
    Occupied { generation: u64, value: T },
}

/// Move-only owner of affine region entries, including non-Copy Drop values.
/// The sole owner field is a checked RegionVec; all logical state is resident.
/// Dropping/forgetting this handle leaves finalization to its region.
///
/// Owners cannot be cloned:
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut region = Region::new(RegionConfig::default()).unwrap();
/// let owner = region.arena_new::<String>().unwrap();
/// let alias = owner.clone();
/// ```
/// Payloads cannot borrow shorter-lived local data:
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut region = Region::new(RegionConfig::default()).unwrap();
/// let local = String::from("temporary");
/// let mut owner = region.arena_new::<&str>().unwrap();
/// region.arena_insert(&mut owner, local.as_str()).unwrap();
/// ```
/// Live projections prevent concurrent mutation of the region:
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut region = Region::new(RegionConfig::default()).unwrap();
/// let mut owner = region.arena_new::<String>().unwrap();
/// let key = region.arena_insert(&mut owner, String::from("value")).unwrap();
/// let value = region.arena_get(&owner, key).unwrap();
/// region.arena_remove(&mut owner, key).unwrap();
/// assert_eq!(value, "value");
/// ```
/// Owners retain the underlying RegionVec type invariance:
/// ```compile_fail
/// use vesper_alloc::RegionSlotArena;
/// fn narrow(owner: RegionSlotArena<fn(&str)>)
///     -> RegionSlotArena<fn(&'static str)> { owner }
/// ```
pub struct RegionSlotArena<T: 'static> {
    entries: RegionVec<Entry<T>>,
}
impl<T> RegionSlotArena<T> {
    pub fn region(&self) -> RegionId {
        self.entries.region()
    }
}
impl<T> fmt::Debug for RegionSlotArena<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegionSlotArena")
            .field("entries", &self.entries)
            .finish()
    }
}

fn successor(current: u64) -> Option<u64> {
    current.checked_add(1)
}

fn mint_id(counter: &AtomicU64) -> Result<ArenaId, ArenaError> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, successor)
        .map(ArenaId)
        .map_err(|_| ArenaError::ExhaustedArenaIds)
}

fn prospective_generation(current: u64) -> Result<(u64, u64), ArenaError> {
    successor(current)
        .map(|next| (current, next))
        .ok_or(ArenaError::ExhaustedGenerations)
}

fn header<T>(entries: &[Entry<T>]) -> &Header {
    match &entries[0] {
        Entry::Header(header) => header,
        _ => unreachable!("private arena header invariant"),
    }
}
fn header_mut<T>(entries: &mut [Entry<T>]) -> &mut Header {
    match &mut entries[0] {
        Entry::Header(header) => header,
        _ => unreachable!("private arena header invariant"),
    }
}
fn value<T>(entry: &Entry<T>) -> &T {
    match entry {
        Entry::Occupied { value, .. } => value,
        _ => unreachable!("validated occupied entry"),
    }
}
fn value_mut<T>(entry: &mut Entry<T>) -> &mut T {
    match entry {
        Entry::Occupied { value, .. } => value,
        _ => unreachable!("validated occupied entry"),
    }
}
fn key_index<T>(entries: &[Entry<T>], key: ArenaKey<T>) -> Result<usize, ArenaError> {
    let expected = header(entries).id;
    if key.arena != expected {
        return Err(ArenaError::ForeignArena {
            expected,
            found: key.arena,
        });
    }
    let index = usize::try_from(key.slot)
        .ok()
        .and_then(|slot| slot.checked_add(1))
        .ok_or(ArenaError::StaleKey)?;
    match entries.get(index) {
        Some(Entry::Occupied { generation, .. }) if *generation == key.generation => Ok(index),
        _ => Err(ArenaError::StaleKey),
    }
}
fn request_entries<T>(payload_slots: usize) -> Result<usize, ArenaError> {
    if payload_slots > u32::MAX as usize {
        return Err(AllocError::SizeOverflow.into());
    }
    let entries = payload_slots
        .checked_add(1)
        .ok_or(AllocError::SizeOverflow)?;
    entries
        .checked_mul(size_of::<Entry<T>>())
        .filter(|bytes| *bytes <= isize::MAX as usize)
        .ok_or(AllocError::SizeOverflow)?;
    Ok(entries)
}

impl<B: ChunkBacking> Region<B> {
    pub fn arena_new<T>(&mut self) -> Result<RegionSlotArena<T>, ArenaError> {
        self.arena_with_capacity(0)
    }

    /// Validate the full enum shape before constructing even a Header entry.
    /// Rust may reject unrepresentable types before this runtime boundary.
    pub fn arena_with_capacity<T>(
        &mut self,
        items: usize,
    ) -> Result<RegionSlotArena<T>, ArenaError> {
        let layout = Layout::new::<Entry<T>>();
        if layout.size() > HARD_MAX_ARENA_ENTRY_BYTES {
            return Err(ArenaError::UnsupportedEntryLayout);
        }
        let count = request_entries::<T>(items)?;
        let id = mint_id(&NEXT_ARENA_ID)?;
        let mut entries = self.vec_with_capacity(count)?;
        // A fresh checked buffer has capacity >=1; this push cannot allocate
        // or run payload code, and the complete entry type is already bounded.
        self.vec_push(
            &mut entries,
            Entry::Header(Header {
                id,
                live: 0,
                free_head: None,
                next_generation: 1,
            }),
        )
        .map_err(|error| ArenaError::from(error.error))?;
        Ok(RegionSlotArena { entries })
    }

    pub fn arena_id<T>(&self, owner: &RegionSlotArena<T>) -> Result<ArenaId, ArenaError> {
        Ok(header(self.vec_as_slice(&owner.entries)?).id)
    }
    pub fn arena_len<T>(&self, owner: &RegionSlotArena<T>) -> Result<usize, ArenaError> {
        Ok(header(self.vec_as_slice(&owner.entries)?).live)
    }
    pub fn arena_is_empty<T>(&self, owner: &RegionSlotArena<T>) -> Result<bool, ArenaError> {
        Ok(self.arena_len(owner)? == 0)
    }
    pub fn arena_capacity<T>(&self, owner: &RegionSlotArena<T>) -> Result<usize, ArenaError> {
        Ok((self.vec_capacity(&owner.entries)? - 1).min(u32::MAX as usize))
    }

    /// Reserve memory for additional insertions, counting reusable vacancies.
    /// This does not consume or promise future generation numbers.
    pub fn arena_reserve<T>(
        &mut self,
        owner: &mut RegionSlotArena<T>,
        additional: usize,
    ) -> Result<(), ArenaError> {
        let entries = self.vec_as_slice(&owner.entries)?;
        let live = header(entries).live;
        let required_live = live
            .checked_add(additional)
            .ok_or(AllocError::SizeOverflow)?;
        request_entries::<T>(required_live)?;
        let vacant = entries.len() - 1 - live;
        let new_slots = additional.saturating_sub(vacant);
        self.vec_reserve(&mut owner.entries, new_slots)?;
        Ok(())
    }

    pub fn arena_insert<T>(
        &mut self,
        owner: &mut RegionSlotArena<T>,
        value: T,
    ) -> Result<ArenaKey<T>, ArenaInsertError<T>> {
        let plan = (|| {
            let entries = self.vec_as_slice(&owner.entries)?;
            let state = *header(entries);
            let (generation, next) = prospective_generation(state.next_generation)?;
            let slot = match state.free_head {
                Some(slot) => slot,
                None => u32::try_from(entries.len() - 1)
                    .map_err(|_| ArenaError::from(AllocError::SizeOverflow))?,
            };
            self.arena_reserve(owner, 1)?;
            Ok::<_, ArenaError>((state, generation, next, slot))
        })();
        let (state, generation, next, slot) = match plan {
            Ok(plan) => plan,
            Err(error) => return Err(ArenaInsertError { value, error }),
        };
        if state.free_head.is_some() {
            let entries = match self.vec_as_mut_slice(&owner.entries) {
                Ok(entries) => entries,
                Err(error) => {
                    return Err(ArenaInsertError {
                        value,
                        error: error.into(),
                    });
                }
            };
            let index = usize::try_from(slot)
                .ok()
                .and_then(|slot| slot.checked_add(1))
                .expect("private free slot fits entry vector");
            let next_free = match &entries[index] {
                Entry::Vacant { next_free } => *next_free,
                _ => unreachable!("private free-list invariant"),
            };
            entries[index] = Entry::Occupied { generation, value };
            let state = header_mut(entries);
            state.free_head = next_free;
            state.live += 1;
            state.next_generation = next;
        } else {
            if let Err(error) =
                self.vec_push(&mut owner.entries, Entry::Occupied { generation, value })
            {
                let Entry::Occupied { value, .. } = error.value else {
                    unreachable!("inserted entry variant")
                };
                return Err(ArenaInsertError {
                    value,
                    error: error.error.into(),
                });
            }
            let state = header_mut(
                self.vec_as_mut_slice(&owner.entries)
                    .expect("same checked owner after capacity-preserving push"),
            );
            state.live += 1;
            state.next_generation = next;
        }
        Ok(ArenaKey {
            arena: state.id,
            slot,
            generation,
            _payload: PhantomData,
        })
    }

    pub fn arena_get<'a, T>(
        &'a self,
        owner: &RegionSlotArena<T>,
        key: ArenaKey<T>,
    ) -> Result<&'a T, ArenaError> {
        let entries = self.vec_as_slice(&owner.entries)?;
        Ok(value(&entries[key_index(entries, key)?]))
    }
    pub fn arena_get_mut<'a, T>(
        &'a mut self,
        owner: &RegionSlotArena<T>,
        key: ArenaKey<T>,
    ) -> Result<&'a mut T, ArenaError> {
        let entries = self.vec_as_mut_slice(&owner.entries)?;
        let index = key_index(entries, key)?;
        Ok(value_mut(&mut entries[index]))
    }

    pub fn arena_get_disjoint_mut<'a, T>(
        &'a mut self,
        owner: &RegionSlotArena<T>,
        first: ArenaKey<T>,
        second: ArenaKey<T>,
    ) -> Result<(&'a mut T, &'a mut T), ArenaError> {
        let entries = self.vec_as_mut_slice(&owner.entries)?;
        let first = key_index(entries, first)?;
        let second = key_index(entries, second)?;
        if first == second {
            return Err(ArenaError::AliasedKey);
        }
        if first < second {
            let (left, right) = entries.split_at_mut(second);
            Ok((value_mut(&mut left[first]), value_mut(&mut right[0])))
        } else {
            let (left, right) = entries.split_at_mut(first);
            Ok((value_mut(&mut right[0]), value_mut(&mut left[second])))
        }
    }

    pub fn arena_remove<T>(
        &mut self,
        owner: &mut RegionSlotArena<T>,
        key: ArenaKey<T>,
    ) -> Result<T, ArenaError> {
        let entries = self.vec_as_mut_slice(&owner.entries)?;
        let index = key_index(entries, key)?;
        let next_free = header(entries).free_head;
        let removed = replace(&mut entries[index], Entry::Vacant { next_free });
        let Entry::Occupied { value, .. } = removed else {
            unreachable!("validated entry variant")
        };
        let state = header_mut(entries);
        state.free_head = Some(key.slot);
        state.live -= 1;
        Ok(value)
    }

    pub fn arena_release<T>(
        &mut self,
        owner: RegionSlotArena<T>,
    ) -> Result<(), OwnerError<RegionSlotArena<T>>> {
        self.vec_release(owner.entries).map_err(|error| OwnerError {
            owner: RegionSlotArena {
                entries: error.owner,
            },
            error: error.error,
        })
    }
}
