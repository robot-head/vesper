use crate::region::{RegionId, Watermark};
use std::fmt;

/// Rejected [`crate::RegionConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    ZeroInitialChunk,
    InitialExceedsMaxChunk,
    InitialExceedsReservation,
    MaxAlignNotPowerOfTwo,
    ChunkLayout,
    AllocationLimitTooLarge,
}

/// Failure to obtain region memory. A failed request leaves the region and
/// every existing handle unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    /// The requested alignment is zero or not a power of two.
    InvalidAlignment { align: usize },
    /// The requested alignment exceeds `RegionConfig::max_align`.
    AlignmentTooLarge { align: usize, max: usize },
    /// Size arithmetic overflowed or exceeded `isize::MAX`.
    SizeOverflow,
    /// The region's bounded reservation cannot hold the request.
    CapacityExceeded {
        requested: usize,
        reserved: usize,
        limit: usize,
    },
    /// The backing refused a chunk.
    OutOfMemory { chunk_bytes: usize },
    /// The tracked-allocation table is full.
    TooManyAllocations { limit: usize },
    /// The 64-bit allocation serial space is exhausted.
    ExhaustedSerials,
}

/// A handle or watermark that does not name a live allocation of this region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    /// The handle was minted by a different region.
    CrossRegion { expected: RegionId, found: RegionId },
    /// The allocation was rewound, reset, released or relocated.
    Stale,
    /// The value was already taken out of its box.
    Moved,
    /// Two mutable views were requested for the same allocation.
    Aliased,
}

/// Failure of a region operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionError {
    Config(ConfigError),
    Alloc(AllocError),
    Handle(HandleError),
    /// Linear payloads are still live where the operation would discard them.
    LinearObligationsLive {
        count: usize,
    },
    TooManyCheckpoints {
        limit: usize,
    },
    /// The allocation predates the newest outstanding watermark and cannot
    /// grow until that watermark is rewound or committed.
    ProtectedByWatermark,
}

/// Allocation failure for a typed value; ownership of the value is returned.
pub struct ValueAllocError<T> {
    pub value: T,
    pub error: AllocError,
}

/// Rewind failure. When live linear payloads block the rewind, the watermark
/// is returned so that the caller can consume them and retry.
#[derive(Debug)]
pub enum RewindError {
    Handle(HandleError),
    LinearObligationsLive { mark: Watermark, count: usize },
}

impl<T> fmt::Debug for ValueAllocError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValueAllocError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

macro_rules! display_debug {
    ($($ty:ty),*) => {$(
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{self:?}")
            }
        }
        impl std::error::Error for $ty {}
    )*};
}
display_debug!(
    ConfigError,
    AllocError,
    HandleError,
    RegionError,
    RewindError
);

impl<T> fmt::Display for ValueAllocError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.error)
    }
}
impl<T> std::error::Error for ValueAllocError<T> {}

impl From<ConfigError> for RegionError {
    fn from(e: ConfigError) -> Self {
        Self::Config(e)
    }
}
impl From<AllocError> for RegionError {
    fn from(e: AllocError) -> Self {
        Self::Alloc(e)
    }
}
impl From<HandleError> for RegionError {
    fn from(e: HandleError) -> Self {
        Self::Handle(e)
    }
}
