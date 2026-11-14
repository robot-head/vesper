//! Region blocks backed by [`vesper_alloc::Region`], shared by the evaluator
//! (payload `Value`) and the native runtime calls (payload `i64` words).
//!
//! Source handles are plain integers: a box handle is an index into the
//! block's box table and a watermark handle an index into its mark table.
//! The allocator, not the table, decides validity: a rewound, reset or moved
//! payload makes its [`vesper_alloc::RegionBox`] stale, so every access is
//! checked by the region's identity and serial and fails with a typed error.
use std::collections::BTreeMap;
use std::fmt;
use vesper_alloc::{Region, RegionBox, RegionConfig, Watermark};

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

struct Block<T: 'static> {
    region: Region,
    boxes: Vec<Option<RegionBox<T>>>,
    marks: Vec<Option<Watermark>>,
}

/// Every open region block of one execution, keyed by the block's runtime
/// capability identity.
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
        let region = Region::new(RegionConfig::bounded(capacity))
            .map_err(|e| RegionFault::Config(format!("{e:?}")))?;
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
    /// `r.box(value)`: moves `value` into region memory.
    pub fn alloc(&mut self, id: u64, value: T, linear: bool) -> Result<u64, RegionFault> {
        let b = self.block(id)?;
        let boxed = if linear {
            b.region.alloc_linear(value)
        } else {
            b.region.alloc_value(value)
        }
        .map_err(|e| RegionFault::Exhausted(format!("{:?}", e.error)))?;
        b.boxes.push(Some(boxed));
        Ok(b.boxes.len() as u64 - 1)
    }
    fn boxed(b: &Block<T>, handle: u64) -> Result<&RegionBox<T>, RegionFault> {
        usize::try_from(handle)
            .ok()
            .and_then(|i| b.boxes.get(i))
            .ok_or(RegionFault::UnknownHandle)?
            .as_ref()
            .ok_or(RegionFault::Stale)
    }
    /// `r.get(b)`.
    pub fn get(&mut self, id: u64, handle: u64) -> Result<&T, RegionFault> {
        let b = self.block(id)?;
        let boxed = Self::boxed(b, handle)?;
        b.region.get(boxed).map_err(|_| RegionFault::Stale)
    }
    /// `r.set(b, value)`: overwrites the payload in place.
    pub fn set(&mut self, id: u64, handle: u64, value: T) -> Result<(), RegionFault> {
        let Block { region, boxes, .. } = self.block(id)?;
        let boxed = usize::try_from(handle)
            .ok()
            .and_then(|i| boxes.get(i))
            .ok_or(RegionFault::UnknownHandle)?
            .as_ref()
            .ok_or(RegionFault::Stale)?;
        *region.get_mut(boxed).map_err(|_| RegionFault::Stale)? = value;
        Ok(())
    }
    /// `r.take(b)`: moves the payload out, discharging a linear obligation.
    /// The handle is consumed: a later use of it is stale.
    pub fn take(&mut self, id: u64, handle: u64) -> Result<T, RegionFault> {
        let b = self.block(id)?;
        let slot = usize::try_from(handle)
            .ok()
            .and_then(|i| b.boxes.get_mut(i))
            .ok_or(RegionFault::UnknownHandle)?;
        let boxed = slot.take().ok_or(RegionFault::Stale)?;
        b.region.take(boxed).map_err(|_| RegionFault::Stale)
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
    /// Ends a block: finalizes every payload and frees the region. A live
    /// linear payload is a protocol violation the checker rules out.
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
