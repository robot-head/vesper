//! Runtime calls behind native region lowering.
//!
//! The native backend lowers a region block to `vesper_region_open` on entry
//! and `vesper_region_close` on every normal exit, and each region operation
//! to one call below. Payloads are single machine words (integers, `bool`,
//! `Unit`, and `f64` bit patterns); the words themselves live in a
//! [`vesper_alloc::Region`] owned by a per-thread table. No call is `unsafe`:
//! handles are table indices checked by the allocator, never raw pointers.
//!
//! A failing call (exhausted bound, stale handle) records a [`RegionFault`]
//! and returns 0. The JIT driver calls [`finish`] after the entry returns:
//! it reports the first fault as a typed error and closes any region an
//! abort left open, so faults never become reads of reclaimed memory.
use super::store::{RegionFault, Regions};
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Default)]
struct Native {
    regions: Regions<i64>,
    /// Open activations of each static region block, innermost last.
    active: BTreeMap<i64, Vec<u64>>,
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
pub fn symbols() -> [(&'static str, *const u8); 10] {
    [
        ("vesper_region_open", vesper_region_open as *const u8),
        ("vesper_region_close", vesper_region_close as *const u8),
        ("vesper_region_box", vesper_region_box as *const u8),
        ("vesper_region_get", vesper_region_get as *const u8),
        ("vesper_region_set", vesper_region_set as *const u8),
        ("vesper_region_take", vesper_region_take as *const u8),
        (
            "vesper_region_checkpoint",
            vesper_region_checkpoint as *const u8,
        ),
        ("vesper_region_rewind", vesper_region_rewind as *const u8),
        ("vesper_region_commit", vesper_region_commit as *const u8),
        ("vesper_region_used", vesper_region_used as *const u8),
    ]
}

/// The innermost open activation of a static block.
fn top(n: &Native, key: i64) -> Result<u64, RegionFault> {
    n.active
        .get(&key)
        .and_then(|a| a.last())
        .copied()
        .ok_or(RegionFault::Inactive)
}
/// Opens a new activation of the block `key`, bounded by `capacity` bytes.
pub extern "C" fn vesper_region_open(key: i64, capacity: u64) -> u64 {
    with(|n| {
        n.next = n.next.checked_add(1).ok_or(RegionFault::Inactive)?;
        let capacity = usize::try_from(capacity)
            .map_err(|_| RegionFault::Config("capacity exceeds the address space".into()))?;
        let id = n.next;
        n.regions.open(id, capacity)?;
        n.active.entry(key).or_default().push(id);
        Ok(id)
    })
}
/// Closes the innermost activation of the block `key`.
pub extern "C" fn vesper_region_close(key: i64) -> u64 {
    with(|n| {
        let id = n
            .active
            .get_mut(&key)
            .and_then(Vec::pop)
            .ok_or(RegionFault::Inactive)?;
        n.regions.close(id).map(|()| 0)
    })
}
pub extern "C" fn vesper_region_box(key: i64, word: i64) -> u64 {
    with(|n| {
        let id = top(n, key)?;
        n.regions.alloc(id, word, false)
    })
}
pub extern "C" fn vesper_region_get(key: i64, handle: u64) -> i64 {
    with(|n| {
        let id = top(n, key)?;
        n.regions.get(id, handle).copied()
    })
}
pub extern "C" fn vesper_region_set(key: i64, handle: u64, word: i64) -> u64 {
    with(|n| {
        let id = top(n, key)?;
        n.regions.set(id, handle, word).map(|()| 0)
    })
}
pub extern "C" fn vesper_region_take(key: i64, handle: u64) -> i64 {
    with(|n| {
        let id = top(n, key)?;
        n.regions.take(id, handle)
    })
}
pub extern "C" fn vesper_region_checkpoint(key: i64) -> u64 {
    with(|n| {
        let id = top(n, key)?;
        n.regions.checkpoint(id)
    })
}
pub extern "C" fn vesper_region_rewind(key: i64, mark: u64) -> u64 {
    with(|n| {
        let id = top(n, key)?;
        n.regions.rewind(id, mark).map(|()| 0)
    })
}
pub extern "C" fn vesper_region_commit(key: i64, mark: u64) -> u64 {
    with(|n| {
        let id = top(n, key)?;
        n.regions.commit(id, mark).map(|()| 0)
    })
}
pub extern "C" fn vesper_region_used(key: i64) -> u64 {
    with(|n| {
        let id = top(n, key)?;
        n.regions.used(id)
    })
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
