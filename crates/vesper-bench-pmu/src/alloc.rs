//! Counting global allocator for zero-allocation evidence (§41.1 item 4).
//!
//! [`CountingAllocator`] forwards every call to [`std::alloc::System`] and
//! counts allocations, reallocations, deallocations and requested bytes, both
//! process-wide (relaxed atomics) and for the calling thread (a const,
//! destructor-free thread local, which never allocates). A binary opts in
//! with `#[global_allocator] static A: CountingAllocator = CountingAllocator;`.
//! [`AllocationObserver`] reads the calling thread's counters around each
//! benchmark window, so concurrent threads (a test harness, a logger) do not
//! leak into the evidence; work the measured code hands to another thread is
//! not attributed to it either, and the report says so. A process that has
//! not installed the allocator gets [`UnavailableReason::AllocatorNotInstalled`]
//! from a probe allocation instead of a fabricated zero.
#![allow(unsafe_code)]
use crate::{Unavailable, UnavailableReason};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_bench::{Invocation, WindowCompletion, WindowObserver};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static REALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
thread_local! {
    // [allocations, reallocations, deallocations, bytes] of this thread.
    static THREAD: Cell<[u64; 4]> = const { Cell::new([0; 4]) };
}
fn bump(slot: usize, by: u64) {
    // try_with: never panic inside the allocator, even during thread exit.
    let _ = THREAD.try_with(|t| {
        let mut v = t.get();
        v[slot] = v[slot].wrapping_add(by);
        t.set(v);
    });
}

/// `System` plus process-wide counters.
pub struct CountingAllocator;

// SAFETY: every method forwards its arguments unchanged to `System`, which
// upholds the GlobalAlloc contract; the counters are lock-free atomics and
// never allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        bump(0, 1);
        bump(3, layout.size() as u64);
        // SAFETY: caller's contract is forwarded.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        bump(0, 1);
        bump(3, layout.size() as u64);
        // SAFETY: caller's contract is forwarded.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        bump(2, 1);
        // SAFETY: caller's contract is forwarded.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        REALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        bump(1, 1);
        bump(3, new_size as u64);
        // SAFETY: caller's contract is forwarded.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Cumulative process-wide counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationCounts {
    pub allocations: u64,
    pub reallocations: u64,
    pub deallocations: u64,
    pub bytes: u64,
}
impl AllocationCounts {
    /// Counts of the calling thread only.
    pub fn thread_now() -> Self {
        let [allocations, reallocations, deallocations, bytes] =
            THREAD.try_with(Cell::get).unwrap_or([0; 4]);
        Self {
            allocations,
            reallocations,
            deallocations,
            bytes,
        }
    }
    /// Process-wide counts.
    pub fn now() -> Self {
        Self {
            allocations: ALLOCATIONS.load(Ordering::Relaxed),
            reallocations: REALLOCATIONS.load(Ordering::Relaxed),
            deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
            bytes: BYTES.load(Ordering::Relaxed),
        }
    }
    /// Counts accumulated since `earlier`; counters only grow (wrapping
    /// after 2^64 events is reported as a regression, not a small delta).
    pub fn since(self, earlier: Self) -> Result<Self, Unavailable> {
        let sub = |a: u64, b: u64| {
            a.checked_sub(b).ok_or(Unavailable::new(
                UnavailableReason::CounterRegression,
                "allocation counters",
            ))
        };
        Ok(Self {
            allocations: sub(self.allocations, earlier.allocations)?,
            reallocations: sub(self.reallocations, earlier.reallocations)?,
            deallocations: sub(self.deallocations, earlier.deallocations)?,
            bytes: sub(self.bytes, earlier.bytes)?,
        })
    }
    /// No allocation or reallocation (deallocations of earlier blocks are
    /// reported but do not make a window allocate).
    pub fn is_zero_allocation(&self) -> bool {
        self.allocations == 0 && self.reallocations == 0
    }
}

/// Whether [`CountingAllocator`] is this process's global allocator, by a
/// real probe allocation.
pub fn counting_allocator_installed() -> bool {
    let before = AllocationCounts::thread_now().allocations;
    let probe = std::hint::black_box(Box::new(std::hint::black_box(0x5au8)));
    let after = AllocationCounts::thread_now().allocations;
    drop(probe);
    after > before
}

/// Allocation counts for one benchmark window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocationDelta {
    pub counts: AllocationCounts,
}

/// Reads the calling thread's counters around each window. Not Send/Sync, like the PMU providers.
pub struct AllocationObserver {
    active: bool,
    _thread_bound: PhantomData<Rc<()>>,
}
/// Counts at the start of one window.
pub struct AllocationToken {
    start: AllocationCounts,
    _thread_bound: PhantomData<Rc<()>>,
}
impl AllocationObserver {
    pub fn open() -> Result<Self, Unavailable> {
        if !counting_allocator_installed() {
            return Err(Unavailable::new(
                UnavailableReason::AllocatorNotInstalled,
                "counting global allocator",
            ));
        }
        Ok(Self {
            active: false,
            _thread_bound: PhantomData,
        })
    }
}
impl WindowObserver for AllocationObserver {
    type Token = AllocationToken;
    type Observation = AllocationDelta;
    type Error = Unavailable;
    fn before_window(&mut self, _: Invocation, _: usize) -> Result<AllocationToken, Unavailable> {
        if self.active {
            return Err(Unavailable::new(
                UnavailableReason::InvalidState,
                "allocation before_window",
            ));
        }
        self.active = true;
        Ok(AllocationToken {
            start: AllocationCounts::thread_now(),
            _thread_bound: PhantomData,
        })
    }
    fn after_window(
        &mut self,
        token: AllocationToken,
        _: WindowCompletion,
    ) -> Result<AllocationDelta, Unavailable> {
        let end = AllocationCounts::thread_now();
        if !self.active {
            return Err(Unavailable::new(
                UnavailableReason::ForeignOrStaleToken,
                "allocation after_window",
            ));
        }
        self.active = false;
        Ok(AllocationDelta {
            counts: end.since(token.start)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn without_the_allocator_installed_evidence_is_unavailable_not_zero() {
        // Unit tests run with the default global allocator.
        assert!(!counting_allocator_installed());
        assert_eq!(
            AllocationObserver::open().err().map(|e| e.reason),
            Some(UnavailableReason::AllocatorNotInstalled)
        );
        let a = AllocationCounts {
            allocations: 1,
            ..AllocationCounts::default()
        };
        assert!(AllocationCounts::default().since(a).is_err());
        assert!(
            !a.since(AllocationCounts::default())
                .unwrap()
                .is_zero_allocation()
        );
    }
}
