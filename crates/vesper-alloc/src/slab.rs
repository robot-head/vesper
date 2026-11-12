//! Bounded, local byte slabs. Allocation owners retain their originating pool.
//!
//! A block is unique and initialized; a copyable key is diagnostic metadata,
//! never permission to dereference or free memory. Pool and block are `!Send`
//! and `!Sync` through `Rc`, so remote frees are rejected by Rust's type system.
//! This slice uses non-atomic reference counts, not source-level linear proofs.
use crate::{ChunkBacking, SystemBacking};
use std::alloc::Layout;
use std::cell::RefCell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Default slab reservation; there is no hugepage, DMA or MPK registration.
pub const SLAB_BYTES: usize = 64 * 1024;
/// Forty-eight explicit classes, in bytes. Requests also obey their alignment.
pub const SLAB_SIZE_CLASSES: [usize; 48] = [
    8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120, 128, 144, 160, 176, 192, 208,
    224, 240, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1280, 1536, 1792, 2048, 2560, 3072,
    3584, 4096, 5120, 6144, 7168, 8192, 10240, 12288, 14336, 16384,
];

/// Explicit reservation/live-owner limits. Metadata is bounded as well:
/// at most 4096 slabs, 8192 slots per slab, and `max_live_blocks` deferred frees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlabConfig {
    pub max_slabs: usize,
    pub max_live_blocks: usize,
}
impl Default for SlabConfig {
    fn default() -> Self {
        Self {
            max_slabs: 16,
            max_live_blocks: 65536,
        }
    }
}
impl SlabConfig {
    pub fn validate(self) -> Result<(), SlabError> {
        if self.max_slabs > 4096 {
            return Err(SlabError::InvalidConfig("max_slabs exceeds 4096"));
        }
        let slots = self
            .max_slabs
            .checked_mul(SLAB_BYTES / SLAB_SIZE_CLASSES[0])
            .ok_or(SlabError::SizeOverflow)?;
        if self.max_live_blocks > slots {
            return Err(SlabError::InvalidConfig(
                "max_live_blocks exceeds possible slab slots",
            ));
        }
        Layout::array::<SlabKey>(self.max_live_blocks).map_err(|_| SlabError::SizeOverflow)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlabId(u64);

/// Snapshot identity. Reusing an address/slot never revalidates an old key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlabKey {
    pool: SlabId,
    slab: u32,
    slot: u32,
    generation: u64,
}
impl SlabKey {
    pub fn pool(self) -> SlabId {
        self.pool
    }
    pub fn generation(self) -> u64 {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlabError {
    InvalidConfig(&'static str),
    InvalidAlignment { align: usize },
    UnsupportedLayout { size: usize, align: usize },
    SizeOverflow,
    CapacityExceeded { max_slabs: usize },
    LiveLimit { limit: usize },
    OutOfMemory,
    ExhaustedIdentities,
    ExhaustedGenerations,
    Foreign { expected: SlabId, found: SlabId },
    Stale,
    Busy,
    LiveBlocks { count: usize },
}
impl fmt::Display for SlabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SlabError {}

/// A rejected consuming operation always returns the unchanged block owner.
pub struct SlabOwnerError<T> {
    pub owner: T,
    pub error: SlabError,
}
impl<T> fmt::Debug for SlabOwnerError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlabOwnerError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl<T> fmt::Display for SlabOwnerError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl<T> std::error::Error for SlabOwnerError<T> {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlabStats {
    pub slabs: usize,
    pub reserved_bytes: usize,
    pub live_blocks: usize,
    pub requested_bytes: usize,
    pub occupied_bytes: usize,
    pub successful_allocations: u64,
}

struct Slot {
    next: Option<u32>,
    generation: u64,
    live: bool,
    requested: usize,
}
struct Slab {
    ptr: NonNull<u8>,
    layout: Layout,
    class: usize,
    slots: Vec<Slot>,
    free: Option<u32>,
}
struct Class {
    bytes: usize,
    available: Vec<usize>,
    slabs: usize,
}
struct State<B: ChunkBacking> {
    backing: B,
    slabs: Vec<Slab>,
    classes: Vec<Class>,
    stats: SlabStats,
    next_generation: u64,
}
struct Inner<B: ChunkBacking> {
    id: SlabId,
    config: SlabConfig,
    state: RefCell<State<B>>,
    // Custom unsafe ChunkBacking implementations may invoke local callbacks.
    // Dropping a block during provisioning must not reborrow `state` or panic.
    // This queue is reserved to the live-owner limit when the pool is created.
    deferred: RefCell<Vec<SlabKey>>,
}
impl<B: ChunkBacking> Drop for Inner<B> {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        for slab in state.slabs.drain(..) {
            // SAFETY: every slab was allocated by this backing with this exact
            // layout. Last Rc means no byte owner or borrowed byte slice lives.
            unsafe { state.backing.deallocate(slab.ptr, slab.layout) };
        }
    }
}

static NEXT_POOL: AtomicU64 = AtomicU64::new(1);
fn allocate_id(counter: &AtomicU64) -> Result<SlabId, SlabError> {
    let mut id = counter.load(Ordering::Relaxed);
    loop {
        let next = id.checked_add(1).ok_or(SlabError::ExhaustedIdentities)?;
        match counter.compare_exchange_weak(id, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(SlabId(id)),
            Err(current) => id = current,
        }
    }
}
fn fresh_id() -> Result<SlabId, SlabError> {
    allocate_id(&NEXT_POOL)
}
fn class_for(size: usize, align: usize) -> Result<usize, SlabError> {
    if !align.is_power_of_two() {
        return Err(SlabError::InvalidAlignment { align });
    }
    Layout::from_size_align(size, align).map_err(|_| SlabError::SizeOverflow)?;
    SLAB_SIZE_CLASSES
        .iter()
        .position(|class| *class >= size.max(1) && *class % align == 0)
        .ok_or(SlabError::UnsupportedLayout { size, align })
}
fn validate<B: ChunkBacking>(id: SlabId, state: &State<B>, key: SlabKey) -> Result<(), SlabError> {
    if id != key.pool {
        return Err(SlabError::Foreign {
            expected: id,
            found: key.pool,
        });
    }
    let slot = state
        .slabs
        .get(key.slab as usize)
        .and_then(|slab| slab.slots.get(key.slot as usize))
        .ok_or(SlabError::Stale)?;
    if !slot.live || slot.generation != key.generation {
        return Err(SlabError::Stale);
    }
    Ok(())
}
fn release<B: ChunkBacking>(
    id: SlabId,
    state: &mut State<B>,
    key: SlabKey,
) -> Result<(), SlabError> {
    validate(id, state, key)?;
    let slab_index = key.slab as usize;
    let slab = &mut state.slabs[slab_index];
    let was_full = slab.free.is_none();
    let slot = &mut slab.slots[key.slot as usize];
    slot.live = false;
    slot.next = slab.free;
    slab.free = Some(key.slot);
    state.stats.live_blocks -= 1;
    state.stats.requested_bytes -= slot.requested;
    state.stats.occupied_bytes -= state.classes[slab.class].bytes;
    if was_full {
        // Provisioning reserves for *all* this class's slabs; local free never
        // grows this vector and never invokes the backing/global allocator.
        state.classes[slab.class].available.push(slab_index);
    }
    Ok(())
}
impl<B: ChunkBacking> Inner<B> {
    fn drain_deferred(&self, state: &mut State<B>) {
        for key in self.deferred.borrow_mut().drain(..) {
            // Only the unique block Drop enqueues keys. Explicit free disarms
            // Drop first; each enqueued key therefore names one live slot.
            let result = release(self.id, state, key);
            debug_assert!(result.is_ok());
        }
    }
    fn drop_block(&self, key: SlabKey) {
        if let Ok(mut state) = self.state.try_borrow_mut() {
            self.drain_deferred(&mut state);
            let result = release(self.id, &mut state, key);
            debug_assert!(result.is_ok());
        } else {
            let mut deferred = self.deferred.borrow_mut();
            debug_assert!(deferred.len() < deferred.capacity());
            deferred.push(key);
        }
    }
}

/// A core-local bounded pool. Every block owns a non-atomic share of its
/// backing memory, so dropping this handle cannot invalidate a block.
///
/// ```compile_fail
/// use vesper_alloc::{ShardSlab, SlabConfig};
/// let pool = ShardSlab::new(SlabConfig::default()).unwrap();
/// std::thread::spawn(move || drop(pool)); // pool is !Send
/// ```
pub struct ShardSlab<B: ChunkBacking = SystemBacking> {
    inner: Rc<Inner<B>>,
}
impl<B: ChunkBacking> fmt::Debug for ShardSlab<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShardSlab")
            .field("id", &self.inner.id)
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}
impl ShardSlab<SystemBacking> {
    pub fn new(config: SlabConfig) -> Result<Self, SlabError> {
        Self::with_backing(config, SystemBacking)
    }
}
impl<B: ChunkBacking> ShardSlab<B> {
    pub fn with_backing(config: SlabConfig, backing: B) -> Result<Self, SlabError> {
        config.validate()?;
        let mut deferred = Vec::new();
        deferred
            .try_reserve_exact(config.max_live_blocks)
            .map_err(|_| SlabError::OutOfMemory)?;
        let mut classes = Vec::new();
        classes
            .try_reserve_exact(SLAB_SIZE_CLASSES.len())
            .map_err(|_| SlabError::OutOfMemory)?;
        for bytes in SLAB_SIZE_CLASSES {
            classes.push(Class {
                bytes,
                available: vec![],
                slabs: 0,
            });
        }
        Ok(Self {
            inner: Rc::new(Inner {
                id: fresh_id()?,
                config,
                deferred: RefCell::new(deferred),
                state: RefCell::new(State {
                    backing,
                    slabs: vec![],
                    classes,
                    stats: SlabStats::default(),
                    next_generation: 1,
                }),
            }),
        })
    }
    pub fn id(&self) -> SlabId {
        self.inner.id
    }
    pub fn config(&self) -> SlabConfig {
        self.inner.config
    }
    pub fn stats(&self) -> Result<SlabStats, SlabError> {
        let mut state = self
            .inner
            .state
            .try_borrow_mut()
            .map_err(|_| SlabError::Busy)?;
        self.inner.drain_deferred(&mut state);
        Ok(state.stats)
    }
    /// Validate diagnostic origin/generation only. Keys never yield references.
    pub fn validate(&self, key: SlabKey) -> Result<(), SlabError> {
        let mut state = self
            .inner
            .state
            .try_borrow_mut()
            .map_err(|_| SlabError::Busy)?;
        self.inner.drain_deferred(&mut state);
        validate(self.id(), &state, key)
    }
    /// Allocate initialized bytes. Zero-sized requests own one smallest slot.
    /// No request falls back to a non-slab allocation.
    pub fn allocate(&self, size: usize, align: usize) -> Result<SlabBlock<B>, SlabError> {
        let class = class_for(size, align)?;
        let mut state = self
            .inner
            .state
            .try_borrow_mut()
            .map_err(|_| SlabError::Busy)?;
        self.inner.drain_deferred(&mut state);
        if state.stats.live_blocks >= self.inner.config.max_live_blocks {
            return Err(SlabError::LiveLimit {
                limit: self.inner.config.max_live_blocks,
            });
        }
        let next = state
            .next_generation
            .checked_add(1)
            .ok_or(SlabError::ExhaustedGenerations)?;
        if state.classes[class].available.is_empty() {
            if state.slabs.len() >= self.inner.config.max_slabs {
                return Err(SlabError::CapacityExceeded {
                    max_slabs: self.inner.config.max_slabs,
                });
            }
            let bytes = state.classes[class].bytes;
            let count = SLAB_BYTES / bytes;
            let mut slots = Vec::new();
            slots
                .try_reserve_exact(count)
                .map_err(|_| SlabError::OutOfMemory)?;
            for i in 0..count {
                slots.push(Slot {
                    next: (i + 1 < count).then_some((i + 1) as u32),
                    generation: 0,
                    live: false,
                    requested: 0,
                });
            }
            state
                .slabs
                .try_reserve(1)
                .map_err(|_| SlabError::OutOfMemory)?;
            let class_info = &mut state.classes[class];
            let needed = class_info.slabs + 1 - class_info.available.len();
            class_info
                .available
                .try_reserve(needed)
                .map_err(|_| SlabError::OutOfMemory)?;
            let layout = Layout::from_size_align(SLAB_BYTES, SLAB_SIZE_CLASSES[47])
                .map_err(|_| SlabError::SizeOverflow)?;
            let ptr = state
                .backing
                .allocate(layout)
                .ok_or(SlabError::OutOfMemory)?;
            // All fallible metadata reservations precede backing allocation.
            let index = state.slabs.len();
            state.slabs.push(Slab {
                ptr,
                layout,
                class,
                slots,
                free: Some(0),
            });
            state.classes[class].available.push(index);
            state.classes[class].slabs += 1;
            state.stats.slabs += 1;
            state.stats.reserved_bytes += SLAB_BYTES;
        }
        self.inner.drain_deferred(&mut state);
        let slab_index = *state.classes[class]
            .available
            .last()
            .ok_or(SlabError::Stale)?;
        let generation = state.next_generation;
        let slab = &mut state.slabs[slab_index];
        let slot_index = slab.free.ok_or(SlabError::Stale)?;
        let stride = SLAB_SIZE_CLASSES[class];
        let offset = (slot_index as usize)
            .checked_mul(stride)
            .ok_or(SlabError::SizeOverflow)?;
        let end = offset.checked_add(size).ok_or(SlabError::SizeOverflow)?;
        if end > SLAB_BYTES {
            return Err(SlabError::SizeOverflow);
        }
        let slot = &mut slab.slots[slot_index as usize];
        slab.free = slot.next;
        slot.live = true;
        slot.generation = generation;
        slot.requested = size;
        // SAFETY: class slots partition this slab. Requested size <= stride,
        // and the stride is a multiple of the requested power-of-two alignment.
        let ptr = unsafe { NonNull::new_unchecked(slab.ptr.as_ptr().add(offset)) };
        if slab.free.is_none() {
            state.classes[class].available.pop();
        }
        state.next_generation = next;
        state.stats.successful_allocations += 1;
        state.stats.live_blocks += 1;
        state.stats.requested_bytes += size;
        state.stats.occupied_bytes += stride;
        // SAFETY: uniquely allocated nonoverlapping live slot, valid size.
        unsafe { ptr.as_ptr().write_bytes(0, size) };
        Ok(SlabBlock {
            inner: self.inner.clone(),
            key: Some(SlabKey {
                pool: self.id(),
                slab: slab_index as u32,
                slot: slot_index,
                generation,
            }),
            ptr,
            len: size,
            align,
            capacity: stride,
        })
    }
    pub fn allocate_copy(&self, bytes: &[u8], align: usize) -> Result<SlabBlock<B>, SlabError> {
        let mut block = self.allocate(bytes.len(), align)?;
        block.copy_from_slice(bytes);
        Ok(block)
    }
    /// Explicit local free. Foreign/stale/busy failures return the whole owner.
    pub fn free(&self, mut block: SlabBlock<B>) -> Result<(), SlabOwnerError<SlabBlock<B>>> {
        let result = (|| {
            let mut state = self
                .inner
                .state
                .try_borrow_mut()
                .map_err(|_| SlabError::Busy)?;
            self.inner.drain_deferred(&mut state);
            release(self.id(), &mut state, block.key())
        })();
        match result {
            Ok(()) => {
                block.key = None;
                Ok(())
            }
            Err(error) => Err(SlabOwnerError {
                owner: block,
                error,
            }),
        }
    }
    /// Failure-atomic resize; shrinking keeps the existing slot. Newly visible
    /// bytes are zeroed. A larger replacement gets a fresh generation/key.
    pub fn resize(
        &self,
        mut block: SlabBlock<B>,
        size: usize,
        align: usize,
    ) -> Result<SlabBlock<B>, SlabOwnerError<SlabBlock<B>>> {
        let checked = class_for(size, align).and_then(|_| self.validate(block.key()));
        if let Err(error) = checked {
            return Err(SlabOwnerError {
                owner: block,
                error,
            });
        }
        if size <= block.capacity && block.ptr.as_ptr().addr().is_multiple_of(align) {
            let result = (|| {
                let mut state = self
                    .inner
                    .state
                    .try_borrow_mut()
                    .map_err(|_| SlabError::Busy)?;
                let key = block.key();
                validate(self.id(), &state, key)?;
                let old = state.slabs[key.slab as usize].slots[key.slot as usize].requested;
                state.slabs[key.slab as usize].slots[key.slot as usize].requested = size;
                state.stats.requested_bytes = state.stats.requested_bytes - old + size;
                Ok(())
            })();
            if let Err(error) = result {
                return Err(SlabOwnerError {
                    owner: block,
                    error,
                });
            }
            if size > block.len {
                // SAFETY: exclusive owner, extension remains inside its slot.
                unsafe {
                    block
                        .ptr
                        .as_ptr()
                        .add(block.len)
                        .write_bytes(0, size - block.len)
                };
            }
            block.len = size;
            block.align = align;
            return Ok(block);
        }
        let mut replacement = match self.allocate(size, align) {
            Ok(replacement) => replacement,
            Err(error) => {
                return Err(SlabOwnerError {
                    owner: block,
                    error,
                });
            }
        };
        let copy = block.len.min(size);
        replacement[..copy].copy_from_slice(&block[..copy]);
        drop(block);
        Ok(replacement)
    }
    /// Local copy-transfer to this pool. Allocation failure returns the
    /// unchanged source owner; success frees the old slot only after copying.
    /// This does not make either pool or block Send or migrate a worker.
    pub fn transfer_from<C: ChunkBacking>(
        &self,
        block: SlabBlock<C>,
    ) -> Result<SlabBlock<B>, SlabOwnerError<SlabBlock<C>>> {
        let replacement = match self.allocate_copy(&block, block.align) {
            Ok(replacement) => replacement,
            Err(error) => {
                return Err(SlabOwnerError {
                    owner: block,
                    error,
                });
            }
        };
        drop(block);
        Ok(replacement)
    }
    /// Fail without destroying the pool when live owners remain. Dropping
    /// the returned pool is nevertheless memory-safe: blocks retain its slabs.
    pub fn close(self) -> Result<SlabStats, SlabCloseError<B>> {
        match self.stats() {
            Ok(stats) if stats.live_blocks == 0 => Ok(stats),
            Ok(stats) => Err(SlabCloseError {
                pool: self,
                error: SlabError::LiveBlocks {
                    count: stats.live_blocks,
                },
            }),
            Err(error) => Err(SlabCloseError { pool: self, error }),
        }
    }
}

pub struct SlabCloseError<B: ChunkBacking> {
    pub pool: ShardSlab<B>,
    pub error: SlabError,
}
impl<B: ChunkBacking> fmt::Debug for SlabCloseError<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlabCloseError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl<B: ChunkBacking> fmt::Display for SlabCloseError<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl<B: ChunkBacking> std::error::Error for SlabCloseError<B> {}

/// Unique initialized byte allocation. No typed values, raw ownership
/// conversion, public constructors, Clone or Copy are provided.
///
/// ```compile_fail
/// use vesper_alloc::{ShardSlab, SlabConfig};
/// let pool = ShardSlab::new(SlabConfig::default()).unwrap();
/// let block = pool.allocate(8, 8).unwrap();
/// std::thread::spawn(move || drop(block)); // remote free is unsupported
/// ```
pub struct SlabBlock<B: ChunkBacking = SystemBacking> {
    inner: Rc<Inner<B>>,
    key: Option<SlabKey>,
    ptr: NonNull<u8>,
    len: usize,
    align: usize,
    capacity: usize,
}
impl<B: ChunkBacking> SlabBlock<B> {
    pub fn key(&self) -> SlabKey {
        self.key.expect("live block has a key")
    }
    pub fn alignment(&self) -> usize {
        self.align
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    /// Inspect the address; dereferencing a retained raw pointer remains unsafe.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr()
    }
}
impl<B: ChunkBacking> Deref for SlabBlock<B> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: unique owner keeps backing alive. The requested byte range
        // was zeroed/copied; safe shared borrowing precludes mutating this block.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}
impl<B: ChunkBacking> DerefMut for SlabBlock<B> {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: one non-Clone owner per slot and exclusive Rust borrow.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}
impl<B: ChunkBacking> Drop for SlabBlock<B> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.inner.drop_block(key);
        }
    }
}
impl<B: ChunkBacking> fmt::Debug for SlabBlock<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlabBlock")
            .field("key", &self.key)
            .field("len", &self.len)
            .field("align", &self.align)
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_and_generations_exhaust_without_wrapping_or_changing_owners() {
        let ids = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_id(&ids).unwrap(), SlabId(u64::MAX - 1));
        assert_eq!(allocate_id(&ids), Err(SlabError::ExhaustedIdentities));
        assert_eq!(ids.load(Ordering::Relaxed), u64::MAX);
        let pool = ShardSlab::new(SlabConfig {
            max_slabs: 1,
            max_live_blocks: 4,
        })
        .unwrap();
        let owner = pool.allocate_copy(b"live", 8).unwrap();
        let before = pool.stats().unwrap();
        pool.inner.state.borrow_mut().next_generation = u64::MAX;
        assert_eq!(
            pool.allocate(8, 8).unwrap_err(),
            SlabError::ExhaustedGenerations
        );
        assert_eq!(pool.stats().unwrap(), before);
        assert_eq!(&*owner, b"live");
        pool.free(owner).unwrap();
    }
}
