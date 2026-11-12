//! Vesper Warp connection shards (design §35.1 item 4, §35.3 `QuicConnTile`
//! and `lookup_cid_simd`).
//!
//! - [`ConnTable`]: a fixed-capacity, Warp-local tile table. Connections live
//!   in tiles of 16 with one 16-lane array per hot field ([`tile`] documents
//!   the cache-line layout). Slots are addressed by generation-checked
//!   [`SlotHandle`]s. Insert, remove, compaction, lookup, batch update and
//!   idle scan perform no heap allocation after construction. This is not
//!   Strata's generic `TiledSeq`, which does not exist on `main`.
//! - CID lookup: an open-addressed hash index for O(1) average lookup, and a
//!   16-lane tile scan ([`ConnTable::lookup_cid_simd`]) with SSE2, AVX2 or
//!   NEON kernels picked at run time, falling back to scalar.
//! - [`rss`]: Toeplitz hashing, indirection tables and 5-tuple input; a
//!   software model only, no NIC RSS is programmed.
//! - [`ShardSet`]: shared-nothing shards, each owning its own table.
//! - [`batch`]: atomic application of up to 64 (slot, event) descriptors and
//!   the idle-timeout scan, with scalar reference implementations.
//!
//! Unsafe code is denied crate-wide and allowed only in [`simd`], which
//! confines `std::arch` loads, stores and feature-gated calls.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod batch;
pub mod cid;
pub mod error;
mod index;
pub mod rss;
pub mod shard;
pub mod simd;
pub mod table;
pub mod tile;

pub use batch::{ConnEvent, ConnEventDesc, MAX_BATCH};
pub use cid::{ConnectionId, MAX_CID_LEN};
pub use error::{BatchError, ConfigError, InsertError, LookupError, SetError, StaleHandle};
pub use rss::{FiveTuple, HashFields, IndirectionTable, MICROSOFT_RSS_KEY, RssError, Toeplitz};
pub use shard::{Shard, ShardId, ShardSet, ShardSetError};
pub use simd::{IsaPath, Kernels, LANES, UnsupportedIsa};
pub use table::{
    ConnState, ConnTable, IdleScan, MAX_CAPACITY, PN_LIMIT, Relocation, SlotHandle, TableConfig,
};
