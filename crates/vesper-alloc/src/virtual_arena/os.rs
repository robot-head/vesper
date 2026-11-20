//! Platform address-space reservations shared by [`super::VirtualArena`] and
//! [`super::VirtualChunkBacking`].
//!
//! A [`Reservation`] is one contiguous range whose usable prefix
//! `[0, committed)` grows on demand:
//!
//! * Unix outside Miri: an anonymous private `mmap` (`MAP_NORESERVE` on
//!   Linux) with read/write protection, so "commit" only records the
//!   high-water mark and pages fault in lazily. Released tails go back with
//!   `madvise(MADV_FREE)` (falling back to `MADV_DONTNEED`). With the 2 MiB
//!   hint, the base is aligned to 2 MiB and Linux gets `madvise(MADV_HUGEPAGE)`.
//! * Windows outside Miri: `VirtualAlloc(MEM_RESERVE, PAGE_NOACCESS)`; the
//!   prefix is committed with `VirtualAlloc(MEM_COMMIT)` in 64 KiB steps and
//!   released tails are decommitted with `VirtualFree(MEM_DECOMMIT)`; drop
//!   releases with `VirtualFree(MEM_RELEASE)`.
//! * Elsewhere and under Miri: one eager `alloc_zeroed` from the global
//!   allocator.
use super::{HugePageStatus, VIRTUAL_PAGE, VirtualArenaError, VirtualBackingKind};
use std::ptr::NonNull;

/// Size of the huge-page hint and of the alignment it requests.
pub const HUGE_PAGE: usize = 2 << 20;

/// Granularity in which the Windows path commits pages.
#[cfg(all(windows, not(miri)))]
const COMMIT_STEP: usize = 64 * 1024;

pub(crate) struct Reserved {
    pub base: NonNull<u8>,
    pub mapping_len: usize,
    pub kind: VirtualBackingKind,
    pub huge: HugePageStatus,
    pub guard_page: bool,
}

pub(crate) struct Reservation {
    base: NonNull<u8>,
    len: usize,
    mapping_len: usize,
    committed: usize,
    kind: VirtualBackingKind,
    huge: HugePageStatus,
    guard_page: bool,
}

// SAFETY: a reservation uniquely owns its address range; access to the bytes
// is mediated by its owner through `&`/`&mut`.
unsafe impl Send for Reservation {}
// SAFETY: `&Reservation` exposes only the base address and counters.
unsafe impl Sync for Reservation {}

impl Reservation {
    /// Reserves `len` usable bytes (a nonzero multiple of [`VIRTUAL_PAGE`]).
    /// When `guard_page` is `true`, reserves an additional OS page at the end
    /// of the mapping and marks it inaccessible (`PROT_NONE` / `PAGE_NOACCESS`)
    /// so out-of-bounds writes trap immediately.
    pub fn new(len: usize, huge_hint: bool, guard_page: bool) -> Result<Self, VirtualArenaError> {
        debug_assert!(len > 0 && len.is_multiple_of(VIRTUAL_PAGE));
        let reserved = imp::reserve(len, huge_hint, guard_page)?;
        let committed = match reserved.kind {
            VirtualBackingKind::GlobalAllocator => len,
            _ => 0,
        };
        Ok(Self {
            base: reserved.base,
            len,
            mapping_len: reserved.mapping_len,
            committed,
            kind: reserved.kind,
            huge: reserved.huge,
            guard_page: reserved.guard_page,
        })
    }

    pub fn base(&self) -> NonNull<u8> {
        self.base
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn kind(&self) -> VirtualBackingKind {
        self.kind
    }

    pub fn huge(&self) -> HugePageStatus {
        self.huge
    }

    /// Whether a trailing OS guard page is active at the end of the mapping.
    pub fn has_guard_page(&self) -> bool {
        self.guard_page
    }

    /// Usable (Windows: committed; Unix: touched high-water) prefix length.
    pub fn committed(&self) -> usize {
        self.committed
    }

    /// Makes `[0, end)` readable and writable. Returns `false` (changing
    /// nothing) when the platform refuses to commit.
    pub fn commit(&mut self, end: usize) -> bool {
        debug_assert!(end <= self.len);
        if end <= self.committed {
            return true;
        }
        #[cfg(all(windows, not(miri)))]
        {
            let target = end.next_multiple_of(COMMIT_STEP).min(self.len);
            // SAFETY: `[committed, target)` lies inside our reservation.
            if !unsafe { imp::commit(self.base, self.committed, target) } {
                return false;
            }
            self.committed = target;
        }
        #[cfg(not(all(windows, not(miri))))]
        {
            self.committed = end.next_multiple_of(VIRTUAL_PAGE).min(self.len);
        }
        true
    }

    /// Returns the pages of `[from, committed)` to the operating system
    /// (rounded up to a page boundary at `from`). Their contents are lost.
    ///
    /// # Safety
    ///
    /// No live reference may point into `[from, committed)`.
    pub unsafe fn release_tail(&mut self, from: usize) {
        let start = from.next_multiple_of(VIRTUAL_PAGE).min(self.len);
        if start >= self.committed {
            return;
        }
        match self.kind {
            VirtualBackingKind::GlobalAllocator => {}
            _ => {
                // SAFETY: caller contract; the range lies inside our mapping.
                unsafe { imp::release(self.base, start, self.committed) };
                self.committed = start;
            }
        }
    }

    /// Bytes of this reservation currently backed by transparent huge pages,
    /// read from `/proc/self/smaps` (Linux only; `None` elsewhere or when the
    /// mapping cannot be found).
    pub fn resident_huge_bytes(&self) -> Option<usize> {
        #[cfg(all(target_os = "linux", not(miri)))]
        {
            smaps_anon_huge(self.base.as_ptr() as usize, self.len)
        }
        #[cfg(not(all(target_os = "linux", not(miri))))]
        {
            None
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // SAFETY: exactly the range from `reserve`; `&mut self` ends every
        // borrow of it.
        unsafe { imp::unreserve(self.base, self.mapping_len, self.kind) }
    }
}

/// Sums `AnonHugePages` over the smaps entries that overlap `[addr, addr+len)`.
#[cfg(all(target_os = "linux", not(miri)))]
fn smaps_anon_huge(addr: usize, len: usize) -> Option<usize> {
    let text = std::fs::read_to_string("/proc/self/smaps").ok()?;
    let end = addr.checked_add(len)?;
    let mut inside = false;
    let mut found = false;
    let mut total = 0usize;
    for line in text.lines() {
        let first = line.split_whitespace().next().unwrap_or("");
        if let Some((lo, hi)) = first.split_once('-')
            && let (Ok(lo), Ok(hi)) = (usize::from_str_radix(lo, 16), usize::from_str_radix(hi, 16))
        {
            inside = lo < end && addr < hi;
            found |= inside;
            continue;
        }
        if inside && let Some(rest) = line.strip_prefix("AnonHugePages:") {
            let kb: usize = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            total = total.checked_add(kb.checked_mul(1024)?)?;
        }
    }
    found.then_some(total)
}

#[cfg(all(unix, not(miri)))]
mod imp {
    use super::{
        HUGE_PAGE, HugePageStatus, Reserved, VIRTUAL_PAGE, VirtualArenaError, VirtualBackingKind,
    };
    use std::ptr::NonNull;

    fn page_size() -> usize {
        // SAFETY: sysconf has no preconditions.
        let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if n <= 0 { VIRTUAL_PAGE } else { n as usize }
    }

    fn map(len: usize) -> Option<NonNull<u8>> {
        #[cfg(target_os = "linux")]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
        #[cfg(not(target_os = "linux"))]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
        // SAFETY: anonymous private mapping at a kernel-chosen address.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                flags,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            None
        } else {
            NonNull::new(p.cast())
        }
    }

    pub fn reserve(
        len: usize,
        huge_hint: bool,
        guard_page: bool,
    ) -> Result<Reserved, VirtualArenaError> {
        let (guard_offset, guard_len, mapping_len) = if guard_page {
            let page = page_size().max(VIRTUAL_PAGE);
            let guard_offset = len
                .checked_next_multiple_of(page)
                .ok_or(VirtualArenaError::SizeOverflow)?;
            let mapping_len = guard_offset
                .checked_add(page)
                .filter(|n| *n <= isize::MAX as usize)
                .ok_or(VirtualArenaError::SizeOverflow)?;
            (guard_offset, page, mapping_len)
        } else {
            (len, 0, len)
        };

        let (base, huge) = if !huge_hint {
            let p = map(mapping_len).ok_or(VirtualArenaError::ReserveFailed)?;
            (p, HugePageStatus::NotRequested)
        } else {
            // Over-reserve by one huge page, then trim both ends so the base is
            // 2 MiB aligned.
            let padded = mapping_len
                .checked_add(HUGE_PAGE)
                .ok_or(VirtualArenaError::SizeOverflow)?;
            let raw = map(padded).ok_or(VirtualArenaError::ReserveFailed)?;
            let raw_addr = raw.as_ptr() as usize;
            let aligned = raw_addr.next_multiple_of(HUGE_PAGE);
            let head = aligned - raw_addr;
            let tail = padded - head - mapping_len;
            // SAFETY: both trimmed pieces lie inside the mapping just created and
            // nothing refers to them.
            unsafe {
                if head > 0 {
                    libc::munmap(raw.as_ptr().cast(), head);
                }
                if tail > 0 {
                    libc::munmap(raw.as_ptr().add(head + mapping_len).cast(), tail);
                }
            }
            // SAFETY: `head < padded`, so the pointer stays inside the mapping.
            let base = unsafe { NonNull::new_unchecked(raw.as_ptr().add(head)) };
            #[cfg(target_os = "linux")]
            let huge = {
                // SAFETY: advisory call over the usable prefix of our own mapping.
                let rc = unsafe { libc::madvise(base.as_ptr().cast(), len, libc::MADV_HUGEPAGE) };
                if rc == 0 {
                    HugePageStatus::Advised
                } else {
                    HugePageStatus::Refused {
                        errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
                    }
                }
            };
            #[cfg(not(target_os = "linux"))]
            let huge = HugePageStatus::AlignedOnly;
            (base, huge)
        };

        if guard_page {
            // SAFETY: `[guard_offset, guard_offset + guard_len)` is the
            // host-page-aligned trailing page inside our owned mapping.
            let rc = unsafe {
                libc::mprotect(
                    base.as_ptr().add(guard_offset).cast(),
                    guard_len,
                    libc::PROT_NONE,
                )
            };
            if rc != 0 {
                // SAFETY: unmapping the mapping we just created.
                unsafe { libc::munmap(base.as_ptr().cast(), mapping_len) };
                return Err(VirtualArenaError::ReserveFailed);
            }
        }

        Ok(Reserved {
            base,
            mapping_len,
            kind: VirtualBackingKind::Mmap,
            huge,
            guard_page,
        })
    }

    /// # Safety
    /// `[start, end)` lies in our mapping with no live borrows.
    pub unsafe fn release(base: NonNull<u8>, start: usize, end: usize) {
        // SAFETY: caller contract.
        let ptr = unsafe { base.as_ptr().add(start) };
        let len = end - start;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // SAFETY: caller contract; madvise does not unmap.
            let rc = unsafe { libc::madvise(ptr.cast(), len, libc::MADV_FREE) };
            if rc == 0 {
                return;
            }
        }
        // SAFETY: caller contract.
        unsafe { libc::madvise(ptr.cast(), len, libc::MADV_DONTNEED) };
    }

    /// # Safety
    /// `base`/`len` are exactly a range returned by `reserve`.
    pub unsafe fn unreserve(base: NonNull<u8>, len: usize, _kind: VirtualBackingKind) {
        // SAFETY: caller contract.
        unsafe { libc::munmap(base.as_ptr().cast(), len) };
    }
}

#[cfg(all(windows, not(miri)))]
mod imp {
    use super::{HugePageStatus, Reserved, VIRTUAL_PAGE, VirtualArenaError, VirtualBackingKind};
    use std::ffi::c_void;
    use std::ptr::NonNull;

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_DECOMMIT: u32 = 0x4000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_NOACCESS: u32 = 0x01;
    const PAGE_READWRITE: u32 = 0x04;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(
            address: *mut c_void,
            size: usize,
            allocation_type: u32,
            protect: u32,
        ) -> *mut c_void;
        fn VirtualProtect(
            address: *mut c_void,
            size: usize,
            new_protect: u32,
            old_protect: *mut u32,
        ) -> i32;
        fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
    }

    pub fn reserve(
        len: usize,
        huge_hint: bool,
        guard_page: bool,
    ) -> Result<Reserved, VirtualArenaError> {
        let mapping_len = if guard_page {
            len.checked_add(VIRTUAL_PAGE)
                .filter(|n| *n <= isize::MAX as usize)
                .ok_or(VirtualArenaError::SizeOverflow)?
        } else {
            len
        };
        // SAFETY: reserves fresh address space at a system-chosen address.
        let p = unsafe {
            VirtualAlloc(
                std::ptr::null_mut(),
                mapping_len,
                MEM_RESERVE,
                PAGE_NOACCESS,
            )
        };
        let p = NonNull::new(p.cast::<u8>()).ok_or(VirtualArenaError::ReserveFailed)?;
        if guard_page {
            // SAFETY: `[len, len + VIRTUAL_PAGE)` is the trailing guard page
            // inside our owned reservation. Commit and explicitly mark it
            // `PAGE_NOACCESS` via `VirtualProtect` so out-of-bounds writes trap.
            let guard_ptr = unsafe { p.as_ptr().add(len).cast::<c_void>() };
            let committed =
                unsafe { VirtualAlloc(guard_ptr, VIRTUAL_PAGE, MEM_COMMIT, PAGE_NOACCESS) };
            let mut old_protect: u32 = 0;
            let protected = if !committed.is_null() {
                unsafe { VirtualProtect(guard_ptr, VIRTUAL_PAGE, PAGE_NOACCESS, &mut old_protect) }
            } else {
                0
            };
            if committed.is_null() || protected == 0 {
                // SAFETY: releasing the reservation we just created.
                unsafe { VirtualFree(p.as_ptr().cast(), 0, MEM_RELEASE) };
                return Err(VirtualArenaError::ReserveFailed);
            }
        }
        // Large pages need SeLockMemoryPrivilege and are committed eagerly
        // (MEM_LARGE_PAGES), which defeats lazy reservation: not attempted.
        let huge = if huge_hint {
            HugePageStatus::Unsupported
        } else {
            HugePageStatus::NotRequested
        };
        Ok(Reserved {
            base: p,
            mapping_len,
            kind: VirtualBackingKind::VirtualAlloc,
            huge,
            guard_page,
        })
    }

    /// # Safety
    /// `[start, end)` lies inside the reservation at `base`.
    pub unsafe fn commit(base: NonNull<u8>, start: usize, end: usize) -> bool {
        // SAFETY: caller contract.
        let ptr = unsafe { base.as_ptr().add(start) };
        // SAFETY: commits pages of our own reservation.
        let p = unsafe { VirtualAlloc(ptr.cast(), end - start, MEM_COMMIT, PAGE_READWRITE) };
        !p.is_null()
    }

    /// # Safety
    /// `[start, end)` lies inside the committed reservation, no live borrows.
    pub unsafe fn release(base: NonNull<u8>, start: usize, end: usize) {
        // SAFETY: caller contract.
        let ptr = unsafe { base.as_ptr().add(start) };
        // SAFETY: decommits pages of our own reservation.
        unsafe { VirtualFree(ptr.cast(), end - start, MEM_DECOMMIT) };
    }

    /// # Safety
    /// `base` is exactly a reservation from `reserve`.
    pub unsafe fn unreserve(base: NonNull<u8>, _len: usize, _kind: VirtualBackingKind) {
        // SAFETY: caller contract; MEM_RELEASE requires size 0.
        unsafe { VirtualFree(base.as_ptr().cast(), 0, MEM_RELEASE) };
    }
}

#[cfg(not(any(all(unix, not(miri)), all(windows, not(miri)))))]
mod imp {
    use super::super::VIRTUAL_PAGE;
    use super::{HugePageStatus, Reserved, VirtualArenaError, VirtualBackingKind};
    use std::alloc::{self, Layout};
    use std::ptr::NonNull;

    pub fn reserve(
        len: usize,
        huge_hint: bool,
        _guard_page: bool,
    ) -> Result<Reserved, VirtualArenaError> {
        let layout = Layout::from_size_align(len, VIRTUAL_PAGE)
            .map_err(|_| VirtualArenaError::SizeOverflow)?;
        // SAFETY: nonzero size.
        let p = NonNull::new(unsafe { alloc::alloc_zeroed(layout) })
            .ok_or(VirtualArenaError::ReserveFailed)?;
        let huge = if huge_hint {
            HugePageStatus::Unsupported
        } else {
            HugePageStatus::NotRequested
        };
        Ok(Reserved {
            base: p,
            mapping_len: len,
            kind: VirtualBackingKind::GlobalAllocator,
            huge,
            guard_page: false,
        })
    }

    /// # Safety
    /// Never called for the global-allocator kind.
    pub unsafe fn release(_base: NonNull<u8>, _start: usize, _end: usize) {}

    /// # Safety
    /// `base`/`len` are exactly an allocation from `reserve`.
    pub unsafe fn unreserve(base: NonNull<u8>, len: usize, _kind: VirtualBackingKind) {
        // SAFETY: allocated in `reserve` with this layout.
        unsafe {
            alloc::dealloc(
                base.as_ptr(),
                Layout::from_size_align_unchecked(len, VIRTUAL_PAGE),
            )
        };
    }
}
