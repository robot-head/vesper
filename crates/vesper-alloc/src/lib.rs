//! Vesper Strata region allocator core (design §15.2 and the VirtualArena half
//! of §15.3), as a host runtime library.
//!
//! A [`Region`] is a bounded, uniquely owned bump allocator over a list of
//! stable chunks. It provides aligned byte and typed allocations, checked size
//! arithmetic, watermarks with LIFO rewind, reset with partial chunk
//! retention, and LIFO finalization of payloads that need drop. Memory is
//! owned explicitly by the region; there is no garbage collector and no
//! reference counting.
//!
//! Handles ([`RegionBytes`], [`RegionBox`], [`Watermark`]) are plain data
//! carrying the minting region's [`RegionId`] and a never-reused serial. Every
//! checked operation rejects handles from other regions and handles whose
//! allocation was rewound, reset, released, relocated, or moved out.
//!
//! ```
//! use vesper_alloc::{HandleError, Region, RegionConfig};
//!
//! let out = Region::scoped(RegionConfig::bounded(64 * 1024), |scratch| {
//!     let header = scratch.alloc_bytes_copy(b"hdr", 8).unwrap();
//!     let mark = scratch.checkpoint().unwrap();
//!     let temp = scratch.alloc_value([0u64; 16]).unwrap();
//!     scratch.rewind(mark).unwrap(); // O(1) bump reset; `temp` is now stale
//!     assert_eq!(scratch.get(&temp), Err(HandleError::Stale));
//!     scratch.bytes(&header).unwrap().len()
//! });
//! assert_eq!(out, Ok(3));
//! ```
//!
//! # What the runtime does not establish
//!
//! Rust's ownership rules do not prove Vesper's linear discipline: a Rust
//! caller may drop a [`RegionBox`] or [`Watermark`], or `mem::forget` it, and
//! may return a handle out of [`Region::scoped`]. The runtime turns those
//! mistakes into errors or reclamation, never into undefined behavior, but the
//! static guarantees of [T-RegionAlloc]/[T-RegionScope] (escape freedom, exact
//! consumption of linear payloads and watermarks) remain compiler obligations.
//! See `docs/parallel-results/strata-regions.md` for the proposed lowering
//! contract.
mod backing;
mod error;
mod region;

pub use backing::{ChunkBacking, SystemBacking};
pub use error::{AllocError, ConfigError, HandleError, RegionError, RewindError, ValueAllocError};
pub use region::{
    CHUNK_ALIGN, Region, RegionBox, RegionBytes, RegionConfig, RegionId, RegionStats, Watermark,
};

/// Version of the proposed Alloc/Region lowering contract described in
/// `docs/parallel-results/strata-regions.md`. Experimental; not yet consumed
/// by the compiler.
pub const REGION_LOWERING_CONTRACT_VERSION: u32 = 0;
