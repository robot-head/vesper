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
//! Unsafe code is denied crate-wide and allowed only in the Rust build's
//! kernels (`src/reference/simd.rs`), which confine `std::arch` loads, stores
//! and feature-gated calls, and in the Vesper build's generated binding.
//!
//! # Two builds
//!
//! By default the Rust implementation in `src/reference/` runs. With the
//! `vesper-port` feature, the table, its CID index, the batch update, the
//! lane kernels and the Toeplitz hashes run in the Vesper port in
//! `vesper/src/` (Rust exit SH6 lane 4, #1399), through the generated
//! `port/binding.rs`. The wrappers in `src/port/` keep the caller's
//! `BuildHasher` (they hash CIDs and pass the hash in), the ISA choice and
//! the conversions between the crate's types and words.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod batch;
pub mod cid;
pub mod error;
#[cfg(feature = "vesper-port")]
mod port;
#[cfg(not(feature = "vesper-port"))]
mod reference;
pub mod rss;
pub mod shard;
pub mod simd;
pub mod table;
pub mod tile;

/// Live Vesper sequence and box allocations of the Vesper build (for leak
/// tests; the Rust allocator never sees them).
#[cfg(feature = "vesper-port")]
#[doc(hidden)]
pub fn port_live_allocations() -> i64 {
    port::binding::live_allocations()
}

/// Which implementation this build runs.
pub const IMPLEMENTATION: &str = if cfg!(feature = "vesper-port") {
    "vesper"
} else {
    "rust"
};

pub use batch::{ConnEvent, ConnEventDesc, MAX_BATCH};
pub use cid::{ConnectionId, MAX_CID_LEN};
pub use error::{
    BatchError, ConfigError, InsertError, LookupError, RekeyError, SetError, StaleHandle,
};
pub use rss::{FiveTuple, HashFields, IndirectionTable, MICROSOFT_RSS_KEY, RssError, Toeplitz};
pub use shard::{Shard, ShardId, ShardSet, ShardSetError};
pub use simd::{IsaPath, Kernels, LANES, UnsupportedIsa};
pub use table::{
    ConnState, ConnTable, IdleScan, MAX_CAPACITY, PN_LIMIT, Relocation, SlotHandle, TableConfig,
};
