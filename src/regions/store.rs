//! Region blocks backed by [`vesper_alloc::Region`], shared by the evaluator
//! (payload `Value`) and the native runtime calls (payload words).
//!
//! # Accounting (`vesper.region.accounting/1`)
//!
//! Every backend charges a region the same bytes, so `r.used()` and the
//! point at which a bound is exhausted agree between the evaluator, the JIT
//! and AOT executables (whose generated runtime, `src/regions/aot_runtime.rs`, implements the
//! same rules in generated code):
//!
//! * a payload of type `T` occupies [`slot_bytes`]`(T)` bytes: its native
//!   layout size, which is a multiple of 8 and at least 8;
//! * a block of capacity `C` is one chunk of `C` bytes; an allocation fails
//!   when it does not fit after the bytes in use;
//! * moving the newest unprotected payload out (`take`) gives its bytes back,
//!   as do the consecutive moved payloads below it (stopping at the newest
//!   watermark); `rewind(m)` restores the bytes and allocations recorded by
//!   `m` and invalidates newer watermarks; `commit(m)` drops `m` and then
//!   gives back the moved tail;
//! * a region holds at most [`MAX_ALLOCATIONS`] tracked allocations and
//!   [`MAX_CHECKPOINTS`] outstanding watermarks.
//!
//! These are the rules of [`vesper_alloc::Region`] with one chunk, 8-byte
//! alignment and byte payloads ([`Region::alloc_bytes`]); the store uses that
//! allocator, not a copy of its rules.
//!
//! Source handles are plain integers: a box handle is an index into the
//! block's box table and a watermark handle an index into its mark table.
//! The allocator, not the table, decides validity: a rewound, released or
//! reset payload makes its [`RegionBytes`] stale, so every access is checked
//! by the region's identity and serial and fails with a typed error.
use std::collections::BTreeMap;
use std::fmt;
use vesper_alloc::{Region, RegionBytes, RegionConfig, Watermark};

/// Tracked allocations per region (the allocator's default bound).
pub const MAX_ALLOCATIONS: usize = 1 << 20;
/// Outstanding watermarks per region (the allocator's default bound).
pub const MAX_CHECKPOINTS: usize = 256;
/// Alignment of every payload.
pub const SLOT_ALIGN: usize = 8;

/// Bytes a payload of native layout size `size` occupies.
pub fn slot_bytes(size: usize) -> usize {
    size.div_ceil(SLOT_ALIGN).max(1) * SLOT_ALIGN
}

/// Allocator configuration of a block of capacity `capacity`: one chunk.
pub fn config(capacity: usize) -> RegionConfig {
    RegionConfig {
        initial_chunk_bytes: capacity,
        max_chunk_bytes: capacity,
        max_reserved_bytes: capacity,
        retained_bytes: capacity,
        max_align: SLOT_ALIGN,
        max_allocations: MAX_ALLOCATIONS,
        max_checkpoints: MAX_CHECKPOINTS,
    }
}

/// Typed failures of region operations at run time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionFault {
    /// The block's capability is not active (no such region).
    Inactive,
    /// The handle does not name an allocation of this region.
    UnknownHandle,
    /// The allocation was rewound, reset or moved out.
    Stale,
    /// The region's byte, allocation or checkpoint bound was reached.
    Exhausted(String),
    /// A rewind or close found live linear payloads.
    LinearLive(usize),
    /// The region configuration was rejected.
    Config(String),
}
impl fmt::Display for RegionFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive => write!(f, "region capability is no longer active"),
            Self::UnknownHandle => write!(f, "region handle does not belong to this region"),
            Self::Stale => write!(f, "region handle is stale (rewound, reset or moved out)"),
            Self::Exhausted(e) => write!(f, "region bound exhausted: {e}"),
            Self::LinearLive(n) => write!(f, "{n} linear region payload(s) are still live"),
            Self::Config(e) => write!(f, "invalid region configuration: {e}"),
        }
    }
}
impl std::error::Error for RegionFault {}

struct Boxed<T> {
    bytes: RegionBytes,
    /// The evaluator's payload value (`None` for native word payloads,
    /// which live in the region bytes themselves).
    value: Option<T>,
}

struct Block<T: 'static> {
    region: Region,
    boxes: Vec<Option<Boxed<T>>>,
    marks: Vec<Option<Watermark>>,
}

/// Every open region block of one execution, keyed by the block's runtime
/// identity.
pub struct Regions<T: 'static> {
    open: BTreeMap<u64, Block<T>>,
    opened: u64,
    closed: u64,
}
impl<T: 'static> Default for Regions<T> {
    fn default() -> Self {
        Self {
            open: BTreeMap::new(),
            opened: 0,
            closed: 0,
        }
    }
}
impl<T: 'static> Regions<T> {
    /// Opens the region of a block bounded by `capacity` bytes.
    pub fn open(&mut self, id: u64, capacity: usize) -> Result<(), RegionFault> {
        if self.open.contains_key(&id) {
            return Ok(());
        }
        let region =
            Region::new(config(capacity)).map_err(|e| RegionFault::Config(format!("{e:?}")))?;
        self.open.insert(
            id,
            Block {
                region,
                boxes: vec![],
                marks: vec![],
            },
        );
        self.opened += 1;
        Ok(())
    }
    pub fn is_open(&self, id: u64) -> bool {
        self.open.contains_key(&id)
    }
    fn block(&mut self, id: u64) -> Result<&mut Block<T>, RegionFault> {
        self.open.get_mut(&id).ok_or(RegionFault::Inactive)
    }
    /// `r.box(..)`: reserves [`slot_bytes`]`(size)` zeroed bytes, holding
    /// `value` for the evaluator.
    pub fn alloc(&mut self, id: u64, size: usize, value: Option<T>) -> Result<u64, RegionFault> {
        let b = self.block(id)?;
        let bytes = b
            .region
            .alloc_bytes(slot_bytes(size), SLOT_ALIGN)
            .map_err(|e| RegionFault::Exhausted(format!("{e:?}")))?;
        b.boxes.push(Some(Boxed { bytes, value }));
        Ok(b.boxes.len() as u64 - 1)
    }
    fn boxed(b: &mut Block<T>, handle: u64) -> Result<&mut Boxed<T>, RegionFault> {
        let boxed = usize::try_from(handle)
            .ok()
            .and_then(|i| b.boxes.get_mut(i))
            .ok_or(RegionFault::UnknownHandle)?
            .as_mut()
            .ok_or(RegionFault::Stale)?;
        // The allocator decides whether the bytes are still live.
        b.region.len(&boxed.bytes).map_err(|_| RegionFault::Stale)?;
        Ok(boxed)
    }
    /// The evaluator's `r.get(b)`.
    pub fn get(&mut self, id: u64, handle: u64) -> Result<&T, RegionFault> {
        let b = self.block(id)?;
        Self::boxed(b, handle)?
            .value
            .as_ref()
            .ok_or(RegionFault::UnknownHandle)
    }
    /// The evaluator's `r.set(b, value)`: overwrites the payload in place.
    pub fn set(&mut self, id: u64, handle: u64, value: T) -> Result<(), RegionFault> {
        let b = self.block(id)?;
        let boxed = Self::boxed(b, handle)?;
        boxed.value = Some(value);
        Ok(())
    }
    /// `r.take(b)`: moves the payload out and releases its bytes. The handle
    /// is consumed: a later use of it is stale.
    pub fn take(&mut self, id: u64, handle: u64) -> Result<Option<T>, RegionFault> {
        let b = self.block(id)?;
        Self::boxed(b, handle)?;
        let boxed = b.boxes[handle as usize].take().ok_or(RegionFault::Stale)?;
        b.region
            .release_bytes(boxed.bytes)
            .map_err(|_| RegionFault::Stale)?;
        Ok(boxed.value)
    }
    /// Word `index` of a native payload.
    pub fn load(&mut self, id: u64, handle: u64, index: u64) -> Result<u64, RegionFault> {
        let b = self.block(id)?;
        let bytes = Self::boxed(b, handle)?.bytes;
        let data = b.region.bytes(&bytes).map_err(|_| RegionFault::Stale)?;
        let word = word_range(index, data.len())?;
        let mut out = [0; 8];
        out.copy_from_slice(&data[word]);
        Ok(u64::from_le_bytes(out))
    }
    /// Writes word `index` of a native payload.
    pub fn store(
        &mut self,
        id: u64,
        handle: u64,
        index: u64,
        word: u64,
    ) -> Result<(), RegionFault> {
        let b = self.block(id)?;
        let bytes = Self::boxed(b, handle)?.bytes;
        let data = b.region.bytes_mut(&bytes).map_err(|_| RegionFault::Stale)?;
        let range = word_range(index, data.len())?;
        data[range].copy_from_slice(&word.to_le_bytes());
        Ok(())
    }
    /// `r.checkpoint()`.
    pub fn checkpoint(&mut self, id: u64) -> Result<u64, RegionFault> {
        let b = self.block(id)?;
        let mark = b
            .region
            .checkpoint()
            .map_err(|e| RegionFault::Exhausted(format!("{e:?}")))?;
        b.marks.push(Some(mark));
        Ok(b.marks.len() as u64 - 1)
    }
    fn mark(b: &mut Block<T>, handle: u64) -> Result<Watermark, RegionFault> {
        usize::try_from(handle)
            .ok()
            .and_then(|i| b.marks.get_mut(i))
            .and_then(Option::take)
            .ok_or(RegionFault::UnknownHandle)
    }
    /// `r.rewind(mark)`.
    pub fn rewind(&mut self, id: u64, handle: u64) -> Result<(), RegionFault> {
        let b = self.block(id)?;
        let mark = Self::mark(b, handle)?;
        b.region.rewind(mark).map_err(|e| match e {
            vesper_alloc::RewindError::LinearObligationsLive { count, .. } => {
                RegionFault::LinearLive(count)
            }
            vesper_alloc::RewindError::Handle(_) => RegionFault::Stale,
        })
    }
    /// `r.commit(mark)`.
    pub fn commit(&mut self, id: u64, handle: u64) -> Result<(), RegionFault> {
        let b = self.block(id)?;
        let mark = Self::mark(b, handle)?;
        b.region.commit(mark).map_err(|_| RegionFault::Stale)
    }
    /// `r.used()`: bytes currently allocated.
    pub fn used(&mut self, id: u64) -> Result<u64, RegionFault> {
        Ok(self.block(id)?.region.stats().used_bytes as u64)
    }
    /// Ends a block: drops every payload and frees the region.
    pub fn close(&mut self, id: u64) -> Result<(), RegionFault> {
        let Some(b) = self.open.remove(&id) else {
            return Ok(());
        };
        self.closed += 1;
        let Block { region, .. } = b;
        region.close().map(|_| ()).map_err(|e| match e {
            vesper_alloc::RegionError::LinearObligationsLive { count } => {
                RegionFault::LinearLive(count)
            }
            other => RegionFault::Exhausted(format!("{other:?}")),
        })
    }
    /// Closes every block still open (after an abort unwound past them).
    pub fn close_all(&mut self) -> usize {
        let ids: Vec<u64> = self.open.keys().copied().collect();
        let n = ids.len();
        for id in ids {
            let _ = self.close(id);
        }
        n
    }
    /// `(opened, closed, open now)` counters.
    pub fn counts(&self) -> (u64, u64, usize) {
        (self.opened, self.closed, self.open.len())
    }
}

fn word_range(index: u64, len: usize) -> Result<std::ops::Range<usize>, RegionFault> {
    usize::try_from(index)
        .ok()
        .and_then(|i| i.checked_mul(8))
        .filter(|start| start.checked_add(8).is_some_and(|end| end <= len))
        .map(|start| start..start + 8)
        .ok_or(RegionFault::UnknownHandle)
}
