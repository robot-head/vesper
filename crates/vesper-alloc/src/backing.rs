//! Physical chunk providers. A region owns every chunk it obtains from its
//! backing and returns each one exactly once, with the layout it requested.
use std::alloc::{self, Layout};
use std::ptr::NonNull;

/// Source of the contiguous chunks that a [`crate::Region`] bump-allocates from.
///
/// # Safety
///
/// `allocate` must return either `None` or a pointer to `layout.size()` bytes
/// aligned to `layout.align()`, exclusively owned by the caller until it is
/// passed back to `deallocate` with the same layout. The region only calls
/// `allocate` with a nonzero size and calls `deallocate` at most once per
/// returned pointer. A zero-sized request should return `None`.
pub unsafe trait ChunkBacking {
    fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>>;
    /// # Safety
    ///
    /// `ptr` must have been returned by `allocate` on this backing with
    /// `layout`, and must not have been deallocated already.
    unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout);
}

/// Chunks from the Rust global allocator. This is the host runtime's
/// provisioning path, not the design's VirtualArena `mmap(MAP_NORESERVE)`
/// reservation; see `docs/parallel-results/strata-regions.md`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemBacking;

// SAFETY: forwards to the global allocator with the caller's nonzero layout.
unsafe impl ChunkBacking for SystemBacking {
    fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        // `allocate` is a safe method that any caller may use, so the
        // nonzero-size precondition of `alloc::alloc` is checked here.
        if layout.size() == 0 {
            return None;
        }
        // SAFETY: `layout` has a nonzero size.
        NonNull::new(unsafe { alloc::alloc(layout) })
    }

    unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: guaranteed by the caller contract.
        unsafe { alloc::dealloc(ptr.as_ptr(), layout) }
    }
}
