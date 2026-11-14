//! Pinned fiber stacks on Windows x86_64.
//!
//! One `VirtualAlloc` reservation per stack, committed eagerly, laid out from
//! low to high addresses as:
//!
//! * the overflow reserve: [`OVERFLOW_RESERVE`] bytes of committed
//!   read/write memory. When a fiber overflows, Windows raises
//!   `STATUS_STACK_OVERFLOW` and dispatches it *on the fiber stack*; this is
//!   the room that exception dispatch, vectored handlers (Rust's stack
//!   overflow reporter and [`crate::fault`]) and the unhandled-exception path
//!   run in. It matches the 20 KiB `SetThreadStackGuarantee` that Rust's
//!   standard library requests for its own threads.
//! * one `PAGE_GUARD` page directly below the usable region;
//! * the usable stack, `[base, top)`.
//!
//! [`FiberStack::guard_len`] is the size of the reserve plus the guard page:
//! everything below [`FiberStack::base`] that a well-behaved fiber never uses.
//!
//! Windows only recognises a guard-page hit as a stack overflow when the
//! faulting address lies inside the *current thread's* stack as recorded in
//! the TEB (`NT_TIB::StackBase`, `NT_TIB::StackLimit`, `DeallocationStack`,
//! `GuaranteedStackBytes`). The switch (`switch_windows.rs`) therefore saves
//! and restores those four fields with every context, and
//! [`FiberStack::teb_limits`] supplies a new fiber's values. Without that, an
//! overflow would surface as a one-shot `STATUS_GUARD_PAGE_VIOLATION` and
//! exception dispatch would reject every fiber frame as lying outside the
//! thread's stack.
//!
//! Commit is eager (the whole reservation is charged against the commit
//! limit at creation) rather than grown on demand through guard pages, so a
//! fiber never fails to grow mid-run: the cost is `size + guard_len` bytes of
//! commit per fiber, about 280 KiB at the default size. As on Linux and
//! macOS, the guard page is not a substitute for stack-size analysis.
use std::io;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_GUARD, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    VirtualProtect,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

pub const DEFAULT_STACK_SIZE: usize = 256 * 1024;

/// Committed bytes below the guard page that remain available to exception
/// dispatch after an overflow; also each fiber's `GuaranteedStackBytes`.
pub const OVERFLOW_RESERVE: usize = 0x5000;

/// Initial TEB stack fields of a fiber (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TebLimits {
    /// `NT_TIB::StackBase`: one past the highest usable address.
    pub stack_base: u64,
    /// `NT_TIB::StackLimit`: lowest committed usable address.
    pub stack_limit: u64,
    /// `TEB::DeallocationStack`: start of the reservation.
    pub deallocation_stack: u64,
    /// `TEB::GuaranteedStackBytes`.
    pub guaranteed_bytes: u32,
}

pub struct FiberStack {
    mapping: *mut u8,
    mapping_len: usize,
    guard_len: usize,
    /// Bytes at the start of the reservation, below the overflow reserve,
    /// that hold the fiber context of a fixed-address stack (zero otherwise).
    header_len: usize,
}

fn page_size() -> usize {
    // SAFETY: GetSystemInfo only writes the provided struct.
    let info = unsafe {
        let mut info: SYSTEM_INFO = std::mem::zeroed();
        GetSystemInfo(&mut info);
        info
    };
    match info.dwPageSize as usize {
        0 => 4096,
        n => n,
    }
}

fn allocation_granularity() -> usize {
    // SAFETY: GetSystemInfo only writes the provided struct.
    let info = unsafe {
        let mut info: SYSTEM_INFO = std::mem::zeroed();
        GetSystemInfo(&mut info);
        info
    };
    match info.dwAllocationGranularity as usize {
        0 => 64 * 1024,
        n => n,
    }
}

/// `(mapping_len, guard_len, header_len, reserve)` for `size` usable bytes.
fn layout(size: usize, fixed: bool) -> io::Result<(usize, usize, usize, usize)> {
    let page = page_size();
    let usable = size
        .checked_add(page - 1)
        .ok_or_else(|| io::Error::other("stack size overflow"))?
        & !(page - 1);
    if usable < 4 * page {
        return Err(io::Error::other("fiber stack must be at least four pages"));
    }
    // OVERFLOW_RESERVE is a multiple of 4 KiB; round it up for larger pages.
    let reserve = (OVERFLOW_RESERVE + page - 1) & !(page - 1);
    let guard_len = reserve + page;
    let header_len = if fixed { page } else { 0 };
    let mapping_len = usable
        .checked_add(guard_len)
        .and_then(|n| n.checked_add(header_len))
        .ok_or_else(|| io::Error::other("stack mapping size overflow"))?;
    Ok((mapping_len, guard_len, header_len, reserve))
}

impl FiberStack {
    pub fn new(size: usize) -> io::Result<Self> {
        crate::fault::install();
        let (mapping_len, guard_len, header_len, reserve) = layout(size, false)?;
        // SAFETY: a fresh private allocation with no address hint.
        let mapping = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                mapping_len,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        if mapping.is_null() {
            return Err(io::Error::last_os_error());
        }
        Self::guard(mapping.cast(), mapping_len, guard_len, header_len, reserve)
    }

    /// A stack whose reservation starts exactly at `reservation_base`, laid
    /// out from low to high as one context page (see
    /// [`Self::context_slot`]), the overflow reserve, the guard page and
    /// `size` usable bytes. `VirtualAlloc` with an explicit address never
    /// replaces an existing allocation: an occupied range fails with
    /// [`io::ErrorKind::AlreadyExists`]. The base must be a multiple of the
    /// allocation granularity (64 KiB), because `VirtualAlloc` would
    /// otherwise round it down to one.
    pub fn with_fixed_base(reservation_base: usize, size: usize) -> io::Result<Self> {
        crate::fault::install();
        if reservation_base == 0 || reservation_base % allocation_granularity() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixed stack base must be a non-null multiple of the allocation granularity",
            ));
        }
        let (mapping_len, guard_len, header_len, reserve) = layout(size, true)?;
        if reservation_base.checked_add(mapping_len).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixed stack reservation overflows the address space",
            ));
        }
        // SAFETY: an explicit-address reservation fails instead of replacing
        // memory owned by anyone else.
        let mapping = unsafe {
            VirtualAlloc(
                reservation_base as *const _,
                mapping_len,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        if mapping.is_null() {
            let error = io::Error::last_os_error();
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("fixed stack address range is in use: {error}"),
            ));
        }
        if mapping as usize != reservation_base {
            // SAFETY: releasing the allocation just created elsewhere.
            unsafe { VirtualFree(mapping, 0, MEM_RELEASE) };
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "fixed stack address range is in use",
            ));
        }
        Self::guard(mapping.cast(), mapping_len, guard_len, header_len, reserve)
    }

    fn guard(
        mapping: *mut u8,
        mapping_len: usize,
        guard_len: usize,
        header_len: usize,
        reserve: usize,
    ) -> io::Result<Self> {
        let page = page_size();
        let mut old = 0u32;
        // SAFETY: the guard page lies inside the allocation created above.
        let ok = unsafe {
            VirtualProtect(
                mapping.add(header_len + reserve).cast(),
                page,
                PAGE_READWRITE | PAGE_GUARD,
                &mut old,
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            // SAFETY: releasing the allocation we own.
            unsafe { VirtualFree(mapping.cast(), 0, MEM_RELEASE) };
            return Err(error);
        }
        Ok(Self {
            mapping,
            mapping_len,
            guard_len,
            header_len,
        })
    }

    /// An address that a fixed stack of `size` usable bytes could be
    /// reserved at right now (found by reserving and releasing one). Another
    /// allocation may take it first: [`Self::with_fixed_base`] then fails.
    pub fn free_fixed_base(size: usize) -> io::Result<usize> {
        let (mapping_len, ..) = layout(size, true)?;
        // SAFETY: a fresh reservation, released immediately.
        unsafe {
            let probe = VirtualAlloc(std::ptr::null(), mapping_len, MEM_RESERVE, PAGE_READWRITE);
            if probe.is_null() {
                return Err(io::Error::last_os_error());
            }
            VirtualFree(probe, 0, MEM_RELEASE);
            Ok(probe as usize)
        }
    }

    /// Lowest usable address (just above the guard page).
    pub fn base(&self) -> usize {
        self.mapping as usize + self.header_len + self.guard_len
    }
    /// One past the highest usable address; 16-byte aligned.
    pub fn top(&self) -> usize {
        self.mapping as usize + self.mapping_len
    }
    pub fn len(&self) -> usize {
        self.mapping_len - self.guard_len - self.header_len
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Bytes below [`Self::base`]: the overflow reserve plus the guard page.
    pub fn guard_len(&self) -> usize {
        self.guard_len
    }
    /// Host page size used for rounding and for the guard.
    pub fn page_size() -> usize {
        page_size()
    }
    pub fn contains(&self, address: u64) -> bool {
        (self.base() as u64..self.top() as u64).contains(&address)
    }
    /// Start of the reservation (the TEB's `DeallocationStack`).
    pub fn allocation_base(&self) -> usize {
        self.mapping as usize
    }
    /// Start of the whole reservation (context page, reserve, guard, stack).
    pub fn reservation_base(&self) -> usize {
        self.mapping as usize
    }
    /// Length of the whole reservation.
    pub fn reservation_len(&self) -> usize {
        self.mapping_len
    }
    /// For a fixed-address stack, the page-aligned, read/write start of its
    /// context page (see the Unix `FiberStack::context_slot`).
    pub fn context_slot(&self) -> Option<*mut u8> {
        (self.header_len != 0).then_some(self.mapping)
    }
    /// The TEB stack fields a fiber running on this stack starts with.
    pub fn teb_limits(&self) -> TebLimits {
        TebLimits {
            stack_base: self.top() as u64,
            stack_limit: self.base() as u64,
            deallocation_stack: self.mapping as u64,
            guaranteed_bytes: OVERFLOW_RESERVE as u32,
        }
    }
}

impl Drop for FiberStack {
    fn drop(&mut self) {
        // SAFETY: the allocation is owned and no frame on it is executing: a
        // fiber is only dropped while created, parked or exited.
        unsafe { VirtualFree(self.mapping.cast(), 0, MEM_RELEASE) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};

    fn protection(address: usize) -> u32 {
        // SAFETY: VirtualQuery only writes the provided struct.
        unsafe {
            let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            let n = VirtualQuery(
                address as *const _,
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            assert_ne!(n, 0);
            info.Protect
        }
    }

    #[test]
    fn stack_is_aligned_and_guarded() {
        let s = FiberStack::new(64 * 1024).unwrap();
        let page = page_size();
        assert_eq!(s.top() % 16, 0);
        assert_eq!(s.len(), 64 * 1024);
        assert_eq!(s.guard_len(), OVERFLOW_RESERVE + page);
        assert!(s.contains(s.base() as u64));
        assert!(!s.contains(s.top() as u64));
        assert!(!s.contains((s.base() - 1) as u64));
        assert_eq!(protection(s.base()), PAGE_READWRITE);
        assert_eq!(protection(s.top() - 1), PAGE_READWRITE);
        assert_eq!(protection(s.base() - 1), PAGE_READWRITE | PAGE_GUARD);
        assert_eq!(protection(s.base() - page - 1), PAGE_READWRITE);
        assert_eq!(protection(s.allocation_base()), PAGE_READWRITE);
        let limits = s.teb_limits();
        assert_eq!(limits.stack_base, s.top() as u64);
        assert_eq!(limits.stack_limit, s.base() as u64);
        assert_eq!(limits.deallocation_stack, s.allocation_base() as u64);
        assert_eq!(limits.guaranteed_bytes as usize, OVERFLOW_RESERVE);
        assert!(FiberStack::new(16).is_err());
        assert!(FiberStack::new(usize::MAX).is_err());
        assert!(FiberStack::new(usize::MAX - page + 1).is_err());
    }
}
