//! The `Alloc` capability (§15.2) as a host API.
//!
//! ```text
//! linear effect Alloc {
//!     fn alloc_raw(size: usize, align: usize) -> RawBlock;
//!     fn dealloc_raw(sink block: RawBlock) -> Unit;
//! }
//! ```
//!
//! Code that receives an `&mut impl Alloc` can allocate; code that does not
//! receive one cannot reach these allocators. This is a Rust API convention
//! only: Rust code always has ambient `GlobalAlloc`, so this trait is not
//! evidence for Vesper's static "no `Alloc` in the row means zero heap
//! allocation" rule, which is a compiler obligation.
#![forbid(unsafe_code)]

use crate::{ChunkBacking, Shard, ShardBlock, ShardSlab, SlabBlock, SlabError, SlabOwnerError};

/// The design's `RawBlock` for the sharded heap.
pub type RawBlock = ShardBlock;

/// Allocation authority over raw byte blocks.
pub trait Alloc {
    /// The unique block owner this allocator mints.
    type Block;

    /// Allocates `size` zeroed bytes aligned to `align`.
    fn alloc_raw(&mut self, size: usize, align: usize) -> Result<Self::Block, SlabError>;

    /// Consumes a block. On failure the unchanged owner is returned.
    fn dealloc_raw(&mut self, block: Self::Block) -> Result<(), SlabOwnerError<Self::Block>>;
}

impl Alloc for Shard {
    type Block = ShardBlock;

    fn alloc_raw(&mut self, size: usize, align: usize) -> Result<ShardBlock, SlabError> {
        self.allocate(size, align)
    }

    fn dealloc_raw(&mut self, block: ShardBlock) -> Result<(), SlabOwnerError<ShardBlock>> {
        self.free(block)
    }
}

impl<B: ChunkBacking> Alloc for ShardSlab<B> {
    type Block = SlabBlock<B>;

    fn alloc_raw(&mut self, size: usize, align: usize) -> Result<SlabBlock<B>, SlabError> {
        self.allocate(size, align)
    }

    fn dealloc_raw(&mut self, block: SlabBlock<B>) -> Result<(), SlabOwnerError<SlabBlock<B>>> {
        self.free(block)
    }
}
