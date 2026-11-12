//! Typed errors. Every failed consuming operation hands its owner back.

use std::fmt;

/// Why a ring operation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingError {
    InvalidSize,
    /// `write`/`read` outside the caller's reserved or peeked window.
    IndexOutsideWindow {
        idx: u32,
    },
    /// `submit`/`release`/cancel of more entries than are outstanding.
    CountExceedsWindow {
        requested: u32,
        outstanding: u32,
    },
}

/// Why the NIC handed us a descriptor we refuse to turn into a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescError {
    /// The address lies outside the UMEM; it names no chunk.
    OutOfUmem {
        addr: u64,
    },
    /// Data starts inside the configured headroom.
    HeadroomOverlap {
        offset: u32,
        headroom: u32,
    },
    /// Data runs past the end of its chunk.
    OverLength {
        offset: u32,
        len: u32,
        frame_size: u32,
    },
    ZeroLength,
    /// Multi-buffer (`XDP_PKT_CONTD`) and other option bits are not supported.
    UnsupportedOptions {
        options: u32,
    },
    /// The descriptor names a chunk the kernel side does not own.
    NotKernelOwned {
        chunk: u32,
    },
}

/// A descriptor rejected by `harvest_rx`. When the chunk was identifiable and
/// owned by the kernel side it was returned to the fill ring, so it is not lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxRejected {
    pub reason: DescError,
    pub chunk: Option<u32>,
    pub recycled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UmemError {
    InvalidConfig(&'static str),
    /// The owner belongs to a different UMEM.
    ForeignOwner,
    /// The driver's location table disagrees with the owner (a bug or a
    /// forged owner); nothing was changed.
    WrongLocation {
        chunk: u32,
    },
    TxRingFull,
    NoFreeFrame,
    LengthOutOfFrame {
        offset: u32,
        len: u32,
        frame_size: u32,
    },
    HeadroomOverlap {
        offset: u32,
        headroom: u32,
    },
    /// Packet numbers are `< 2^62` (RFC 9000 §17.1).
    PacketNumberOutOfRange {
        pn: u64,
    },
    Ring(RingError),
    /// An OS call failed (`errno`).
    Os {
        op: &'static str,
        errno: i32,
    },
}

impl From<RingError> for UmemError {
    fn from(e: RingError) -> Self {
        Self::Ring(e)
    }
}

impl fmt::Display for UmemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for UmemError {}

/// A refused consuming call: the owner comes back with the reason.
#[must_use = "a refused owner must still be consumed"]
#[derive(Debug)]
pub struct Refused<T> {
    pub owner: T,
    pub error: UmemError,
}

impl<T> Refused<T> {
    pub(crate) fn new(owner: T, error: UmemError) -> Self {
        Self { owner, error }
    }
    pub fn into_owner(self) -> T {
        self.owner
    }
}
