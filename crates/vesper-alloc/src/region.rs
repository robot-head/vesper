//! Bounded, chunked bump regions with checked handles.
//!
//! Physical layout: a region owns a list of chunks obtained from its
//! [`ChunkBacking`]. Chunks never move, so pointers into a chunk stay valid
//! until the allocation is rewound, reset, or the region is dropped. Chunks
//! `0..=current` are in use; later chunks are spares retained after a rewind.
//!
//! Logical layout: every tracked allocation has a slot in an allocation-ordered
//! table. A handle names `(region id, slot index, serial)`. Serials are never
//! reused by a region, so a handle to rewound or reset memory is rejected even
//! after its slot index and its bytes are reused by a newer allocation.
use crate::backing::{ChunkBacking, SystemBacking};
use crate::error::{
    AllocError, ConfigError, HandleError, RegionError, RewindError, ValueAllocError,
};
use std::alloc::Layout;
use std::fmt;
use std::marker::PhantomData;
use std::mem;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU64, Ordering};

/// Minimum alignment of every chunk base.
pub const CHUNK_ALIGN: usize = 16;

static NEXT_REGION_ID: AtomicU64 = AtomicU64::new(1);

/// Process-unique identity of a region. Identities are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegionId(u64);

impl RegionId {
    pub fn bits(self) -> u64 {
        self.0
    }
}

/// Bounds for one region. Every limit is enforced; exceeding one is an error,
/// never an unbounded fallback to more memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionConfig {
    /// Capacity of the first chunk. Later chunks double up to `max_chunk_bytes`.
    pub initial_chunk_bytes: usize,
    /// Growth cap for ordinary chunks. A single larger request receives a
    /// dedicated chunk if the reservation permits it.
    pub max_chunk_bytes: usize,
    /// Upper bound on the sum of all chunk capacities held by the region.
    pub max_reserved_bytes: usize,
    /// Largest accepted allocation alignment (a power of two).
    pub max_align: usize,
    /// Upper bound on tracked allocations (live, moved, or released slots that
    /// have not yet been reclaimed by a tail release, rewind, or reset).
    pub max_allocations: usize,
    /// Upper bound on outstanding watermarks.
    pub max_checkpoints: usize,
    /// Chunk capacity retained across `reset` for reuse; the rest is returned
    /// to the backing.
    pub retained_bytes: usize,
}

impl Default for RegionConfig {
    fn default() -> Self {
        Self {
            initial_chunk_bytes: 4096,
            max_chunk_bytes: 1 << 20,
            max_reserved_bytes: 64 << 20,
            max_align: 4096,
            max_allocations: 1 << 20,
            max_checkpoints: 256,
            retained_bytes: 4096,
        }
    }
}

impl RegionConfig {
    /// Default growth policy with a total reservation bound.
    pub fn bounded(max_reserved_bytes: usize) -> Self {
        let initial = max_reserved_bytes.clamp(1, 4096);
        Self {
            initial_chunk_bytes: initial,
            max_reserved_bytes,
            retained_bytes: initial,
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.initial_chunk_bytes == 0 {
            return Err(ConfigError::ZeroInitialChunk);
        }
        if self.initial_chunk_bytes > self.max_chunk_bytes {
            return Err(ConfigError::InitialExceedsMaxChunk);
        }
        if self.initial_chunk_bytes > self.max_reserved_bytes {
            return Err(ConfigError::InitialExceedsReservation);
        }
        if !self.max_align.is_power_of_two() {
            return Err(ConfigError::MaxAlignNotPowerOfTwo);
        }
        if Layout::from_size_align(self.max_chunk_bytes, CHUNK_ALIGN.max(self.max_align)).is_err() {
            return Err(ConfigError::ChunkLayout);
        }
        if self.max_allocations > u32::MAX as usize {
            return Err(ConfigError::AllocationLimitTooLarge);
        }
        Ok(())
    }
}

/// Point-in-time accounting for a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionStats {
    pub reserved_bytes: usize,
    pub used_bytes: usize,
    pub chunks: usize,
    pub tracked_allocations: usize,
    pub live_linear: usize,
    pub checkpoints: usize,
    /// Incremented by every successful rewind and reset.
    pub epoch: u64,
}

/// Checked handle to a zero-initialized byte allocation (`RegionSlice[r, u8]`).
/// Bytes are a copy payload, so the handle is `Copy`; when the allocation is
/// released, relocated, rewound, or reset, every copy becomes stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegionBytes {
    region: RegionId,
    slot: u32,
    serial: u64,
}

impl RegionBytes {
    pub fn region(&self) -> RegionId {
        self.region
    }
}

/// Owning handle to a `T` stored in region memory (`RegionBox[r, T]`).
/// It is deliberately neither `Clone` nor `Copy`: the payload may be affine or
/// linear. Dropping a `RegionBox` does not drop the payload; the payload is
/// finalized by `take`, rewind, reset, or region drop.
pub struct RegionBox<T> {
    region: RegionId,
    slot: u32,
    serial: u64,
    /// Invariant in `T`: `get_mut` writes through a shared handle reference,
    /// so a covariant handle could store a shorter-lived value under a longer
    /// type (for example a local borrow as `&'static str`).
    _payload: PhantomData<fn(T) -> T>,
}

impl<T> RegionBox<T> {
    pub fn region(&self) -> RegionId {
        self.region
    }
}

impl<T> fmt::Debug for RegionBox<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegionBox")
            .field("region", &self.region)
            .field("slot", &self.slot)
            .field("serial", &self.serial)
            .finish()
    }
}

/// Linear checkpoint token (`Watermark`). It must be passed by value to
/// [`Region::rewind`] or [`Region::commit`]. Rewinding to an older watermark
/// invalidates every newer one.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a watermark must be rewound or committed"]
pub struct Watermark {
    region: RegionId,
    serial: u64,
}

impl Watermark {
    pub fn region(&self) -> RegionId {
        self.region
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Bytes,
    Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Live,
    Moved,
    Released,
}

struct Slot {
    serial: u64,
    chunk: usize,
    /// Cursor of `chunk` before this allocation (includes alignment padding).
    prior_cursor: usize,
    start: usize,
    len: usize,
    align: usize,
    kind: SlotKind,
    state: SlotState,
    finalizer: Option<unsafe fn(*mut u8)>,
    linear: bool,
}

struct Chunk {
    base: NonNull<u8>,
    layout: Layout,
    cursor: usize,
}

impl Chunk {
    fn capacity(&self) -> usize {
        self.layout.size()
    }

    /// Offset of an aligned `size`-byte block at the cursor, if it fits.
    fn fit(&self, size: usize, align: usize) -> Option<usize> {
        let base = self.base.as_ptr().addr();
        let at = base.checked_add(self.cursor)?;
        let aligned = at.checked_add(align - 1)? & !(align - 1);
        let start = aligned - base;
        let end = start.checked_add(size)?;
        (end <= self.capacity()).then_some(start)
    }
}

struct MarkRecord {
    serial: u64,
    slot_len: usize,
    chunk: usize,
    cursor: usize,
}

#[derive(Clone, Copy)]
struct Placement {
    chunk: usize,
    prior_cursor: usize,
    start: usize,
}

/// A bounded region. It is uniquely owned, has no `Clone` implementation, and
/// is not `Send`/`Sync`: a region is confined to the shard that created it.
pub struct Region<B: ChunkBacking = SystemBacking> {
    id: RegionId,
    config: RegionConfig,
    backing: B,
    chunks: Vec<Chunk>,
    current: usize,
    reserved: usize,
    slots: Vec<Slot>,
    next_serial: u64,
    marks: Vec<MarkRecord>,
    live_linear: usize,
    epoch: u64,
}

impl Region<SystemBacking> {
    pub fn new(config: RegionConfig) -> Result<Self, ConfigError> {
        Self::with_backing(config, SystemBacking)
    }

    /// Runs `body` with a fresh region and closes it on return (`Region::scoped`).
    /// The region is also reclaimed if `body` unwinds. Returns
    /// `LinearObligationsLive` if linear payloads were abandoned; they are still
    /// finalized so that their resources are not leaked.
    pub fn scoped<R>(
        config: RegionConfig,
        body: impl FnOnce(&mut Region) -> R,
    ) -> Result<R, RegionError> {
        Self::scoped_in(config, SystemBacking, body)
    }
}

unsafe fn drop_value<T>(ptr: *mut u8) {
    // SAFETY: the region registers this finalizer only for a live, initialized,
    // suitably aligned `T` at `ptr`, and clears it when the value is moved out.
    unsafe { ptr::drop_in_place(ptr.cast::<T>()) }
}

impl<B: ChunkBacking> Region<B> {
    pub fn with_backing(config: RegionConfig, backing: B) -> Result<Self, ConfigError> {
        config.validate()?;
        let id = NEXT_REGION_ID.fetch_add(1, Ordering::Relaxed);
        assert!(id != u64::MAX, "region identities exhausted");
        Ok(Self {
            id: RegionId(id),
            config,
            backing,
            chunks: Vec::new(),
            current: 0,
            reserved: 0,
            slots: Vec::new(),
            next_serial: 0,
            marks: Vec::new(),
            live_linear: 0,
            epoch: 0,
        })
    }

    /// [`Region::scoped`] with an explicit backing.
    pub fn scoped_in<R>(
        config: RegionConfig,
        backing: B,
        body: impl FnOnce(&mut Region<B>) -> R,
    ) -> Result<R, RegionError> {
        let mut region = Self::with_backing(config, backing)?;
        let out = body(&mut region);
        region.close()?;
        Ok(out)
    }

    pub fn id(&self) -> RegionId {
        self.id
    }

    pub fn config(&self) -> &RegionConfig {
        &self.config
    }

    pub fn backing(&self) -> &B {
        &self.backing
    }

    pub fn stats(&self) -> RegionStats {
        RegionStats {
            reserved_bytes: self.reserved,
            used_bytes: self.chunks.iter().map(|c| c.cursor).sum(),
            chunks: self.chunks.len(),
            tracked_allocations: self.slots.len(),
            live_linear: self.live_linear,
            checkpoints: self.marks.len(),
            epoch: self.epoch,
        }
    }

    // ----- placement ------------------------------------------------------

    fn check_align(&self, align: usize) -> Result<(), AllocError> {
        if !align.is_power_of_two() {
            return Err(AllocError::InvalidAlignment { align });
        }
        if align > self.config.max_align {
            return Err(AllocError::AlignmentTooLarge {
                align,
                max: self.config.max_align,
            });
        }
        Ok(())
    }

    /// Reserves `size` bytes at `align`. On error nothing changes.
    fn place(&mut self, size: usize, align: usize) -> Result<Placement, AllocError> {
        self.check_align(align)?;
        if size > isize::MAX as usize {
            return Err(AllocError::SizeOverflow);
        }
        if let Some(chunk) = self.chunks.get(self.current)
            && let Some(start) = chunk.fit(size, align)
        {
            return Ok(self.commit_placement(self.current, start, size));
        }
        // Reuse spare chunks retained by an earlier rewind. Spares before the
        // first one that fits are returned so the chunk list stays
        // allocation-ordered; that is only done once the request is certain to
        // succeed, so a failed request leaves every spare in place.
        let first_spare = if self.chunks.is_empty() {
            0
        } else {
            self.current + 1
        };
        let fitting = (first_spare..self.chunks.len())
            .find_map(|i| self.chunks[i].fit(size, align).map(|start| (i, start)));
        if let Some((index, start)) = fitting {
            for spare in self.chunks.drain(first_spare..index).collect::<Vec<_>>() {
                self.release_chunk(spare);
            }
            self.current = first_spare;
            return Ok(self.commit_placement(first_spare, start, size));
        }
        // No spare fits: a new chunk replaces them all, so their capacity
        // counts as available to it. They are released only after the backing
        // has supplied the replacement.
        let spare_bytes = self.chunks[first_spare..].iter().map(Chunk::capacity).sum();
        let chunk = self.new_chunk(size, align, first_spare, spare_bytes)?;
        let start = chunk.fit(size, align).ok_or(AllocError::SizeOverflow)?;
        for spare in self.chunks.split_off(first_spare) {
            self.release_chunk(spare);
        }
        self.chunks.push(chunk);
        self.current = self.chunks.len() - 1;
        Ok(self.commit_placement(self.current, start, size))
    }

    fn commit_placement(&mut self, chunk: usize, start: usize, size: usize) -> Placement {
        let c = &mut self.chunks[chunk];
        let prior_cursor = c.cursor;
        c.cursor = start + size;
        Placement {
            chunk,
            prior_cursor,
            start,
        }
    }

    /// Obtains a chunk to follow `chunks[..in_use]`, treating `releasable`
    /// bytes of spare capacity as available. Changes nothing on error.
    fn new_chunk(
        &mut self,
        size: usize,
        align: usize,
        in_use: usize,
        releasable: usize,
    ) -> Result<Chunk, AllocError> {
        let growth = match self.chunks[..in_use].last() {
            None => self.config.initial_chunk_bytes,
            Some(last) => last
                .capacity()
                .saturating_mul(2)
                .min(self.config.max_chunk_bytes),
        };
        let reserved = self.reserved - releasable;
        let remaining = self.config.max_reserved_bytes - reserved;
        let exceeded = AllocError::CapacityExceeded {
            requested: size,
            reserved,
            limit: self.config.max_reserved_bytes,
        };
        if size > remaining {
            return Err(exceeded);
        }
        let capacity = growth.min(remaining).max(size);
        if capacity == 0 {
            return Err(exceeded);
        }
        let layout = Layout::from_size_align(capacity, CHUNK_ALIGN.max(align))
            .map_err(|_| AllocError::SizeOverflow)?;
        let base = self
            .backing
            .allocate(layout)
            .ok_or(AllocError::OutOfMemory {
                chunk_bytes: capacity,
            })?;
        self.reserved += capacity;
        Ok(Chunk {
            base,
            layout,
            cursor: 0,
        })
    }

    fn release_chunk(&mut self, chunk: Chunk) {
        self.reserved -= chunk.capacity();
        // SAFETY: each chunk came from this backing with this layout and is
        // removed from `self.chunks` before being released, so it is released
        // once. No live slot refers to a released chunk.
        unsafe { self.backing.deallocate(chunk.base, chunk.layout) }
    }

    fn next_serial(&mut self) -> Result<u64, AllocError> {
        let serial = self.next_serial;
        self.next_serial = serial.checked_add(1).ok_or(AllocError::ExhaustedSerials)?;
        Ok(serial)
    }

    fn reserve_slot(&self) -> Result<(), AllocError> {
        if self.slots.len() >= self.config.max_allocations {
            return Err(AllocError::TooManyAllocations {
                limit: self.config.max_allocations,
            });
        }
        if self.next_serial == u64::MAX {
            return Err(AllocError::ExhaustedSerials);
        }
        Ok(())
    }

    fn ptr_at(&self, chunk: usize, start: usize) -> NonNull<u8> {
        let c = &self.chunks[chunk];
        debug_assert!(start <= c.capacity());
        // SAFETY: `start <= capacity`, so the offset stays within (or one past)
        // the chunk allocation.
        unsafe { c.base.add(start) }
    }

    fn push_slot(&mut self, p: Placement, len: usize, align: usize, kind: SlotKind) -> u32 {
        let serial = self.next_serial().expect("checked by reserve_slot");
        self.slots.push(Slot {
            serial,
            chunk: p.chunk,
            prior_cursor: p.prior_cursor,
            start: p.start,
            len,
            align,
            kind,
            state: SlotState::Live,
            finalizer: None,
            linear: false,
        });
        (self.slots.len() - 1) as u32
    }

    fn mark_floor(&self) -> usize {
        self.marks.last().map_or(0, |m| m.slot_len)
    }

    /// Whether `slot` is the newest allocation, is not protected by a
    /// watermark, and ends exactly at the bump cursor of the active chunk.
    fn is_tip(&self, slot: usize) -> bool {
        let s = &self.slots[slot];
        slot + 1 == self.slots.len()
            && slot >= self.mark_floor()
            && s.chunk == self.current
            && self.chunks[s.chunk].cursor == s.start + s.len
    }

    /// Pops released or moved allocations at the tip so their bytes are reused.
    /// When the active chunk empties, the previous chunk becomes active again
    /// (the emptied one stays as a spare), so the cascade continues across
    /// chunk boundaries. It never steps below the newest watermark's chunk or
    /// past a chunk that still holds a slot (including a zero-sized one); a
    /// nonzero raw allocation keeps its chunk's cursor nonzero and so also
    /// stops it.
    fn reclaim_tail(&mut self) {
        loop {
            let pinned = self.marks.last().map_or(0, |m| m.chunk);
            // A zero cursor alone does not mean the chunk is empty: a
            // zero-sized allocation can open a chunk without advancing it.
            // Slots are allocation-ordered, so the chunk holds a slot exactly
            // when the newest slot lives in it.
            while self.current > pinned
                && self.chunks[self.current].cursor == 0
                && self.slots.last().is_none_or(|s| s.chunk < self.current)
            {
                self.current -= 1;
            }
            let Some(last) = self.slots.len().checked_sub(1) else {
                break;
            };
            if self.slots[last].state == SlotState::Live || !self.is_tip(last) {
                break;
            }
            let s = self.slots.pop().expect("nonempty");
            self.chunks[s.chunk].cursor = s.prior_cursor;
        }
    }

    // ----- handle validation ----------------------------------------------

    fn resolve(
        &self,
        region: RegionId,
        slot: u32,
        serial: u64,
        kind: SlotKind,
    ) -> Result<usize, HandleError> {
        if region != self.id {
            return Err(HandleError::CrossRegion {
                expected: self.id,
                found: region,
            });
        }
        let index = slot as usize;
        let s = self
            .slots
            .get(index)
            .filter(|s| s.serial == serial)
            .ok_or(HandleError::Stale)?;
        debug_assert_eq!(s.kind, kind, "serials are unique per region");
        match s.state {
            SlotState::Live => Ok(index),
            SlotState::Moved => Err(HandleError::Moved),
            SlotState::Released => Err(HandleError::Stale),
        }
    }

    fn resolve_bytes(&self, h: &RegionBytes) -> Result<usize, HandleError> {
        self.resolve(h.region, h.slot, h.serial, SlotKind::Bytes)
    }

    fn resolve_box<T>(&self, b: &RegionBox<T>) -> Result<usize, HandleError> {
        self.resolve(b.region, b.slot, b.serial, SlotKind::Value)
    }

    // ----- bytes ------------------------------------------------------------

    /// Allocates `len` zero-initialized bytes aligned to `align`.
    pub fn alloc_bytes(&mut self, len: usize, align: usize) -> Result<RegionBytes, AllocError> {
        self.reserve_slot()?;
        let p = self.place(len, align)?;
        let ptr = self.ptr_at(p.chunk, p.start);
        // SAFETY: `place` reserved `len` bytes at `ptr` inside a chunk that no
        // other live allocation overlaps.
        unsafe { ptr.as_ptr().write_bytes(0, len) };
        let slot = self.push_slot(p, len, align, SlotKind::Bytes);
        Ok(self.bytes_handle(slot))
    }

    /// Allocates a copy of `data` aligned to `align`.
    pub fn alloc_bytes_copy(
        &mut self,
        data: &[u8],
        align: usize,
    ) -> Result<RegionBytes, AllocError> {
        self.reserve_slot()?;
        let p = self.place(data.len(), align)?;
        let ptr = self.ptr_at(p.chunk, p.start);
        // SAFETY: fresh, exclusively reserved destination of `data.len()` bytes;
        // `data` is a shared borrow that cannot alias region memory mutably.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), ptr.as_ptr(), data.len()) };
        let slot = self.push_slot(p, data.len(), align, SlotKind::Bytes);
        Ok(self.bytes_handle(slot))
    }

    fn bytes_handle(&self, slot: u32) -> RegionBytes {
        RegionBytes {
            region: self.id,
            slot,
            serial: self.slots[slot as usize].serial,
        }
    }

    pub fn len(&self, h: &RegionBytes) -> Result<usize, HandleError> {
        Ok(self.slots[self.resolve_bytes(h)?].len)
    }

    pub fn bytes(&self, h: &RegionBytes) -> Result<&[u8], HandleError> {
        let s = &self.slots[self.resolve_bytes(h)?];
        let ptr = self.ptr_at(s.chunk, s.start);
        // SAFETY: live slot, initialized at allocation, bounded by its chunk;
        // `&self` excludes concurrent mutable views.
        Ok(unsafe { std::slice::from_raw_parts(ptr.as_ptr(), s.len) })
    }

    pub fn bytes_mut(&mut self, h: &RegionBytes) -> Result<&mut [u8], HandleError> {
        let s = &self.slots[self.resolve_bytes(h)?];
        let ptr = self.ptr_at(s.chunk, s.start);
        // SAFETY: as in `bytes`; `&mut self` makes this the only view.
        Ok(unsafe { std::slice::from_raw_parts_mut(ptr.as_ptr(), s.len) })
    }

    /// Two simultaneous mutable views of distinct allocations. Live
    /// allocations never overlap, so distinct slots yield disjoint slices.
    pub fn bytes_mut_pair(
        &mut self,
        a: &RegionBytes,
        b: &RegionBytes,
    ) -> Result<(&mut [u8], &mut [u8]), HandleError> {
        let ia = self.resolve_bytes(a)?;
        let ib = self.resolve_bytes(b)?;
        if ia == ib {
            return Err(HandleError::Aliased);
        }
        let (sa, sb) = (&self.slots[ia], &self.slots[ib]);
        let pa = self.ptr_at(sa.chunk, sa.start);
        let pb = self.ptr_at(sb.chunk, sb.start);
        // SAFETY: two distinct live slots occupy disjoint byte ranges.
        Ok(unsafe {
            (
                std::slice::from_raw_parts_mut(pa.as_ptr(), sa.len),
                std::slice::from_raw_parts_mut(pb.as_ptr(), sb.len),
            )
        })
    }

    /// Appends `data`. Grows in place when `h` is the unprotected tip of the
    /// active chunk; otherwise relocates to a new allocation, releases the old
    /// one and updates `h` (other copies of the old handle become stale). On
    /// error, `h` and its contents are unchanged.
    ///
    /// An allocation made before the newest outstanding watermark cannot grow:
    /// its new bytes would lie above the watermark and be discarded by the
    /// rewind while the allocation itself survives. That case returns
    /// `ProtectedByWatermark` instead of silently losing data.
    pub fn extend_bytes(&mut self, h: &mut RegionBytes, data: &[u8]) -> Result<(), RegionError> {
        let index = self.resolve_bytes(h)?;
        if data.is_empty() {
            return Ok(());
        }
        if index < self.mark_floor() {
            return Err(RegionError::ProtectedByWatermark);
        }
        let (old_len, align, chunk, start) = {
            let s = &self.slots[index];
            (s.len, s.align, s.chunk, s.start)
        };
        let new_len = old_len
            .checked_add(data.len())
            .filter(|n| *n <= isize::MAX as usize)
            .ok_or(AllocError::SizeOverflow)?;
        if self.is_tip(index) && start + new_len <= self.chunks[chunk].capacity() {
            let dst = self.ptr_at(chunk, start + old_len);
            // SAFETY: the bytes after the tip up to capacity belong to no live
            // allocation; `data` cannot alias them mutably.
            unsafe { ptr::copy_nonoverlapping(data.as_ptr(), dst.as_ptr(), data.len()) };
            self.chunks[chunk].cursor = start + new_len;
            self.slots[index].len = new_len;
            return Ok(());
        }
        self.reserve_slot()?;
        let p = self.place(new_len, align)?;
        let src = self.ptr_at(chunk, start);
        let dst = self.ptr_at(p.chunk, p.start);
        // SAFETY: the destination is a fresh reservation disjoint from the
        // live source slot and from `data`.
        unsafe {
            ptr::copy_nonoverlapping(src.as_ptr(), dst.as_ptr(), old_len);
            ptr::copy_nonoverlapping(data.as_ptr(), dst.as_ptr().add(old_len), data.len());
        }
        let slot = self.push_slot(p, new_len, align, SlotKind::Bytes);
        self.slots[index].state = SlotState::Released;
        *h = self.bytes_handle(slot);
        Ok(())
    }

    /// Releases a byte allocation. Every copy of the handle becomes stale. The
    /// bytes are reused immediately if the allocation is the unprotected tip,
    /// and otherwise at the next covering rewind or reset.
    pub fn release_bytes(&mut self, h: RegionBytes) -> Result<(), HandleError> {
        let index = self.resolve_bytes(&h)?;
        self.slots[index].state = SlotState::Released;
        self.reclaim_tail();
        Ok(())
    }

    /// Stable address of a live byte allocation for lowered native code.
    /// The pointer is valid for `len` bytes until the allocation is released,
    /// rewound, or reset, or the region is dropped.
    pub fn bytes_ptr(&self, h: &RegionBytes) -> Result<NonNull<u8>, HandleError> {
        let s = &self.slots[self.resolve_bytes(h)?];
        Ok(self.ptr_at(s.chunk, s.start))
    }

    /// Untracked raw bump allocation (the native fast path's slow-path entry).
    /// The memory is uninitialized and has no handle: it is reclaimed only by a
    /// covering rewind, reset, or drop, and no finalizer runs for it.
    /// Dereferencing the pointer is the caller's `unsafe` obligation.
    pub fn alloc_raw(&mut self, size: usize, align: usize) -> Result<NonNull<u8>, AllocError> {
        let p = self.place(size, align)?;
        Ok(self.ptr_at(p.chunk, p.start))
    }

    // ----- typed values -----------------------------------------------------

    fn alloc_typed<T: 'static>(
        &mut self,
        value: T,
        linear: bool,
    ) -> Result<RegionBox<T>, ValueAllocError<T>> {
        let placed = self
            .reserve_slot()
            .and_then(|()| self.place(mem::size_of::<T>(), mem::align_of::<T>()));
        let p = match placed {
            Ok(p) => p,
            Err(error) => return Err(ValueAllocError { value, error }),
        };
        let ptr = self.ptr_at(p.chunk, p.start).cast::<T>();
        // SAFETY: `place` reserved `size_of::<T>()` bytes aligned for `T`.
        unsafe { ptr.as_ptr().write(value) };
        let slot = self.push_slot(
            p,
            mem::size_of::<T>(),
            mem::align_of::<T>(),
            SlotKind::Value,
        );
        let s = &mut self.slots[slot as usize];
        if mem::needs_drop::<T>() {
            s.finalizer = Some(drop_value::<T>);
        }
        s.linear = linear;
        let serial = s.serial;
        if linear {
            self.live_linear += 1;
        }
        Ok(RegionBox {
            region: self.id,
            slot,
            serial,
            _payload: PhantomData,
        })
    }

    /// Moves an affine or copy value into the region. If `T` needs drop, it is
    /// registered on the region's LIFO finalizer list.
    pub fn alloc_value<T: 'static>(
        &mut self,
        value: T,
    ) -> Result<RegionBox<T>, ValueAllocError<T>> {
        self.alloc_typed(value, false)
    }

    /// Moves a linear value into the region. While it is live, `rewind` over
    /// it and `reset` fail, and `close` reports an abandoned obligation.
    pub fn alloc_linear<T: 'static>(
        &mut self,
        value: T,
    ) -> Result<RegionBox<T>, ValueAllocError<T>> {
        self.alloc_typed(value, true)
    }

    pub fn get<T>(&self, b: &RegionBox<T>) -> Result<&T, HandleError> {
        let s = &self.slots[self.resolve_box(b)?];
        let ptr = self.ptr_at(s.chunk, s.start).cast::<T>();
        // SAFETY: a live value slot minted for this handle's `T` (serials are
        // unique) holds an initialized, aligned `T`.
        Ok(unsafe { ptr.as_ref() })
    }

    pub fn get_mut<T>(&mut self, b: &RegionBox<T>) -> Result<&mut T, HandleError> {
        let s = &self.slots[self.resolve_box(b)?];
        let mut ptr = self.ptr_at(s.chunk, s.start).cast::<T>();
        // SAFETY: as in `get`; `&mut self` makes this the only view.
        Ok(unsafe { ptr.as_mut() })
    }

    /// Moves the value out, unregistering its finalizer and discharging a
    /// linear obligation. The bytes are reused immediately at the tip.
    pub fn take<T>(&mut self, b: RegionBox<T>) -> Result<T, HandleError> {
        let index = self.resolve_box(&b)?;
        let s = &mut self.slots[index];
        s.state = SlotState::Moved;
        s.finalizer = None;
        if mem::replace(&mut s.linear, false) {
            self.live_linear -= 1;
        }
        let (chunk, start) = (s.chunk, s.start);
        let ptr = self.ptr_at(chunk, start).cast::<T>();
        // SAFETY: the slot was live and is now marked moved, so the value is
        // read exactly once and never finalized by the region.
        let value = unsafe { ptr.as_ptr().read() };
        self.reclaim_tail();
        Ok(value)
    }

    // ----- checkpoints, rewind, reset --------------------------------------

    /// Records the current allocation point.
    pub fn checkpoint(&mut self) -> Result<Watermark, RegionError> {
        if self.marks.len() >= self.config.max_checkpoints {
            return Err(RegionError::TooManyCheckpoints {
                limit: self.config.max_checkpoints,
            });
        }
        let serial = self.next_serial()?;
        self.marks.push(MarkRecord {
            serial,
            slot_len: self.slots.len(),
            chunk: self.current,
            cursor: self.chunks.get(self.current).map_or(0, |c| c.cursor),
        });
        Ok(Watermark {
            region: self.id,
            serial,
        })
    }

    fn find_mark(&self, mark: &Watermark) -> Result<usize, HandleError> {
        if mark.region != self.id {
            return Err(HandleError::CrossRegion {
                expected: self.id,
                found: mark.region,
            });
        }
        self.marks
            .iter()
            .rposition(|m| m.serial == mark.serial)
            .ok_or(HandleError::Stale)
    }

    /// Finalizes and discards every allocation made after `mark`, invalidates
    /// newer watermarks, and makes the bytes reusable. Chunks obtained after
    /// the watermark are kept as spares. Fails without changes if a linear
    /// payload allocated after `mark` is still live.
    pub fn rewind(&mut self, mark: Watermark) -> Result<(), RewindError> {
        let pos = self.find_mark(&mark).map_err(RewindError::Handle)?;
        let floor = self.marks[pos].slot_len;
        let count = self.slots[floor..]
            .iter()
            .filter(|s| s.state == SlotState::Live && s.linear)
            .count();
        if count > 0 {
            return Err(RewindError::LinearObligationsLive { mark, count });
        }
        self.marks.truncate(pos + 1);
        let record = self.marks.pop().expect("mark found at pos");
        self.finalize_from(floor);
        for c in &mut self.chunks[record.chunk..] {
            c.cursor = 0;
        }
        if let Some(c) = self.chunks.get_mut(record.chunk) {
            c.cursor = record.cursor;
        }
        self.current = record.chunk;
        self.epoch += 1;
        Ok(())
    }

    /// Discards a watermark without rewinding.
    pub fn commit(&mut self, mark: Watermark) -> Result<(), HandleError> {
        let pos = self.find_mark(&mark)?;
        self.marks.remove(pos);
        self.reclaim_tail();
        Ok(())
    }

    /// Pops slots down to `floor` in LIFO order, running finalizers. Each slot
    /// is removed before its finalizer runs, so a panicking finalizer cannot
    /// cause a double drop; the remaining slots are finalized by `Drop`.
    fn finalize_from(&mut self, floor: usize) {
        while self.slots.len() > floor {
            let s = self.slots.pop().expect("len > floor");
            if s.state == SlotState::Live {
                if s.linear {
                    self.live_linear -= 1;
                }
                if let Some(finalize) = s.finalizer {
                    let ptr = self.ptr_at(s.chunk, s.start);
                    // SAFETY: the slot was live and is now removed, so this is
                    // the only finalization of the initialized value.
                    unsafe { finalize(ptr.as_ptr()) };
                }
            }
        }
    }

    /// Finalizes every allocation, invalidates every handle and watermark, and
    /// returns chunk capacity beyond `retained_bytes` to the backing. Fails
    /// without changes while linear payloads are live.
    pub fn reset(&mut self) -> Result<(), RegionError> {
        if self.live_linear > 0 {
            return Err(RegionError::LinearObligationsLive {
                count: self.live_linear,
            });
        }
        self.reset_unchecked();
        Ok(())
    }

    fn reset_unchecked(&mut self) {
        self.marks.clear();
        self.finalize_from(0);
        let mut kept = 0;
        let mut retained = 0usize;
        for c in &mut self.chunks {
            if retained.saturating_add(c.capacity()) > self.config.retained_bytes {
                break;
            }
            retained += c.capacity();
            c.cursor = 0;
            kept += 1;
        }
        for chunk in self.chunks.split_off(kept) {
            self.release_chunk(chunk);
        }
        self.current = 0;
        self.epoch += 1;
    }

    /// Finalizes and frees the region. Returns `LinearObligationsLive` if
    /// linear payloads were abandoned; those payloads are still finalized
    /// rather than leaked, but this is a protocol violation that the compiler
    /// must rule out statically.
    pub fn close(mut self) -> Result<RegionStats, RegionError> {
        let abandoned = self.live_linear;
        let stats = self.stats();
        self.marks.clear();
        self.finalize_from(0);
        drop(self);
        if abandoned > 0 {
            return Err(RegionError::LinearObligationsLive { count: abandoned });
        }
        Ok(stats)
    }
}

impl<B: ChunkBacking> Drop for Region<B> {
    fn drop(&mut self) {
        /// Runs on normal exit and while unwinding from a panicking finalizer:
        /// the remaining payloads are still finalized (a second panic aborts,
        /// as for `Vec`) and every chunk is returned to the backing.
        struct Finish<'a, B: ChunkBacking>(&'a mut Region<B>);
        impl<B: ChunkBacking> Drop for Finish<'_, B> {
            fn drop(&mut self) {
                self.0.finalize_from(0);
                for chunk in mem::take(&mut self.0.chunks) {
                    self.0.release_chunk(chunk);
                }
            }
        }
        let finish = Finish(self);
        finish.0.finalize_from(0);
    }
}
