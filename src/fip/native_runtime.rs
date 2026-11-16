//! JIT host runtime for `Box[T]` allocations (`vesper_box_alloc` / `vesper_box_free`) (§12.4).
//!
//! Inside verified `@fip` functions, Stage 4 `egglog` constructor reuse eliminates
//! every `Box::new` allocation in favor of in-place slot reuse, so `@fip` code
//! executes zero calls to [`vesper_box_alloc`] or [`vesper_box_free`].
//! Non-`@fip` callers (such as test drivers constructing initial trees)
//! allocate and reclaim `Box[T]` payloads through this thread-local allocator,
//! whose counts are exposed via [`alloc_count`], [`free_count`], and
//! [`live_count`].

use std::{cell::RefCell, collections::HashMap};

#[derive(Default)]
struct BoxRuntime {
    allocations: HashMap<u64, Box<[u64]>>,
    count: u64,
    free_count: u64,
}

thread_local! {
    static BOX_RUNTIME: RefCell<BoxRuntime> = RefCell::new(BoxRuntime::default());
}

/// Symbols imported by Cranelift JIT execution for `Box[T]`.
pub fn symbols() -> [(&'static str, *const u8); 2] {
    [
        ("vesper_box_alloc", vesper_box_alloc as *const u8),
        ("vesper_box_free", vesper_box_free as *const u8),
    ]
}

/// Allocates a zero-initialized `Box[T]` buffer of at least `size` bytes
/// (rounded up to whole `u64` words) and returns its aligned heap address.
pub extern "C" fn vesper_box_alloc(size: u64) -> u64 {
    let words = (size as usize).max(8).div_ceil(8);
    let mut buf = vec![0u64; words].into_boxed_slice();
    let ptr = buf.as_mut_ptr() as usize as u64;
    BOX_RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        rt.allocations.insert(ptr, buf);
        rt.count += 1;
    });
    ptr
}

/// Releases an owned `Box[T]` buffer allocated by [`vesper_box_alloc`].
pub extern "C" fn vesper_box_free(ptr: u64) {
    if ptr == 0 {
        return;
    }
    BOX_RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        if rt.allocations.remove(&ptr).is_some() {
            rt.free_count += 1;
        }
    });
}

/// Releases all leftover JIT-allocated `Box[T]` buffers after the caller has
/// decoded any aggregate return value. Preserves [`alloc_count`] and
/// [`free_count`] so tests can assert exact allocation/reclamation counts.
pub fn finish() {
    BOX_RUNTIME.with(|rt| {
        rt.borrow_mut().allocations.clear();
    });
}

/// Total number of `vesper_box_alloc` calls since the last [`reset_counts`].
pub fn alloc_count() -> u64 {
    BOX_RUNTIME.with(|rt| rt.borrow().count)
}

/// Total number of `vesper_box_free` calls since the last [`reset_counts`].
pub fn free_count() -> u64 {
    BOX_RUNTIME.with(|rt| rt.borrow().free_count)
}

/// Number of currently live un-freed `Box[T]` buffers on this thread.
pub fn live_count() -> usize {
    BOX_RUNTIME.with(|rt| rt.borrow().allocations.len())
}

/// Resets the allocation/free counters and clears any leftover buffers on this thread.
pub fn reset_counts() {
    BOX_RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        rt.allocations.clear();
        rt.count = 0;
        rt.free_count = 0;
    });
}
