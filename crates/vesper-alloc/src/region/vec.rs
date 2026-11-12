//! Region-backed contiguous vectors.
//!
//! A [`RegionVec<T>`] is a unique, move-only handle to an element buffer that
//! is a tracked allocation of its region. The region records the buffer's
//! capacity and how many leading elements are initialized, so finalization by
//! rewind, reset, release or region drop never depends on a handle surviving.
//! Every element byte lives in region chunks; there is no fallback to a host
//! `Vec`.
//!
//! [`Region::vec_freeze`] turns a vector into a copyable, read-only
//! [`RegionSlice<T>`]. Copies of a slice handle name the same buffer and can
//! only produce shared views.
use super::{Elems, Region, RegionId, SlotKind, SlotState};
use crate::backing::ChunkBacking;
use crate::error::{AllocError, HandleError, InsertError, OwnerError, RegionError};
use std::fmt;
use std::marker::PhantomData;
use std::mem;
use std::ptr;

/// Unique owning handle to a region-backed growable vector
/// (`RegionVec[r, T]`).
///
/// The handle is neither `Clone` nor `Copy`; mutation additionally requires
/// `&mut Region`, so at most one mutable view of the elements exists. Growth
/// may relocate the buffer, in which case the method updates the handle in
/// place. Dropping or forgetting the handle does not drop the elements: they
/// are finalized by [`Region::vec_release`], rewind, reset or region drop.
///
/// The handle is invariant in `T`, so a stored borrowed type cannot be
/// reinterpreted at a shorter lifetime:
///
/// ```compile_fail
/// use vesper_alloc::RegionVec;
/// fn shorten<'a, 'h>(v: &'h RegionVec<&'static str>) -> &'h RegionVec<&'a str> {
///     v
/// }
/// ```
///
/// A shared view cannot outlive a mutation of the region, so growth can
/// never leave a dangling view:
///
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let mut v = r.vec_new::<u32>().unwrap();
/// let view = r.vec_as_slice(&v).unwrap();
/// r.vec_push(&mut v, 1).unwrap();
/// assert!(view.is_empty());
/// ```
///
/// Two mutable views cannot coexist, even through the same handle:
///
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let v = r.vec_new::<u32>().unwrap();
/// let a = r.vec_as_mut_slice(&v).unwrap();
/// let b = r.vec_as_mut_slice(&v).unwrap();
/// a.swap_with_slice(b);
/// ```
///
/// The handle is unique: it cannot be cloned into an alias.
///
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let v = r.vec_new::<u32>().unwrap();
/// let alias = v.clone();
/// ```
///
/// Elements must be `'static`, because the region may finalize them after any
/// borrow they hold has ended:
///
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let local = String::from("short-lived");
/// let mut v = r.vec_new::<&str>().unwrap();
/// r.vec_push(&mut v, local.as_str()).unwrap();
/// drop(local);
/// ```
pub struct RegionVec<T> {
    region: RegionId,
    slot: u32,
    serial: u64,
    _payload: PhantomData<fn(T) -> T>,
}

/// Copyable, read-only handle to a frozen element buffer
/// (`RegionSlice[r, T]`). Copies alias the same elements; no API yields a
/// mutable view through it. Releasing any copy, or a covering rewind or reset,
/// makes every copy stale. Invariant in `T` like [`RegionVec`]:
///
/// ```compile_fail
/// use vesper_alloc::RegionSlice;
/// fn shorten<'a>(s: RegionSlice<&'static str>) -> RegionSlice<&'a str> {
///     s
/// }
/// ```
pub struct RegionSlice<T> {
    region: RegionId,
    slot: u32,
    serial: u64,
    _payload: PhantomData<fn(T) -> T>,
}

impl<T> RegionVec<T> {
    pub fn region(&self) -> RegionId {
        self.region
    }
}

impl<T> RegionSlice<T> {
    pub fn region(&self) -> RegionId {
        self.region
    }
}

impl<T> Clone for RegionSlice<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for RegionSlice<T> {}

impl<T> PartialEq for RegionSlice<T> {
    fn eq(&self, other: &Self) -> bool {
        (self.region, self.slot, self.serial) == (other.region, other.slot, other.serial)
    }
}
impl<T> Eq for RegionSlice<T> {}

impl<T> fmt::Debug for RegionVec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegionVec")
            .field("region", &self.region)
            .field("slot", &self.slot)
            .field("serial", &self.serial)
            .finish()
    }
}

impl<T> fmt::Debug for RegionSlice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegionSlice")
            .field("region", &self.region)
            .field("slot", &self.slot)
            .field("serial", &self.serial)
            .finish()
    }
}

/// Drops `base[from..to]` highest index first. If a destructor panics, the
/// remaining lower elements are still dropped while unwinding (a second panic
/// aborts, as for `Vec`).
///
/// # Safety
///
/// `base[from..to]` must be initialized, aligned `T`s that nothing else will
/// read or drop afterwards.
unsafe fn drop_range<T>(base: *mut T, from: usize, to: usize) {
    struct Rest<T> {
        base: *mut T,
        from: usize,
        next: usize,
    }
    impl<T> Drop for Rest<T> {
        fn drop(&mut self) {
            while self.next > self.from {
                self.next -= 1;
                // SAFETY: by the caller contract of `drop_range`; `next`
                // only decreases, so each element is dropped once.
                unsafe { ptr::drop_in_place(self.base.add(self.next)) };
            }
        }
    }
    let mut rest = Rest {
        base,
        from,
        next: to,
    };
    while rest.next > rest.from {
        rest.next -= 1;
        // SAFETY: as above. `next` is decremented before the drop, so an
        // unwinding `Rest` continues with the element below this one.
        unsafe { ptr::drop_in_place(base.add(rest.next)) };
    }
}

/// Element finalizer registered for a buffer of `T`.
unsafe fn drop_elems<T>(base: *mut u8, len: usize) {
    // SAFETY: the region registers this only for a live buffer of `T` whose
    // first `len` elements are initialized, and removes the slot first.
    unsafe { drop_range(base.cast::<T>(), 0, len) }
}

/// Smallest nonzero capacity chosen by amortized growth (as for `Vec`).
fn min_growth_cap(elem_size: usize) -> usize {
    if elem_size == 1 {
        8
    } else if elem_size <= 1024 {
        4
    } else {
        1
    }
}

fn buffer_bytes(cap: usize, elem_size: usize) -> Result<usize, AllocError> {
    cap.checked_mul(elem_size)
        .filter(|b| *b <= isize::MAX as usize)
        .ok_or(AllocError::SizeOverflow)
}

impl<B: ChunkBacking> Region<B> {
    // ----- internal element-buffer machinery --------------------------------

    fn alloc_elements<T: 'static>(&mut self, cap: usize, linear: bool) -> Result<u32, AllocError> {
        let elem_size = mem::size_of::<T>();
        let align = mem::align_of::<T>();
        let (cap, bytes) = if elem_size == 0 {
            (usize::MAX, 0)
        } else {
            (cap, buffer_bytes(cap, elem_size)?)
        };
        self.reserve_slot()?;
        let p = self.place(bytes, align)?;
        let slot = self.push_slot(p, bytes, align, SlotKind::Elements);
        self.slots[slot as usize].elems = Some(Elems {
            len: 0,
            cap,
            elem_size,
            drop: mem::needs_drop::<T>().then_some(drop_elems::<T> as unsafe fn(*mut u8, usize)),
            linear,
        });
        Ok(slot)
    }

    fn elems(&self, index: usize) -> &Elems {
        self.slots[index]
            .elems
            .as_ref()
            .expect("element slots carry element metadata")
    }

    fn elems_mut(&mut self, index: usize) -> &mut Elems {
        self.slots[index]
            .elems
            .as_mut()
            .expect("element slots carry element metadata")
    }

    fn elem_base<T>(&self, index: usize) -> *mut T {
        let s = &self.slots[index];
        self.ptr_at(s.chunk, s.start).as_ptr().cast::<T>()
    }

    /// Keeps the slot's linear-obligation flag and the region counter in step
    /// with the element count. Called after every length change.
    fn sync_linear(&mut self, index: usize) {
        let s = &mut self.slots[index];
        let want =
            s.state == SlotState::Live && s.elems.as_ref().is_some_and(|e| e.linear && e.len > 0);
        if want != s.linear {
            s.linear = want;
            if want {
                self.live_linear += 1;
            } else {
                self.live_linear -= 1;
            }
        }
    }

    /// Ensures room for `additional` more elements. Returns the (possibly new)
    /// slot index. On error the buffer, its contents and its slot are
    /// unchanged.
    fn grow_elements(
        &mut self,
        index: usize,
        additional: usize,
        exact: bool,
    ) -> Result<usize, RegionError> {
        let (len, cap, elem_size) = {
            let e = self.elems(index);
            (e.len, e.cap, e.elem_size)
        };
        let required = len
            .checked_add(additional)
            .ok_or(AllocError::SizeOverflow)?;
        if required <= cap {
            return Ok(index);
        }
        // New capacity would lie above the newest watermark and be reclaimed
        // by its rewind while the buffer itself survives.
        if index < self.mark_floor() {
            return Err(RegionError::ProtectedByWatermark);
        }
        if exact {
            return self.resize_elements(index, required);
        }
        let target = required
            .max(cap.saturating_mul(2))
            .max(min_growth_cap(elem_size));
        match self.resize_elements(index, target) {
            Err(RegionError::Alloc(
                AllocError::SizeOverflow
                | AllocError::CapacityExceeded { .. }
                | AllocError::OutOfMemory { .. },
            )) if target > required => self.resize_elements(index, required),
            other => other,
        }
    }

    /// Gives the buffer `new_cap` elements of capacity, in place at the tip
    /// of the active chunk or by relocation. Relocation moves the initialized
    /// elements bitwise, transfers the element/finalizer/linear metadata to
    /// the new slot and only then releases the old one; no user code runs in
    /// between. Changes nothing on error.
    fn resize_elements(&mut self, index: usize, new_cap: usize) -> Result<usize, RegionError> {
        let (chunk, start, align) = {
            let s = &self.slots[index];
            (s.chunk, s.start, s.align)
        };
        let (len, elem_size) = {
            let e = self.elems(index);
            (e.len, e.elem_size)
        };
        let new_bytes = buffer_bytes(new_cap, elem_size)?;
        if self.is_tip(index)
            && let Some(end) = start.checked_add(new_bytes)
            && end <= self.chunks[chunk].capacity()
        {
            self.chunks[chunk].cursor = end;
            self.slots[index].len = new_bytes;
            self.elems_mut(index).cap = new_cap;
            return Ok(index);
        }
        self.reserve_slot()?;
        let p = self.place(new_bytes, align)?;
        // `place` only removes spare chunks after the active one, so `chunk`
        // still names the source buffer.
        let src = self.ptr_at(chunk, start);
        let dst = self.ptr_at(p.chunk, p.start);
        // SAFETY: `len * elem_size <= old capacity bytes`; the destination is
        // a fresh reservation disjoint from every live allocation.
        unsafe { ptr::copy_nonoverlapping(src.as_ptr(), dst.as_ptr(), len * elem_size) };
        let new = self.push_slot(p, new_bytes, align, SlotKind::Elements) as usize;
        let old = &mut self.slots[index];
        let mut elems = old.elems.take().expect("element slot");
        let linear = mem::replace(&mut old.linear, false);
        old.state = SlotState::Released;
        elems.cap = new_cap;
        let fresh = &mut self.slots[new];
        fresh.elems = Some(elems);
        fresh.linear = linear;
        Ok(new)
    }

    fn rebind<T>(&self, v: &mut RegionVec<T>, index: usize) {
        v.slot = index as u32;
        v.serial = self.slots[index].serial;
    }

    fn resolve_vec<T>(&self, v: &RegionVec<T>) -> Result<usize, HandleError> {
        self.resolve(v.region, v.slot, v.serial, SlotKind::Elements)
    }

    fn resolve_slice<T>(&self, s: &RegionSlice<T>) -> Result<usize, HandleError> {
        self.resolve(s.region, s.slot, s.serial, SlotKind::Frozen)
    }

    fn vec_handle<T>(&self, slot: u32) -> RegionVec<T> {
        RegionVec {
            region: self.id,
            slot,
            serial: self.slots[slot as usize].serial,
            _payload: PhantomData,
        }
    }

    fn elements<T>(&self, index: usize) -> &[T] {
        let len = self.elems(index).len;
        // SAFETY: a live element slot minted for this handle's `T` holds `len`
        // initialized, aligned elements; `&self` excludes mutable views.
        unsafe { std::slice::from_raw_parts(self.elem_base::<T>(index), len) }
    }

    /// Releases a live element buffer, dropping its elements highest index
    /// first. The slot is released and its metadata cleared before any
    /// destructor runs, so a panicking destructor cannot cause a double drop.
    fn release_elements<T>(&mut self, index: usize) {
        let base = self.elem_base::<T>(index);
        let len = mem::replace(&mut self.elems_mut(index).len, 0);
        self.sync_linear(index);
        self.slots[index].state = SlotState::Released;
        // SAFETY: the `len` initialized elements are no longer recorded.
        unsafe { drop_range(base, 0, len) };
        self.reclaim_tail();
    }

    // ----- construction -----------------------------------------------------

    /// Creates an empty vector for affine or copy elements. It owns a
    /// zero-byte buffer, so it consumes a slot but no element memory.
    pub fn vec_new<T: 'static>(&mut self) -> Result<RegionVec<T>, AllocError> {
        self.vec_with_capacity(0)
    }

    /// Creates an empty vector with room for `cap` elements. Zero-sized
    /// element types always have capacity `usize::MAX`.
    pub fn vec_with_capacity<T: 'static>(
        &mut self,
        cap: usize,
    ) -> Result<RegionVec<T>, AllocError> {
        let slot = self.alloc_elements::<T>(cap, false)?;
        Ok(self.vec_handle(slot))
    }

    /// Creates a vector of linear elements. While it is nonempty it is one
    /// live linear obligation: rewind over it and reset fail, `close` reports
    /// it, and `truncate`, `clear`, `release` and `freeze` refuse to discard
    /// elements. Elements leave only through `pop` or `drain`.
    pub fn vec_with_capacity_linear<T: 'static>(
        &mut self,
        cap: usize,
    ) -> Result<RegionVec<T>, AllocError> {
        let slot = self.alloc_elements::<T>(cap, true)?;
        Ok(self.vec_handle(slot))
    }

    // ----- inspection ---------------------------------------------------------

    pub fn vec_len<T>(&self, v: &RegionVec<T>) -> Result<usize, HandleError> {
        Ok(self.elems(self.resolve_vec(v)?).len)
    }

    pub fn vec_is_empty<T>(&self, v: &RegionVec<T>) -> Result<bool, HandleError> {
        Ok(self.vec_len(v)? == 0)
    }

    pub fn vec_capacity<T>(&self, v: &RegionVec<T>) -> Result<usize, HandleError> {
        Ok(self.elems(self.resolve_vec(v)?).cap)
    }

    /// Shared view of the initialized elements.
    pub fn vec_as_slice<T>(&self, v: &RegionVec<T>) -> Result<&[T], HandleError> {
        Ok(self.elements(self.resolve_vec(v)?))
    }

    /// Exclusive view of the initialized elements. `&mut self` makes this the
    /// only view of any region memory while it lives.
    pub fn vec_as_mut_slice<T>(&mut self, v: &RegionVec<T>) -> Result<&mut [T], HandleError> {
        let index = self.resolve_vec(v)?;
        let len = self.elems(index).len;
        // SAFETY: as in `elements`; `&mut self` makes the view exclusive.
        Ok(unsafe { std::slice::from_raw_parts_mut(self.elem_base::<T>(index), len) })
    }

    pub fn vec_get<T>(&self, v: &RegionVec<T>, i: usize) -> Result<Option<&T>, HandleError> {
        Ok(self.vec_as_slice(v)?.get(i))
    }

    pub fn vec_get_mut<T>(
        &mut self,
        v: &RegionVec<T>,
        i: usize,
    ) -> Result<Option<&mut T>, HandleError> {
        Ok(self.vec_as_mut_slice(v)?.get_mut(i))
    }

    // ----- growth and insertion ---------------------------------------------

    /// Ensures capacity for `additional` more elements with amortized
    /// (doubling) growth, falling back to the exact requirement when the
    /// amortized request does not fit the region's bounds. May relocate and
    /// update `v`. On error `v` and its elements are unchanged.
    pub fn vec_reserve<T>(
        &mut self,
        v: &mut RegionVec<T>,
        additional: usize,
    ) -> Result<(), RegionError> {
        let index = self.resolve_vec(v)?;
        let index = self.grow_elements(index, additional, false)?;
        self.rebind(v, index);
        Ok(())
    }

    /// As [`Region::vec_reserve`], without amortization.
    pub fn vec_reserve_exact<T>(
        &mut self,
        v: &mut RegionVec<T>,
        additional: usize,
    ) -> Result<(), RegionError> {
        let index = self.resolve_vec(v)?;
        let index = self.grow_elements(index, additional, true)?;
        self.rebind(v, index);
        Ok(())
    }

    /// Appends `value`. On failure the vector is unchanged and the value is
    /// returned in the error.
    pub fn vec_push<T>(&mut self, v: &mut RegionVec<T>, value: T) -> Result<(), InsertError<T>> {
        let grown = self
            .resolve_vec(v)
            .map_err(RegionError::from)
            .and_then(|index| self.grow_elements(index, 1, false));
        let index = match grown {
            Ok(index) => index,
            Err(error) => return Err(InsertError { value, error }),
        };
        self.rebind(v, index);
        let len = self.elems(index).len;
        // SAFETY: `grow_elements` ensured `len < cap`; the slot at `len` is
        // inside the buffer and uninitialized.
        unsafe { self.elem_base::<T>(index).add(len).write(value) };
        self.elems_mut(index).len = len + 1;
        self.sync_linear(index);
        Ok(())
    }

    /// Appends a copy of `data`, all or nothing.
    pub fn vec_extend_from_slice<T: Copy>(
        &mut self,
        v: &mut RegionVec<T>,
        data: &[T],
    ) -> Result<(), RegionError> {
        self.vec_reserve(v, data.len())?;
        let index = self.resolve_vec(v)?;
        let len = self.elems(index).len;
        // SAFETY: capacity for `data.len()` more elements was reserved; `data`
        // is a shared borrow and cannot alias region memory under `&mut self`.
        unsafe {
            ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.elem_base::<T>(index).add(len),
                data.len(),
            );
        }
        self.elems_mut(index).len = len + data.len();
        self.sync_linear(index);
        Ok(())
    }

    // ----- removal and extraction ---------------------------------------------

    /// Moves the last element out. Capacity is kept.
    pub fn vec_pop<T>(&mut self, v: &mut RegionVec<T>) -> Result<Option<T>, HandleError> {
        let index = self.resolve_vec(v)?;
        let Some(len) = self.elems(index).len.checked_sub(1) else {
            return Ok(None);
        };
        self.elems_mut(index).len = len;
        self.sync_linear(index);
        // SAFETY: element `len` was initialized and is no longer recorded, so
        // it is read exactly once.
        Ok(Some(unsafe { self.elem_base::<T>(index).add(len).read() }))
    }

    /// Drops the elements at `new_len..`, highest index first. A longer
    /// `new_len` is a no-op. Linear elements cannot be discarded this way.
    pub fn vec_truncate<T>(&mut self, v: &RegionVec<T>, new_len: usize) -> Result<(), RegionError> {
        let index = self.resolve_vec(v)?;
        let e = self.elems(index);
        let len = e.len;
        if new_len >= len {
            return Ok(());
        }
        if e.linear {
            return Err(RegionError::LinearObligationsLive {
                count: len - new_len,
            });
        }
        self.elems_mut(index).len = new_len;
        // SAFETY: `new_len..len` were initialized and are no longer recorded.
        unsafe { drop_range(self.elem_base::<T>(index), new_len, len) };
        Ok(())
    }

    pub fn vec_clear<T>(&mut self, v: &RegionVec<T>) -> Result<(), RegionError> {
        self.vec_truncate(v, 0)
    }

    /// Moves every element out to `sink`, front to back, leaving the vector
    /// empty with its capacity. This discharges linear elements. If `sink`
    /// panics, the elements not yet passed are dropped, highest index first.
    pub fn vec_drain<T>(
        &mut self,
        v: &mut RegionVec<T>,
        mut sink: impl FnMut(T),
    ) -> Result<(), HandleError> {
        struct Rest<T> {
            base: *mut T,
            next: usize,
            end: usize,
        }
        impl<T> Drop for Rest<T> {
            fn drop(&mut self) {
                // SAFETY: `next..end` are initialized and unrecorded; each
                // earlier element was moved out exactly once.
                unsafe { drop_range(self.base, self.next, self.end) }
            }
        }
        let index = self.resolve_vec(v)?;
        let base = self.elem_base::<T>(index);
        let len = mem::replace(&mut self.elems_mut(index).len, 0);
        self.sync_linear(index);
        let mut rest = Rest {
            base,
            next: 0,
            end: len,
        };
        while rest.next < rest.end {
            // SAFETY: element `next` is initialized and read exactly once;
            // `next` advances before `sink` can unwind.
            let value = unsafe { base.add(rest.next).read() };
            rest.next += 1;
            sink(value);
        }
        Ok(())
    }

    /// Drops the elements (highest index first) and releases the buffer;
    /// every alias of the handle becomes stale. The bytes are reused at once
    /// when the buffer is the unprotected tip. A nonempty linear vector is
    /// returned unchanged with `LinearObligationsLive`.
    pub fn vec_release<T>(&mut self, v: RegionVec<T>) -> Result<(), OwnerError<RegionVec<T>>> {
        let index = match self.resolve_vec(&v) {
            Ok(index) => index,
            Err(e) => {
                return Err(OwnerError {
                    owner: v,
                    error: e.into(),
                });
            }
        };
        if self.slots[index].linear {
            let count = self.elems(index).len;
            return Err(OwnerError {
                owner: v,
                error: RegionError::LinearObligationsLive { count },
            });
        }
        self.release_elements::<T>(index);
        Ok(())
    }

    /// Freezes the vector into a copyable read-only slice handle naming the
    /// same buffer. A nonempty linear vector is returned unchanged.
    pub fn vec_freeze<T>(
        &mut self,
        v: RegionVec<T>,
    ) -> Result<RegionSlice<T>, OwnerError<RegionVec<T>>> {
        let index = match self.resolve_vec(&v) {
            Ok(index) => index,
            Err(e) => {
                return Err(OwnerError {
                    owner: v,
                    error: e.into(),
                });
            }
        };
        if self.slots[index].linear {
            let count = self.elems(index).len;
            return Err(OwnerError {
                owner: v,
                error: RegionError::LinearObligationsLive { count },
            });
        }
        self.elems_mut(index).linear = false;
        self.slots[index].kind = SlotKind::Frozen;
        Ok(RegionSlice {
            region: v.region,
            slot: v.slot,
            serial: v.serial,
            _payload: PhantomData,
        })
    }

    // ----- frozen slices --------------------------------------------------------

    pub fn slice_len<T>(&self, s: &RegionSlice<T>) -> Result<usize, HandleError> {
        Ok(self.elems(self.resolve_slice(s)?).len)
    }

    /// Shared view of a frozen buffer. There is no mutable counterpart.
    pub fn slice<T>(&self, s: &RegionSlice<T>) -> Result<&[T], HandleError> {
        Ok(self.elements(self.resolve_slice(s)?))
    }

    /// Drops the elements and releases the buffer; every copy becomes stale.
    pub fn slice_release<T>(&mut self, s: RegionSlice<T>) -> Result<(), HandleError> {
        let index = self.resolve_slice(&s)?;
        self.release_elements::<T>(index);
        Ok(())
    }
}
