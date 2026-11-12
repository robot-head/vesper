//! Shared-nothing shard set: RSS steering of 5-tuples to per-shard tables.

use crate::error::ConfigError;
use crate::rss::{FiveTuple, HashFields, IndirectionTable, RssError, RssInput, Toeplitz};
use crate::table::{ConnTable, TableConfig};
use core::fmt;
use std::hash::BuildHasher;

/// Shard number.
pub type ShardId = u16;

/// One shard: a connection table owned outright by whoever holds the shard.
#[derive(Debug)]
pub struct Shard<S> {
    id: ShardId,
    /// The shard's own table. No field is shared with any other shard.
    pub table: ConnTable<S>,
}

impl<S> Shard<S> {
    /// Shard number.
    pub fn id(&self) -> ShardId {
        self.id
    }
}

/// Error building a [`ShardSet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShardSetError {
    /// RSS configuration rejected.
    Rss(RssError),
    /// Table configuration rejected.
    Table(ConfigError),
}

impl fmt::Display for ShardSetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for ShardSetError {}

/// N shards, each owning a disjoint [`ConnTable`], plus the RSS function that
/// maps flows to shards.
///
/// There are no locks, atomics or shared pointers: [`ShardSet::shards_mut`]
/// hands out disjoint `&mut Shard`s that can be moved to one worker thread
/// each (for example with `std::thread::scope`). Steering is a pure function
/// of the tuple, key and indirection table. NIC RSS programming is not
/// performed; see [`crate::rss`].
#[derive(Debug)]
pub struct ShardSet<S> {
    toeplitz: Toeplitz,
    indirection: IndirectionTable,
    fields: HashFields,
    shards: Box<[Shard<S>]>,
}

impl<S: BuildHasher> ShardSet<S> {
    /// Builds `indirection.shards()` shards; `hasher(id)` supplies each
    /// shard's index hasher.
    pub fn new(
        key: &[u8; 40],
        indirection: IndirectionTable,
        fields: HashFields,
        table: TableConfig,
        mut hasher: impl FnMut(ShardId) -> S,
    ) -> Result<ShardSet<S>, ShardSetError> {
        let shards = (0..indirection.shards())
            .map(|id| ConnTable::with_hasher(table, hasher(id)).map(|table| Shard { id, table }))
            .collect::<Result<Box<[_]>, _>>()
            .map_err(ShardSetError::Table)?;
        Ok(ShardSet {
            toeplitz: Toeplitz::new(key),
            indirection,
            fields,
            shards,
        })
    }

    /// Toeplitz hash of a tuple under the configured fields.
    pub fn rss_hash(&self, t: &FiveTuple) -> Result<u32, RssError> {
        let input = RssInput::new(t, self.fields)?;
        self.toeplitz.hash(input.as_bytes())
    }

    /// Shard that owns flow `t`.
    pub fn steer(&self, t: &FiveTuple) -> Result<ShardId, RssError> {
        Ok(self.indirection.shard_for(self.rss_hash(t)?))
    }

    /// Number of shards.
    pub fn len(&self) -> usize {
        self.shards.len()
    }

    /// Whether there are no shards (never true for a constructed set).
    pub fn is_empty(&self) -> bool {
        self.shards.is_empty()
    }

    /// The indirection table.
    pub fn indirection(&self) -> &IndirectionTable {
        &self.indirection
    }

    /// Replaces the indirection table. The shard count must not change.
    pub fn set_indirection(&mut self, table: IndirectionTable) -> Result<(), RssError> {
        if usize::from(table.shards()) != self.shards.len() {
            return Err(RssError::ShardCount {
                shards: usize::from(table.shards()),
            });
        }
        self.indirection = table;
        Ok(())
    }

    /// Shard by id.
    pub fn shard(&self, id: ShardId) -> Option<&Shard<S>> {
        self.shards.get(usize::from(id))
    }

    /// Mutable shard by id.
    pub fn shard_mut(&mut self, id: ShardId) -> Option<&mut Shard<S>> {
        self.shards.get_mut(usize::from(id))
    }

    /// All shards as disjoint mutable borrows.
    pub fn shards_mut(&mut self) -> &mut [Shard<S>] {
        &mut self.shards
    }

    /// All shards.
    pub fn shards(&self) -> &[Shard<S>] {
        &self.shards
    }
}
