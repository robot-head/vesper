//! VirtualArena: one contiguous reserved address range with a bump cursor
//! (§15.3, Tier 2).
//!
//! On Linux and macOS (outside Miri) the reservation is an anonymous private
//! `mmap` (`MAP_NORESERVE` on Linux), so physical pages are only faulted in
//! when first written. [`VirtualArena::reset`] returns the touched pages with
//! `madvise(MADV_FREE)` (falling back to `MADV_DONTNEED` where `MADV_FREE` is
//! refused). Elsewhere, and under Miri, the range comes from the global
//! allocator, which commits it eagerly; [`VirtualArena::backing_kind`] says
//! which path is in use.
//!
//! Because the range never moves, the allocation at the tip can grow in place:
//! [`VirtualArena::extend`] and [`VirtualVec`] never copy existing bytes. A
//! collection that is not at the tip cannot grow ([`VirtualArenaError::NotTip`]);
//! there is no relocation fallback.
//!
//! Every allocation is zeroed when it is handed out (pages released by
//! `MADV_FREE` may still hold old data until written). Handles carry the arena
//! identity and a never-reused serial; rewind, reset and drop make covered
//! handles stale.
#[cfg(not(all(unix, not(miri))))]
use std::alloc::{self, Layout};
use std::fmt;
use std::marker::PhantomData;
use std::mem::{align_of, size_of};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

/// Alignment of the reservation base and the largest allocation alignment.
pub const VIRTUAL_PAGE: usize = 4096;

static NEXT_ARENA: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualArenaConfig {
    /// Bytes of address space to reserve (rounded up to [`VIRTUAL_PAGE`]).
    pub reserve_bytes: usize,
    pub max_allocations: usize,
    pub max_checkpoints: usize,
}

impl VirtualArenaConfig {
    pub fn reserve(reserve_bytes: usize) -> Self {
        Self {
            reserve_bytes,
            max_allocations: 1 << 20,
            max_checkpoints: 1024,
        }
    }
}

/// How the address range was obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VirtualBackingKind {
    /// Anonymous `mmap`; pages fault in lazily and are released on reset.
    Mmap,
    /// Global allocator (`alloc_zeroed`); no lazy reservation guarantee.
    GlobalAllocator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VirtualArenaError {
    InvalidConfig(&'static str),
    ReserveFailed,
    InvalidAlignment {
        align: usize,
    },
    SizeOverflow,
    Exhausted {
        requested: usize,
        available: usize,
    },
    TooManyAllocations {
        limit: usize,
    },
    TooManyCheckpoints {
        limit: usize,
    },
    CrossArena,
    Stale,
    /// Only the newest allocation can grow in place.
    NotTip,
    /// The allocation predates the newest checkpoint; bytes added now would be
    /// discarded by a rewind while the allocation survives it.
    ProtectedByWatermark,
}

impl fmt::Display for VirtualArenaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for VirtualArenaError {}

/// Failed push: the value is returned.
#[derive(Debug, PartialEq, Eq)]
pub struct VirtualPushError<T> {
    pub value: T,
    pub error: VirtualArenaError,
}

/// Counters for one arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VirtualArenaStats {
    pub reserved_bytes: usize,
    pub used_bytes: usize,
    /// High-water mark of bytes written since the last reset.
    pub touched_bytes: usize,
    pub allocations: usize,
    pub resets: u64,
    pub in_place_growths: u64,
}

/// Copyable checked handle to a zeroed byte range in a [`VirtualArena`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VirtualBytes {
    arena: u64,
    index: u32,
    serial: u64,
}

/// Linear checkpoint token of a [`VirtualArena`].
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a checkpoint must be rewound or committed"]
pub struct VirtualMark {
    arena: u64,
    serial: u64,
}

/// A contiguous `Copy` vector at the tip of a [`VirtualArena`]. Unique owner;
/// growth extends its byte range in place and never copies elements.
/// Elements must be `Send + Sync`, because the arena is `Send + Sync` and
/// hands out element views through `&VirtualArena`:
///
/// ```compile_fail
/// use std::cell::Cell;
/// use vesper_alloc::{VirtualArena, VirtualArenaConfig};
/// let mut a = VirtualArena::new(VirtualArenaConfig::reserve(4096)).unwrap();
/// let cell: &'static Cell<usize> = Box::leak(Box::new(Cell::new(0)));
/// let mut v = a.vec_with_capacity::<&'static Cell<usize>>(1).unwrap();
/// ```
#[must_use = "dropping the owner does not free arena bytes"]
pub struct VirtualVec<T: Copy + Send + Sync + 'static> {
    bytes: VirtualBytes,
    len: usize,
    cap: usize,
    _elem: PhantomData<fn(T) -> T>,
}

impl<T: Copy + Send + Sync + 'static> fmt::Debug for VirtualVec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtualVec")
            .field("len", &self.len)
            .field("cap", &self.cap)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
struct Alloc {
    start: usize,
    len: usize,
    serial: u64,
}

struct Mark {
    serial: u64,
    cursor: usize,
    allocs: usize,
}

pub struct VirtualArena {
    id: u64,
    base: NonNull<u8>,
    reserved: usize,
    kind: VirtualBackingKind,
    cursor: usize,
    touched: usize,
    allocs: Vec<Alloc>,
    marks: Vec<Mark>,
    next_serial: u64,
    config: VirtualArenaConfig,
    resets: u64,
    growths: u64,
}

// SAFETY: the arena uniquely owns its reservation; all access to the bytes
// goes through `&self`/`&mut self`.
unsafe impl Send for VirtualArena {}
// SAFETY: `&VirtualArena` only exposes shared byte slices.
unsafe impl Sync for VirtualArena {}

impl fmt::Debug for VirtualArena {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtualArena")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("reserved", &self.reserved)
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

#[cfg(all(unix, not(miri)))]
mod os {
    use std::ptr::NonNull;

    pub fn reserve(len: usize) -> Option<NonNull<u8>> {
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

    /// # Safety
    /// `[ptr, ptr+len)` lies in a mapping from `reserve`, with no live borrows.
    pub unsafe fn release_pages(ptr: NonNull<u8>, len: usize) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // SAFETY: caller contract; madvise does not unmap.
            let rc = unsafe { libc::madvise(ptr.as_ptr().cast(), len, libc::MADV_FREE) };
            if rc == 0 {
                return;
            }
        }
        // SAFETY: caller contract.
        unsafe { libc::madvise(ptr.as_ptr().cast(), len, libc::MADV_DONTNEED) };
    }

    /// # Safety
    /// `ptr`/`len` are exactly a mapping returned by `reserve`.
    pub unsafe fn unreserve(ptr: NonNull<u8>, len: usize) {
        // SAFETY: caller contract.
        unsafe { libc::munmap(ptr.as_ptr().cast(), len) };
    }
}

impl VirtualArena {
    pub fn new(config: VirtualArenaConfig) -> Result<Self, VirtualArenaError> {
        if config.reserve_bytes == 0 {
            return Err(VirtualArenaError::InvalidConfig(
                "reserve_bytes must be nonzero",
            ));
        }
        if config.max_allocations == 0 || config.max_allocations > u32::MAX as usize {
            return Err(VirtualArenaError::InvalidConfig(
                "max_allocations must be 1..=u32::MAX",
            ));
        }
        let reserved = config
            .reserve_bytes
            .checked_next_multiple_of(VIRTUAL_PAGE)
            .filter(|n| *n <= isize::MAX as usize)
            .ok_or(VirtualArenaError::SizeOverflow)?;
        let id = {
            let mut id = NEXT_ARENA.load(Ordering::Relaxed);
            loop {
                let next = id.checked_add(1).ok_or(VirtualArenaError::InvalidConfig(
                    "arena identities exhausted",
                ))?;
                match NEXT_ARENA.compare_exchange_weak(
                    id,
                    next,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break id,
                    Err(current) => id = current,
                }
            }
        };
        let (base, kind) = Self::reserve(reserved)?;
        Ok(Self {
            id,
            base,
            reserved,
            kind,
            cursor: 0,
            touched: 0,
            allocs: Vec::new(),
            marks: Vec::new(),
            next_serial: 1,
            config,
            resets: 0,
            growths: 0,
        })
    }

    #[cfg(all(unix, not(miri)))]
    fn reserve(len: usize) -> Result<(NonNull<u8>, VirtualBackingKind), VirtualArenaError> {
        os::reserve(len)
            .map(|p| (p, VirtualBackingKind::Mmap))
            .ok_or(VirtualArenaError::ReserveFailed)
    }

    #[cfg(not(all(unix, not(miri))))]
    fn reserve(len: usize) -> Result<(NonNull<u8>, VirtualBackingKind), VirtualArenaError> {
        let layout = Layout::from_size_align(len, VIRTUAL_PAGE)
            .map_err(|_| VirtualArenaError::SizeOverflow)?;
        // SAFETY: nonzero size.
        NonNull::new(unsafe { alloc::alloc_zeroed(layout) })
            .map(|p| (p, VirtualBackingKind::GlobalAllocator))
            .ok_or(VirtualArenaError::ReserveFailed)
    }

    pub fn backing_kind(&self) -> VirtualBackingKind {
        self.kind
    }

    pub fn reserved_bytes(&self) -> usize {
        self.reserved
    }

    pub fn stats(&self) -> VirtualArenaStats {
        VirtualArenaStats {
            reserved_bytes: self.reserved,
            used_bytes: self.cursor,
            touched_bytes: self.touched,
            allocations: self.allocs.len(),
            resets: self.resets,
            in_place_growths: self.growths,
        }
    }

    fn next_serial(&mut self) -> Result<u64, VirtualArenaError> {
        let s = self.next_serial;
        self.next_serial = s
            .checked_add(1)
            .ok_or(VirtualArenaError::InvalidConfig("serials exhausted"))?;
        Ok(s)
    }

    /// Zeroes `[start, end)` and records the high-water mark.
    fn zero(&mut self, start: usize, end: usize) {
        debug_assert!(start <= end && end <= self.reserved);
        // SAFETY: inside the reservation; no handle can borrow these bytes
        // while `&mut self` is held.
        unsafe { self.base.as_ptr().add(start).write_bytes(0, end - start) };
        self.touched = self.touched.max(end);
    }

    /// Allocates `len` zeroed bytes aligned to `align` (at most a page).
    pub fn alloc_bytes(
        &mut self,
        len: usize,
        align: usize,
    ) -> Result<VirtualBytes, VirtualArenaError> {
        if !align.is_power_of_two() || align > VIRTUAL_PAGE {
            return Err(VirtualArenaError::InvalidAlignment { align });
        }
        if self.allocs.len() >= self.config.max_allocations {
            return Err(VirtualArenaError::TooManyAllocations {
                limit: self.config.max_allocations,
            });
        }
        let start = self
            .cursor
            .checked_next_multiple_of(align)
            .ok_or(VirtualArenaError::SizeOverflow)?;
        let end = start
            .checked_add(len)
            .ok_or(VirtualArenaError::SizeOverflow)?;
        if end > self.reserved {
            return Err(VirtualArenaError::Exhausted {
                requested: len,
                available: self.reserved.saturating_sub(start),
            });
        }
        self.allocs
            .try_reserve(1)
            .map_err(|_| VirtualArenaError::Exhausted {
                requested: len,
                available: 0,
            })?;
        let serial = self.next_serial()?;
        self.zero(start, end);
        let index = self.allocs.len() as u32;
        self.allocs.push(Alloc { start, len, serial });
        self.cursor = end;
        Ok(VirtualBytes {
            arena: self.id,
            index,
            serial,
        })
    }

    fn resolve(&self, h: &VirtualBytes) -> Result<Alloc, VirtualArenaError> {
        if h.arena != self.id {
            return Err(VirtualArenaError::CrossArena);
        }
        self.allocs
            .get(h.index as usize)
            .copied()
            .filter(|a| a.serial == h.serial)
            .ok_or(VirtualArenaError::Stale)
    }

    pub fn len(&self, h: &VirtualBytes) -> Result<usize, VirtualArenaError> {
        Ok(self.resolve(h)?.len)
    }

    pub fn bytes(&self, h: &VirtualBytes) -> Result<&[u8], VirtualArenaError> {
        let a = self.resolve(h)?;
        // SAFETY: a live allocation inside the reservation; zeroed or written.
        Ok(unsafe { std::slice::from_raw_parts(self.base.as_ptr().add(a.start), a.len) })
    }

    pub fn bytes_mut(&mut self, h: &VirtualBytes) -> Result<&mut [u8], VirtualArenaError> {
        let a = self.resolve(h)?;
        // SAFETY: as above, exclusive through `&mut self`.
        Ok(unsafe { std::slice::from_raw_parts_mut(self.base.as_ptr().add(a.start), a.len) })
    }

    /// Address of an allocation, for in-place growth checks. Inspect only.
    pub fn bytes_addr(&self, h: &VirtualBytes) -> Result<usize, VirtualArenaError> {
        let a = self.resolve(h)?;
        Ok(self.base.as_ptr() as usize + a.start)
    }

    /// Grows the newest allocation in place by `additional` zeroed bytes.
    pub fn extend(&mut self, h: &VirtualBytes, additional: usize) -> Result<(), VirtualArenaError> {
        let a = self.resolve(h)?;
        if h.index as usize + 1 != self.allocs.len() {
            return Err(VirtualArenaError::NotTip);
        }
        if self
            .marks
            .last()
            .is_some_and(|m| m.allocs > h.index as usize)
        {
            return Err(VirtualArenaError::ProtectedByWatermark);
        }
        let end = a.start + a.len;
        let new_end = end
            .checked_add(additional)
            .ok_or(VirtualArenaError::SizeOverflow)?;
        if new_end > self.reserved {
            return Err(VirtualArenaError::Exhausted {
                requested: additional,
                available: self.reserved - end,
            });
        }
        self.zero(end, new_end);
        self.allocs[h.index as usize].len += additional;
        self.cursor = new_end;
        self.growths += 1;
        Ok(())
    }

    pub fn checkpoint(&mut self) -> Result<VirtualMark, VirtualArenaError> {
        if self.marks.len() >= self.config.max_checkpoints {
            return Err(VirtualArenaError::TooManyCheckpoints {
                limit: self.config.max_checkpoints,
            });
        }
        self.marks
            .try_reserve(1)
            .map_err(|_| VirtualArenaError::TooManyCheckpoints {
                limit: self.marks.len(),
            })?;
        let serial = self.next_serial()?;
        self.marks.push(Mark {
            serial,
            cursor: self.cursor,
            allocs: self.allocs.len(),
        });
        Ok(VirtualMark {
            arena: self.id,
            serial,
        })
    }

    fn find_mark(&self, mark: &VirtualMark) -> Result<usize, VirtualArenaError> {
        if mark.arena != self.id {
            return Err(VirtualArenaError::CrossArena);
        }
        self.marks
            .iter()
            .position(|m| m.serial == mark.serial)
            .ok_or(VirtualArenaError::Stale)
    }

    /// O(1) bump reset to the checkpoint; newer handles and marks go stale.
    pub fn rewind(&mut self, mark: VirtualMark) -> Result<(), VirtualArenaError> {
        let i = self.find_mark(&mark)?;
        let m = &self.marks[i];
        self.cursor = m.cursor;
        self.allocs.truncate(m.allocs);
        self.marks.truncate(i);
        Ok(())
    }

    /// Discards the checkpoint and keeps everything allocated since.
    pub fn commit(&mut self, mark: VirtualMark) -> Result<(), VirtualArenaError> {
        let i = self.find_mark(&mark)?;
        self.marks.remove(i);
        Ok(())
    }

    /// Invalidates every handle and mark and returns touched pages to the OS
    /// (mmap path). The reservation is kept.
    pub fn reset(&mut self) {
        self.allocs.clear();
        self.marks.clear();
        self.cursor = 0;
        #[cfg(all(unix, not(miri)))]
        if self.touched > 0 {
            let len = self
                .touched
                .next_multiple_of(VIRTUAL_PAGE)
                .min(self.reserved);
            // SAFETY: inside our mapping; no handle can borrow bytes because
            // every allocation was just invalidated under `&mut self`.
            unsafe { os::release_pages(self.base, len) };
        }
        self.touched = 0;
        self.resets += 1;
    }

    // ----- VirtualVec ------------------------------------------------------

    fn elem_layout<T>() -> Result<(), VirtualArenaError> {
        if size_of::<T>() == 0 {
            return Err(VirtualArenaError::InvalidConfig(
                "zero-sized VirtualVec elements are unsupported",
            ));
        }
        if align_of::<T>() > VIRTUAL_PAGE {
            return Err(VirtualArenaError::InvalidAlignment {
                align: align_of::<T>(),
            });
        }
        Ok(())
    }

    /// New vector with room for `cap` elements, allocated at the tip.
    pub fn vec_with_capacity<T: Copy + Send + Sync + 'static>(
        &mut self,
        cap: usize,
    ) -> Result<VirtualVec<T>, VirtualArenaError> {
        Self::elem_layout::<T>()?;
        let bytes = cap
            .checked_mul(size_of::<T>())
            .ok_or(VirtualArenaError::SizeOverflow)?;
        let h = self.alloc_bytes(bytes, align_of::<T>())?;
        Ok(VirtualVec {
            bytes: h,
            len: 0,
            cap,
            _elem: PhantomData,
        })
    }

    pub fn vec_len<T: Copy + Send + Sync + 'static>(
        &self,
        v: &VirtualVec<T>,
    ) -> Result<usize, VirtualArenaError> {
        self.resolve(&v.bytes)?;
        Ok(v.len)
    }

    pub fn vec_capacity<T: Copy + Send + Sync + 'static>(
        &self,
        v: &VirtualVec<T>,
    ) -> Result<usize, VirtualArenaError> {
        self.resolve(&v.bytes)?;
        Ok(v.cap)
    }

    /// Address of element 0; constant across in-place growth. Inspect only.
    pub fn vec_addr<T: Copy + Send + Sync + 'static>(
        &self,
        v: &VirtualVec<T>,
    ) -> Result<usize, VirtualArenaError> {
        self.bytes_addr(&v.bytes)
    }

    pub fn vec_as_slice<T: Copy + Send + Sync + 'static>(
        &self,
        v: &VirtualVec<T>,
    ) -> Result<&[T], VirtualArenaError> {
        let a = self.resolve(&v.bytes)?;
        debug_assert!(v.len * size_of::<T>() <= a.len);
        // SAFETY: the range is aligned for T (allocated with align_of::<T>())
        // and its first `len` elements were written by `vec_push`.
        Ok(unsafe { std::slice::from_raw_parts(self.base.as_ptr().add(a.start).cast(), v.len) })
    }

    pub fn vec_as_mut_slice<T: Copy + Send + Sync + 'static>(
        &mut self,
        v: &VirtualVec<T>,
    ) -> Result<&mut [T], VirtualArenaError> {
        let a = self.resolve(&v.bytes)?;
        // SAFETY: as in `vec_as_slice`, exclusive through `&mut self`.
        Ok(
            unsafe {
                std::slice::from_raw_parts_mut(self.base.as_ptr().add(a.start).cast(), v.len)
            },
        )
    }

    /// Appends, doubling capacity in place when full. Never copies elements.
    pub fn vec_push<T: Copy + Send + Sync + 'static>(
        &mut self,
        v: &mut VirtualVec<T>,
        value: T,
    ) -> Result<(), VirtualPushError<T>> {
        let fail = |error| VirtualPushError { value, error };
        let a = self.resolve(&v.bytes).map_err(fail)?;
        if v.len == v.cap {
            let add = v.cap.max(4);
            let add_bytes = add
                .checked_mul(size_of::<T>())
                .ok_or(VirtualArenaError::SizeOverflow)
                .map_err(fail)?;
            self.extend(&v.bytes, add_bytes).map_err(fail)?;
            v.cap += add;
        }
        // SAFETY: `len < cap` and the range holds `cap` aligned T slots.
        unsafe {
            self.base
                .as_ptr()
                .add(a.start)
                .cast::<T>()
                .add(v.len)
                .write(value)
        };
        v.len += 1;
        Ok(())
    }

    pub fn vec_pop<T: Copy + Send + Sync + 'static>(
        &mut self,
        v: &mut VirtualVec<T>,
    ) -> Result<Option<T>, VirtualArenaError> {
        let s = self.vec_as_slice(v)?;
        let out = s.last().copied();
        if out.is_some() {
            v.len -= 1;
        }
        Ok(out)
    }
}

impl Drop for VirtualArena {
    fn drop(&mut self) {
        #[cfg(all(unix, not(miri)))]
        // SAFETY: exactly the mapping from `reserve`; `&mut self` ends every
        // borrow of it.
        unsafe {
            os::unreserve(self.base, self.reserved)
        };
        #[cfg(not(all(unix, not(miri))))]
        // SAFETY: allocated in `reserve` with this layout.
        unsafe {
            alloc::dealloc(
                self.base.as_ptr(),
                Layout::from_size_align_unchecked(self.reserved, VIRTUAL_PAGE),
            )
        };
    }
}
