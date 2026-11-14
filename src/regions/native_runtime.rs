//! Runtime calls behind native region lowering.
//!
//! The native backend lowers a region block to `vesper_region_open` on entry
//! and `vesper_region_close` on every normal exit, and each region operation
//! to calls below. A payload is its native layout, moved word by word
//! (`vesper_region_alloc`, then `vesper_region_store` per word; reads with
//! `vesper_region_load`); the bytes live in a [`vesper_alloc::Region`] owned
//! by a per-thread table. No call is `unsafe`:
//! handles are table indices checked by the allocator, never raw pointers.
//!
//! A failing call (exhausted bound, stale handle) records a [`RegionFault`]
//! and returns 0. The JIT driver calls [`finish`] after the entry returns:
//! it reports the first fault as a typed error and closes any region an
//! abort left open, so faults never become reads of reclaimed memory.
use super::store::{RegionFault, Regions};
use std::cell::RefCell;

#[derive(Default)]
struct Native {
    regions: Regions<()>,
    next: u64,
    fault: Option<RegionFault>,
    /// Regions opened and closed by executions already finished.
    finished: (u64, u64),
}
thread_local! {
    static NATIVE: RefCell<Native> = RefCell::new(Native::default());
}

fn with<R: Default>(f: impl FnOnce(&mut Native) -> Result<R, RegionFault>) -> R {
    NATIVE.with(|n| {
        let mut n = n.borrow_mut();
        if n.fault.is_some() {
            return R::default();
        }
        match f(&mut n) {
            Ok(v) => v,
            Err(e) => {
                n.fault = Some(e);
                R::default()
            }
        }
    })
}

/// Symbols the native backend imports, with their addresses for the JIT.
/// AOT executables define the same symbols in generated code
/// (`src/regions/aot_runtime.rs`).
pub fn symbols() -> [(&'static str, *const u8); 10] {
    [
        ("vesper_region_open", vesper_region_open as *const u8),
        ("vesper_region_close", vesper_region_close as *const u8),
        ("vesper_region_alloc", vesper_region_alloc as *const u8),
        ("vesper_region_load", vesper_region_load as *const u8),
        ("vesper_region_store", vesper_region_store as *const u8),
        ("vesper_region_release", vesper_region_release as *const u8),
        (
            "vesper_region_checkpoint",
            vesper_region_checkpoint as *const u8,
        ),
        ("vesper_region_rewind", vesper_region_rewind as *const u8),
        ("vesper_region_commit", vesper_region_commit as *const u8),
        ("vesper_region_used", vesper_region_used as *const u8),
    ]
}

/// Opens a new region bounded by `capacity` bytes and returns its id.
pub extern "C" fn vesper_region_open(capacity: u64) -> u64 {
    with(|n| {
        n.next = n.next.checked_add(1).ok_or(RegionFault::Inactive)?;
        let capacity = usize::try_from(capacity)
            .map_err(|_| RegionFault::Config("capacity exceeds the address space".into()))?;
        let id = n.next;
        n.regions.open(id, capacity)?;
        Ok(id)
    })
}
/// Closes the region `id`.
pub extern "C" fn vesper_region_close(id: u64) -> u64 {
    with(|n| n.regions.close(id).map(|()| 0))
}
/// Allocates a zeroed payload of `size` bytes (rounded to whole words).
pub extern "C" fn vesper_region_alloc(id: u64, size: u64) -> u64 {
    with(|n| {
        let size = usize::try_from(size)
            .map_err(|_| RegionFault::Exhausted("payload exceeds the address space".into()))?;
        n.regions.alloc(id, size, None)
    })
}
/// Word `index` of a payload.
pub extern "C" fn vesper_region_load(id: u64, handle: u64, index: u64) -> u64 {
    with(|n| n.regions.load(id, handle, index))
}
/// Writes word `index` of a payload.
pub extern "C" fn vesper_region_store(id: u64, handle: u64, index: u64, word: u64) -> u64 {
    with(|n| n.regions.store(id, handle, index, word).map(|()| 0))
}
/// Releases a payload moved out by `take`.
pub extern "C" fn vesper_region_release(id: u64, handle: u64) -> u64 {
    with(|n| n.regions.take(id, handle).map(|_| 0))
}
pub extern "C" fn vesper_region_checkpoint(id: u64) -> u64 {
    with(|n| n.regions.checkpoint(id))
}
pub extern "C" fn vesper_region_rewind(id: u64, mark: u64) -> u64 {
    with(|n| n.regions.rewind(id, mark).map(|()| 0))
}
pub extern "C" fn vesper_region_commit(id: u64, mark: u64) -> u64 {
    with(|n| n.regions.commit(id, mark).map(|()| 0))
}
pub extern "C" fn vesper_region_used(id: u64) -> u64 {
    with(|n| n.regions.used(id))
}

/// Counters of the calling thread's native regions: `(opened, closed by
/// their block, still open)`. Regions an abort left to [`finish`] count as
/// opened but not closed.
pub fn counts() -> (u64, u64, usize) {
    NATIVE.with(|n| {
        let n = n.borrow();
        let (opened, closed, open) = n.regions.counts();
        (n.finished.0 + opened, n.finished.1 + closed, open)
    })
}

/// Ends a native execution on this thread: closes regions left open by an
/// abort and returns the first recorded fault.
pub fn finish() -> Result<(), RegionFault> {
    NATIVE.with(|n| {
        let mut n = n.borrow_mut();
        let (opened, closed, _) = n.regions.counts();
        let finished = (n.finished.0 + opened, n.finished.1 + closed);
        n.regions.close_all();
        let fault = n.fault.take();
        *n = Native {
            finished,
            ..Native::default()
        };
        fault.map_or(Ok(()), Err)
    })
}
