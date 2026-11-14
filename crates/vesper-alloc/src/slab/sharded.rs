//! Per-thread size-class shards with batched remote-free queues (§15.3, Tier 1).
//!
//! A [`ShardedSlab`] is a process-shareable heap made of a fixed number of
//! shards. A thread claims one shard as a [`Shard`] handle, which owns that
//! shard's slabs and size-class free lists outright: same-shard allocation and
//! free pop or push a preallocated free stack and touch no lock, atomic or
//! global allocator once the class has a provisioned slab with a free slot.
//!
//! A [`ShardBlock`] is `Send`. When a thread that holds a different shard frees
//! it, the free is appended to a per-target outgoing batch owned by the freeing
//! shard; a full batch (64 frees by default) is appended to the origin shard's
//! mutex-protected remote queue under one lock acquisition. The origin shard
//! drains its queue on its allocation slow path, or explicitly. This follows the
//! snmalloc message-passing shape; it is not lock-free (the queue is a `Mutex`),
//! and there is no NUMA, hugepage, DMA or MPK placement.
//!
//! Blocks are linear owners: their only exits are [`Shard::free`],
//! [`ShardedSlab::free_unbound`] and the [`crate::Alloc`] trait. Dropping one
//! is detected, never silently released: the slot stays allocated, the drop is
//! counted process-wide ([`dropped_shard_blocks`]) and [`ShardedSlab::close`]
//! keeps reporting it as live.
//!
//! A claimed shard can move to another thread but cannot be shared:
//!
//! ```compile_fail
//! fn shared<T: Sync>(_: &T) {}
//! let heap = vesper_alloc::ShardedSlab::new(Default::default()).unwrap();
//! let shard = heap.claim(0).unwrap();
//! shared(&shard);
//! ```
//!
//! Blocks are unique owners:
//!
//! ```compile_fail
//! let heap = vesper_alloc::ShardedSlab::new(Default::default()).unwrap();
//! let mut shard = heap.claim(0).unwrap();
//! let block = shard.allocate(8, 8).unwrap();
//! let copy = block.clone();
//! ```
use super::{SLAB_BYTES, SLAB_SIZE_CLASSES, SlabError, SlabOwnerError, class_for};
use std::alloc::{self, Layout};
use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Default number of remote frees batched per target shard before a flush.
pub const REMOTE_FREE_BATCH: usize = 64;
/// Largest shard count a heap accepts.
pub const MAX_SHARDS: usize = 256;

static NEXT_HEAP: AtomicU64 = AtomicU64::new(1);
static DROPPED_BLOCKS: AtomicU64 = AtomicU64::new(0);

/// Process-wide count of [`ShardBlock`]s dropped without an explicit free.
pub fn dropped_shard_blocks() -> u64 {
    DROPPED_BLOCKS.load(Ordering::Relaxed)
}

/// Limits of a sharded heap. Every limit is checked before memory is reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShardedSlabConfig {
    /// Number of shards (1..=256); one per worker thread.
    pub shards: usize,
    /// Physical 64 KiB slabs each shard may provision (1..=4096).
    pub max_slabs_per_shard: usize,
    /// Remote frees batched per target before one locked append (1..=4096).
    pub remote_batch: usize,
}

impl Default for ShardedSlabConfig {
    fn default() -> Self {
        Self {
            shards: 4,
            max_slabs_per_shard: 16,
            remote_batch: REMOTE_FREE_BATCH,
        }
    }
}

impl ShardedSlabConfig {
    pub fn validate(self) -> Result<(), SlabError> {
        if self.shards == 0 || self.shards > MAX_SHARDS {
            return Err(SlabError::InvalidConfig("shards must be 1..=256"));
        }
        if self.max_slabs_per_shard == 0 || self.max_slabs_per_shard > 4096 {
            return Err(SlabError::InvalidConfig(
                "max_slabs_per_shard must be 1..=4096",
            ));
        }
        if self.remote_batch == 0 || self.remote_batch > 4096 {
            return Err(SlabError::InvalidConfig("remote_batch must be 1..=4096"));
        }
        Ok(())
    }
}

/// Process-unique heap identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShardedSlabId(u64);

impl ShardedSlabId {
    pub fn bits(self) -> u64 {
        self.0
    }
}

/// Counters of one heap. `live_blocks` excludes frees still queued remotely.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShardedSlabStats {
    pub slabs: usize,
    pub reserved_bytes: usize,
    pub live_blocks: usize,
    pub local_frees: u64,
    pub remote_frees: u64,
    pub remote_batches: u64,
    pub queued_remote_frees: usize,
}

#[derive(Clone, Copy, Debug)]
struct RemoteFree {
    slab: u32,
    slot: u32,
    generation: u64,
}

struct SlabMem {
    ptr: NonNull<u8>,
    class: usize,
    /// Generation of the current owner of each slot; 0 means vacant.
    slots: Vec<u64>,
}

struct ClassList {
    /// Free `(slab, slot)` pairs. Capacity always covers every slot of every
    /// slab of this class, so pushes on free never allocate.
    free: Vec<(u32, u32)>,
}

struct LocalShard {
    slabs: Vec<SlabMem>,
    classes: Vec<ClassList>,
    live: usize,
    next_generation: u64,
    local_frees: u64,
    remote_frees: u64,
    remote_batches: u64,
}

// SAFETY: `LocalShard` uniquely owns its slab allocations; the raw pointers
// are not shared with any other `LocalShard`, and block owners only touch the
// disjoint slot bytes they own. Moving the bookkeeping between threads is
// sound because it is reachable from exactly one claimant at a time.
unsafe impl Send for LocalShard {}

fn slab_layout() -> Layout {
    // 64 KiB slabs aligned to 16 KiB, matching the local ShardSlab.
    Layout::from_size_align(SLAB_BYTES, SLAB_SIZE_CLASSES[47]).expect("constant layout")
}

impl LocalShard {
    fn new() -> Result<Self, SlabError> {
        let mut classes = Vec::new();
        classes
            .try_reserve_exact(SLAB_SIZE_CLASSES.len())
            .map_err(|_| SlabError::OutOfMemory)?;
        for _ in SLAB_SIZE_CLASSES {
            classes.push(ClassList { free: Vec::new() });
        }
        Ok(Self {
            slabs: Vec::new(),
            classes,
            live: 0,
            next_generation: 1,
            local_frees: 0,
            remote_frees: 0,
            remote_batches: 0,
        })
    }

    fn release(&mut self, slab: u32, slot: u32, generation: u64) -> Result<(), SlabError> {
        let mem = self.slabs.get_mut(slab as usize).ok_or(SlabError::Stale)?;
        let current = mem.slots.get_mut(slot as usize).ok_or(SlabError::Stale)?;
        if *current != generation || generation == 0 {
            return Err(SlabError::Stale);
        }
        *current = 0;
        let list = &mut self.classes[mem.class];
        debug_assert!(list.free.len() < list.free.capacity());
        list.free.push((slab, slot));
        self.live -= 1;
        Ok(())
    }

    fn provision(&mut self, class: usize, max_slabs: usize) -> Result<(), SlabError> {
        if self.slabs.len() >= max_slabs {
            return Err(SlabError::CapacityExceeded { max_slabs });
        }
        let count = SLAB_BYTES / SLAB_SIZE_CLASSES[class];
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(count)
            .map_err(|_| SlabError::OutOfMemory)?;
        slots.resize(count, 0);
        self.slabs
            .try_reserve(1)
            .map_err(|_| SlabError::OutOfMemory)?;
        self.classes[class]
            .free
            .try_reserve(count)
            .map_err(|_| SlabError::OutOfMemory)?;
        // SAFETY: the layout has a nonzero size.
        let ptr =
            NonNull::new(unsafe { alloc::alloc(slab_layout()) }).ok_or(SlabError::OutOfMemory)?;
        let index = u32::try_from(self.slabs.len()).map_err(|_| SlabError::SizeOverflow)?;
        self.slabs.push(SlabMem { ptr, class, slots });
        // Push in reverse so slot 0 is handed out first.
        let list = &mut self.classes[class].free;
        for slot in (0..count as u32).rev() {
            list.push((index, slot));
        }
        Ok(())
    }

    fn reserved(&self) -> usize {
        self.slabs.len() * SLAB_BYTES
    }

    /// Returns every slab to the global allocator. Only called when no block
    /// of this shard is live (no owner can reach the memory).
    fn deallocate_all(&mut self) {
        for slab in self.slabs.drain(..) {
            // SAFETY: allocated in `provision` with `slab_layout()`, freed once.
            unsafe { alloc::dealloc(slab.ptr.as_ptr(), slab_layout()) };
        }
    }
}

struct ShardShared {
    claimed: AtomicBool,
    /// Parked bookkeeping while no thread claims this shard.
    parked: Mutex<Option<LocalShard>>,
    remote: Mutex<Vec<RemoteFree>>,
    queued: AtomicUsize,
}

struct Shared {
    id: ShardedSlabId,
    config: ShardedSlabConfig,
    shards: Box<[ShardShared]>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while a queue lock is held leaves only plain data behind.
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    fn push_remote(&self, shard: usize, frees: &mut Vec<RemoteFree>) {
        if frees.is_empty() {
            return;
        }
        let target = &self.shards[shard];
        let mut queue = lock(&target.remote);
        // Capacity grows to the largest backlog seen and is then reused.
        queue.extend(frees.drain(..));
        target.queued.store(queue.len(), Ordering::Release);
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        for shard in self.shards.iter_mut() {
            let Some(mut local) = shard
                .parked
                .get_mut()
                .unwrap_or_else(|p| p.into_inner())
                .take()
            else {
                continue;
            };
            let queue = mem::take(shard.remote.get_mut().unwrap_or_else(|p| p.into_inner()));
            for f in queue {
                let _ = local.release(f.slab, f.slot, f.generation);
            }
            if local.live == 0 {
                local.deallocate_all();
            } else {
                // Live (or dropped) owners may still reach these bytes: keep
                // the slabs allocated rather than free memory in use.
                mem::forget(local);
            }
        }
    }
}

/// A shareable sharded heap. Clones name the same heap.
#[derive(Clone)]
pub struct ShardedSlab {
    shared: Arc<Shared>,
}

impl fmt::Debug for ShardedSlab {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShardedSlab")
            .field("id", &self.shared.id)
            .field("config", &self.shared.config)
            .finish()
    }
}

/// `close` failure: the heap is returned so the caller can free and retry.
#[derive(Debug)]
pub struct ShardedCloseError {
    pub heap: ShardedSlab,
    pub error: SlabError,
}

impl fmt::Display for ShardedCloseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for ShardedCloseError {}

impl ShardedSlab {
    pub fn new(config: ShardedSlabConfig) -> Result<Self, SlabError> {
        config.validate()?;
        let id = {
            let mut id = NEXT_HEAP.load(Ordering::Relaxed);
            loop {
                let next = id.checked_add(1).ok_or(SlabError::ExhaustedIdentities)?;
                match NEXT_HEAP.compare_exchange_weak(
                    id,
                    next,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break ShardedSlabId(id),
                    Err(current) => id = current,
                }
            }
        };
        let mut shards = Vec::new();
        shards
            .try_reserve_exact(config.shards)
            .map_err(|_| SlabError::OutOfMemory)?;
        for _ in 0..config.shards {
            let mut remote = Vec::new();
            remote
                .try_reserve(config.remote_batch)
                .map_err(|_| SlabError::OutOfMemory)?;
            shards.push(ShardShared {
                claimed: AtomicBool::new(false),
                parked: Mutex::new(Some(LocalShard::new()?)),
                remote: Mutex::new(remote),
                queued: AtomicUsize::new(0),
            });
        }
        Ok(Self {
            shared: Arc::new(Shared {
                id,
                config,
                shards: shards.into_boxed_slice(),
            }),
        })
    }

    pub fn id(&self) -> ShardedSlabId {
        self.shared.id
    }

    pub fn config(&self) -> ShardedSlabConfig {
        self.shared.config
    }

    /// Claims shard `index` for the calling thread. A shard has at most one
    /// claimant; dropping the [`Shard`] flushes its outgoing batches and parks
    /// its bookkeeping for the next claimant.
    pub fn claim(&self, index: usize) -> Result<Shard, SlabError> {
        let shard = self
            .shared
            .shards
            .get(index)
            .ok_or(SlabError::InvalidConfig("shard index out of range"))?;
        if shard
            .claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(SlabError::Busy);
        }
        let local = lock(&shard.parked).take();
        let Some(local) = local else {
            shard.claimed.store(false, Ordering::Release);
            return Err(SlabError::Busy);
        };
        let mut outgoing = Vec::new();
        let reserved = outgoing
            .try_reserve_exact(self.shared.shards.len())
            .map_err(|_| SlabError::OutOfMemory)
            .and_then(|()| {
                for _ in 0..self.shared.shards.len() {
                    let mut batch = Vec::new();
                    batch
                        .try_reserve_exact(self.shared.config.remote_batch)
                        .map_err(|_| SlabError::OutOfMemory)?;
                    outgoing.push(batch);
                }
                Ok(())
            });
        if let Err(e) = reserved {
            *lock(&shard.parked) = Some(local);
            shard.claimed.store(false, Ordering::Release);
            return Err(e);
        }
        Ok(Shard {
            shared: self.shared.clone(),
            index,
            local: Some(local),
            outgoing,
            _not_sync: PhantomData,
        })
    }

    /// Frees a block from a thread that holds no shard: one locked append to
    /// the origin's remote queue. A foreign block is returned unchanged.
    pub fn free_unbound(&self, block: ShardBlock) -> Result<(), SlabOwnerError<ShardBlock>> {
        if block.heap != self.shared.id {
            let error = SlabError::ForeignHeap {
                expected: self.shared.id.0,
                found: block.heap.0,
            };
            return Err(SlabOwnerError {
                owner: block,
                error,
            });
        }
        let mut one = Vec::new();
        if one.try_reserve_exact(1).is_err() {
            return Err(SlabOwnerError {
                owner: block,
                error: SlabError::OutOfMemory,
            });
        }
        let shard = block.shard as usize;
        one.push(block.disarm());
        self.shared.push_remote(shard, &mut one);
        Ok(())
    }

    /// Statistics over parked shards. Claimed shards are reported by
    /// [`Shard::stats`]; their slabs are omitted here (returns `Busy` if any
    /// shard is claimed, since its counters are owned by another thread).
    pub fn stats(&self) -> Result<ShardedSlabStats, SlabError> {
        let mut stats = ShardedSlabStats::default();
        for shard in self.shared.shards.iter() {
            let parked = lock(&shard.parked);
            let local = parked.as_ref().ok_or(SlabError::Busy)?;
            let queued = lock(&shard.remote).len();
            accumulate(&mut stats, local, queued);
        }
        Ok(stats)
    }

    /// Drains every remote queue and reports success only when no block is
    /// live (dropped owners count as live). On failure the heap is returned.
    pub fn close(self) -> Result<ShardedSlabStats, ShardedCloseError> {
        let result = (|| {
            let mut live = 0;
            for shard in self.shared.shards.iter() {
                let mut parked = lock(&shard.parked);
                let local = parked.as_mut().ok_or(SlabError::Busy)?;
                drain_queue(shard, local);
                live += local.live;
            }
            if live != 0 {
                return Err(SlabError::LiveBlocks { count: live });
            }
            self.stats()
        })();
        match result {
            Ok(stats) => Ok(stats),
            Err(error) => Err(ShardedCloseError { heap: self, error }),
        }
    }
}

fn accumulate(stats: &mut ShardedSlabStats, local: &LocalShard, queued: usize) {
    stats.slabs += local.slabs.len();
    stats.reserved_bytes += local.reserved();
    stats.live_blocks += local.live;
    stats.local_frees += local.local_frees;
    stats.remote_frees += local.remote_frees;
    stats.remote_batches += local.remote_batches;
    stats.queued_remote_frees += queued;
}

fn drain_queue(shard: &ShardShared, local: &mut LocalShard) -> usize {
    if shard.queued.load(Ordering::Acquire) == 0 {
        return 0;
    }
    let mut queue = lock(&shard.remote);
    let n = queue.len();
    // `drain` keeps the queue's capacity, so steady-state remote frees do not
    // allocate on either side.
    for f in queue.drain(..) {
        let ok = local.release(f.slab, f.slot, f.generation).is_ok();
        debug_assert!(ok, "remote free of a non-live slot");
    }
    shard.queued.store(0, Ordering::Release);
    if n > 0 {
        local.remote_frees += n as u64;
        local.remote_batches += 1;
    }
    n
}

/// A claimed shard: the thread-local allocation handle. `Send` (it can be
/// moved to the worker that will use it) but not `Sync`.
pub struct Shard {
    shared: Arc<Shared>,
    index: usize,
    local: Option<LocalShard>,
    outgoing: Vec<Vec<RemoteFree>>,
    _not_sync: PhantomData<Cell<()>>,
}

impl fmt::Debug for Shard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shard")
            .field("heap", &self.shared.id)
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

impl Shard {
    fn local(&mut self) -> &mut LocalShard {
        self.local.as_mut().expect("claimed shard bookkeeping")
    }

    pub fn heap(&self) -> ShardedSlabId {
        self.shared.id
    }

    pub fn index(&self) -> usize {
        self.index
    }

    /// Allocates `size` zeroed bytes aligned to `align` from this shard.
    pub fn allocate(&mut self, size: usize, align: usize) -> Result<ShardBlock, SlabError> {
        let class = class_for(size, align)?;
        let max_slabs = self.shared.config.max_slabs_per_shard;
        if self.local().classes[class].free.is_empty() {
            let shard = &self.shared.shards[self.index];
            let local = self.local.as_mut().expect("claimed shard bookkeeping");
            drain_queue(shard, local);
            if local.classes[class].free.is_empty() {
                local.provision(class, max_slabs)?;
            }
        }
        let heap = self.shared.id;
        let shard = self.index as u16;
        let local = self.local();
        let generation = local.next_generation;
        let next = generation
            .checked_add(1)
            .ok_or(SlabError::ExhaustedGenerations)?;
        let (slab, slot) = local.classes[class].free.pop().ok_or(SlabError::Stale)?;
        let mem = &mut local.slabs[slab as usize];
        mem.slots[slot as usize] = generation;
        let stride = SLAB_SIZE_CLASSES[class];
        let offset = slot as usize * stride;
        debug_assert!(offset + stride <= SLAB_BYTES);
        // SAFETY: slots partition the slab; `offset + size <= SLAB_BYTES`.
        let ptr = unsafe { NonNull::new_unchecked(mem.ptr.as_ptr().add(offset)) };
        // SAFETY: the slot is exclusively owned by the new block.
        unsafe { ptr.as_ptr().write_bytes(0, size) };
        local.next_generation = next;
        local.live += 1;
        Ok(ShardBlock {
            heap,
            shard,
            slab,
            slot,
            generation,
            ptr,
            len: size,
            capacity: stride,
            armed: true,
        })
    }

    /// Frees a block. A block of this shard is pushed on the local free stack;
    /// a block of another shard joins that shard's outgoing batch, which is
    /// appended to its remote queue once it holds `remote_batch` frees.
    pub fn free(&mut self, block: ShardBlock) -> Result<(), SlabOwnerError<ShardBlock>> {
        if block.heap != self.shared.id {
            let error = SlabError::ForeignHeap {
                expected: self.shared.id.0,
                found: block.heap.0,
            };
            return Err(SlabOwnerError {
                owner: block,
                error,
            });
        }
        let target = block.shard as usize;
        if target == self.index {
            let local = self.local();
            if let Err(error) = local.release(block.slab, block.slot, block.generation) {
                return Err(SlabOwnerError {
                    owner: block,
                    error,
                });
            }
            local.local_frees += 1;
            block.disarm();
            return Ok(());
        }
        let batch = self.shared.config.remote_batch;
        let out = &mut self.outgoing[target];
        out.push(block.disarm());
        if out.len() >= batch {
            self.shared.push_remote(target, out);
        }
        Ok(())
    }

    /// Appends every partially filled outgoing batch to its target queue.
    pub fn flush_remote(&mut self) {
        for (target, out) in self.outgoing.iter_mut().enumerate() {
            self.shared.push_remote(target, out);
        }
    }

    /// Applies the frees other shards queued for this one; returns how many.
    pub fn drain_remote(&mut self) -> usize {
        let shard = &self.shared.shards[self.index];
        let local = self.local.as_mut().expect("claimed shard bookkeeping");
        drain_queue(shard, local)
    }

    /// Frees sitting in this shard's outgoing batches, not yet sent.
    pub fn pending_outgoing(&self) -> usize {
        self.outgoing.iter().map(Vec::len).sum()
    }

    pub fn stats(&self) -> ShardedSlabStats {
        let mut stats = ShardedSlabStats::default();
        let queued = lock(&self.shared.shards[self.index].remote).len();
        accumulate(
            &mut stats,
            self.local.as_ref().expect("claimed shard bookkeeping"),
            queued,
        );
        stats
    }
}

impl Drop for Shard {
    fn drop(&mut self) {
        self.flush_remote();
        let shard = &self.shared.shards[self.index];
        *lock(&shard.parked) = self.local.take();
        shard.claimed.store(false, Ordering::Release);
    }
}

/// A unique, initialized byte allocation of a sharded heap (`RawBlock`).
/// `Send + Sync` like `Box<[u8]>`; neither `Clone` nor `Copy`.
#[must_use = "a ShardBlock must be freed; dropping it leaks its slot"]
pub struct ShardBlock {
    heap: ShardedSlabId,
    shard: u16,
    slab: u32,
    slot: u32,
    generation: u64,
    ptr: NonNull<u8>,
    len: usize,
    capacity: usize,
    armed: bool,
}

// SAFETY: a block uniquely owns its slot bytes, which stay allocated until the
// block is consumed by a free (or forever, if the heap is dropped while it is
// live). Shared references only expose `&[u8]`.
unsafe impl Send for ShardBlock {}
// SAFETY: see above; `&ShardBlock` grants read-only access to plain bytes.
unsafe impl Sync for ShardBlock {}

impl ShardBlock {
    fn disarm(mut self) -> RemoteFree {
        self.armed = false;
        RemoteFree {
            slab: self.slab,
            slot: self.slot,
            generation: self.generation,
        }
    }

    pub fn heap(&self) -> ShardedSlabId {
        self.heap
    }

    /// The shard this block must return to.
    pub fn shard(&self) -> usize {
        self.shard as usize
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Size-class stride backing this block.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Inspect the address; dereferencing a retained raw pointer is unsafe.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr()
    }
}

impl Deref for ShardBlock {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: `len` initialized bytes owned by this block.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for ShardBlock {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, with unique access through `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for ShardBlock {
    fn drop(&mut self) {
        if self.armed {
            // Detected leak: the slot stays allocated and is reported by close.
            DROPPED_BLOCKS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl fmt::Debug for ShardBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShardBlock")
            .field("heap", &self.heap)
            .field("shard", &self.shard)
            .field("generation", &self.generation)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}
