//! JIT host runtime for `Box[T]` allocations (`vesper_box_alloc`) (§12.4).
//!
//! Inside verified `@fip` functions, Stage 4 `egglog` constructor reuse eliminates
//! every `Box::new` allocation in favor of in-place slot reuse, so `@fip` code
//! executes zero calls to [`vesper_box_alloc`]. Non-`@fip` callers (such as test
//! drivers constructing initial trees) allocate `Box[T]` payloads through this
//! thread-local allocator, whose count is exposed via [`alloc_count`] for
//! zero-allocation verification tests.

use std::cell::RefCell;

#[derive(Default)]
struct BoxRuntime {
    allocations: Vec<Box<[u64]>>,
    count: u64,
}

thread_local! {
    static BOX_RUNTIME: RefCell<BoxRuntime> = RefCell::new(BoxRuntime::default());
}

/// Symbols imported by Cranelift JIT execution for `Box[T]`.
pub fn symbols() -> [(&'static str, *const u8); 1] {
    [("vesper_box_alloc", vesper_box_alloc as *const u8)]
}

/// Allocates a zero-initialized `Box[T]` buffer of at least `size` bytes
/// (rounded up to whole `u64` words) and returns its aligned heap address.
pub extern "C" fn vesper_box_alloc(size: u64) -> u64 {
    let words = (size as usize).max(8).div_ceil(8);
    let mut buf = vec![0u64; words].into_boxed_slice();
    let ptr = buf.as_mut_ptr() as usize as u64;
    BOX_RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        rt.allocations.push(buf);
        rt.count += 1;
    });
    ptr
}

/// Releases all JIT-allocated `Box[T]` buffers after the caller has decoded
/// any aggregate return value. Preserves [`alloc_count`] so tests can assert
/// exact allocation counts across a JIT invocation.
pub fn finish() {
    BOX_RUNTIME.with(|rt| {
        rt.borrow_mut().allocations.clear();
    });
}

/// Total number of `vesper_box_alloc` calls since the last [`reset_counts`].
pub fn alloc_count() -> u64 {
    BOX_RUNTIME.with(|rt| rt.borrow().count)
}

/// Resets the allocation counter and clears any leftover buffers on this thread.
pub fn reset_counts() {
    BOX_RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        rt.allocations.clear();
        rt.count = 0;
    });
}
