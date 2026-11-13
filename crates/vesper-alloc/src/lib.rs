//! Vesper Strata host-runtime allocators (design §15.2/§15.3).
//!
//! [`ShardSlab`] supplies bounded local byte allocations in 64 KiB slabs,
//! with 48 size classes and unique initialized [`SlabBlock`] owners. Blocks
//! retain backing storage after the pool handle is dropped; local bookkeeping
//! uses non-atomic `Rc`, and cross-thread use is deliberately unsupported.
//! This is separate from the existing region APIs described below.
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
//! Region collections ([`RegionVec`], [`RegionString`]) keep every element
//! byte in region chunks and the initialized length in region metadata, so
//! rewind, reset and drop finalize exactly the live elements:
//!
//! ```
//! use vesper_alloc::{Region, RegionConfig, RegionError};
//!
//! let mut r = Region::new(RegionConfig::bounded(4096)).unwrap();
//! let mut names = r.vec_with_capacity::<&'static str>(2).unwrap();
//! r.vec_push(&mut names, "ada").unwrap();
//! let mut s = r.string_from_str("grüß").unwrap();
//! r.string_push(&mut s, '!').unwrap();
//! assert_eq!(r.string_as_str(&s), Ok("grüß!"));
//! // A buffer allocated before a watermark cannot grow inside it: the new
//! // capacity would be discarded by the rewind while the vector survives.
//! r.vec_push(&mut names, "bob").unwrap();
//! let mark = r.checkpoint().unwrap();
//! let err = r.vec_push(&mut names, "eve").unwrap_err();
//! assert_eq!((err.value, err.error), ("eve", RegionError::ProtectedByWatermark));
//! r.rewind(mark).unwrap();
//! assert_eq!(r.vec_as_slice(&names).unwrap(), ["ada", "bob"]);
//! ```
//!
//! `RegionBox<T>` is invariant in `T`, so a handle cannot be reinterpreted at
//! a shorter lifetime and used to store a dangling borrow:
//!
//! ```compile_fail
//! use vesper_alloc::RegionBox;
//! fn shorten<'a, 'h>(b: &'h RegionBox<&'static str>) -> &'h RegionBox<&'a str> {
//!     b
//! }
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
mod slab;

pub use backing::{ChunkBacking, SystemBacking};
pub use error::{
    AllocError, ConfigError, HandleError, InsertError, OwnerError, RegionError, RewindError,
    StringError, ValueAllocError,
};
pub use region::{
    CHUNK_ALIGN, HARD_MAX_TILE_BYTES, Region, RegionBox, RegionBytes, RegionConfig, RegionId,
    RegionSlice, RegionStats, RegionStr, RegionString, RegionTiled2, RegionVec, TiledError,
    Watermark,
};

/// Version of the proposed Alloc/Region lowering contract described in
/// `docs/parallel-results/strata-regions.md`. Experimental; not yet consumed
/// by the compiler.
pub const REGION_LOWERING_CONTRACT_VERSION: u32 = 0;

/// Version of the proposed `RegionAccess` contract (allocation origin,
/// exclusive mutation, generation/watermark invalidation and linear payload
/// obligations for region collections) described in
/// `docs/parallel-results/strata-collections.md`. Experimental; not yet
/// consumed by the compiler.
pub const REGION_ACCESS_CONTRACT_VERSION: u32 = 1;

pub use slab::{
    SLAB_BYTES, SLAB_SIZE_CLASSES, ShardSlab, SlabBlock, SlabCloseError, SlabConfig, SlabError,
    SlabId, SlabKey, SlabOwnerError, SlabStats,
};
