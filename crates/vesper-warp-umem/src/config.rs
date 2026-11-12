//! UMEM geometry and ring sizes.

use crate::error::UmemError;

/// Smallest chunk the Linux aligned-chunk mode accepts.
pub const MIN_FRAME_SIZE: u32 = 2048;
/// Largest chunk this crate accepts. `UmemFrame` stores the data offset and
/// packet length as `u16`, so a frame must stay below 64 KiB.
pub const MAX_FRAME_SIZE: u32 = 32 * 1024;
/// Every frame keeps at least this many payload bytes after the headroom.
pub const MIN_FRAME_PAYLOAD: u32 = 64;
/// Upper bound for any ring, to keep the model's metadata bounded.
pub const MAX_RING_SIZE: u32 = 1 << 20;
/// Upper bound for the number of frames.
pub const MAX_FRAMES: u32 = 1 << 20;

/// Geometry of one UMEM and its four rings.
///
/// The UMEM is `frame_count` chunks of `frame_size` bytes each (aligned-chunk
/// mode). Received data starts at `headroom` within a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UmemConfig {
    pub frame_count: u32,
    /// Power of two, `MIN_FRAME_SIZE..=MAX_FRAME_SIZE`. Default 4 KiB.
    pub frame_size: u32,
    /// Bytes reserved at the start of each chunk.
    pub headroom: u32,
    /// Must be a power of two of at least `frame_count`, so that returning a
    /// frame to the fill ring can never fail for lack of space.
    pub fill_size: u32,
    pub rx_size: u32,
    pub tx_size: u32,
    pub completion_size: u32,
}

impl Default for UmemConfig {
    fn default() -> Self {
        Self::with_frames(4096)
    }
}

impl UmemConfig {
    /// 4 KiB frames, no headroom, fill ring sized to hold every frame and
    /// 2048-entry rx/tx/completion rings (the libxdp defaults).
    pub fn with_frames(frame_count: u32) -> Self {
        let fill_size = frame_count.max(1).checked_next_power_of_two().unwrap_or(0);
        Self {
            frame_count,
            frame_size: 4096,
            headroom: 0,
            fill_size,
            rx_size: 2048,
            tx_size: 2048,
            completion_size: 2048,
        }
    }

    pub fn validate(&self) -> Result<(), UmemError> {
        let bad = |why| Err(UmemError::InvalidConfig(why));
        if self.frame_count == 0 || self.frame_count > MAX_FRAMES {
            return bad("frame_count must be in 1..=2^20");
        }
        if !self.frame_size.is_power_of_two()
            || !(MIN_FRAME_SIZE..=MAX_FRAME_SIZE).contains(&self.frame_size)
        {
            return bad("frame_size must be a power of two in 2048..=32768");
        }
        match self.frame_size.checked_sub(self.headroom) {
            Some(payload) if payload >= MIN_FRAME_PAYLOAD => {}
            _ => return bad("headroom leaves fewer than 64 payload bytes"),
        }
        for size in [
            self.fill_size,
            self.rx_size,
            self.tx_size,
            self.completion_size,
        ] {
            if !size.is_power_of_two() || size > MAX_RING_SIZE {
                return bad("ring sizes must be powers of two up to 2^20");
            }
        }
        if self.fill_size < self.frame_count {
            return bad("fill ring must be able to hold every frame");
        }
        self.umem_len()?;
        Ok(())
    }

    /// Total UMEM bytes, checked.
    pub fn umem_len(&self) -> Result<usize, UmemError> {
        u64::from(self.frame_count)
            .checked_mul(u64::from(self.frame_size))
            .and_then(|len| usize::try_from(len).ok())
            .ok_or(UmemError::InvalidConfig("UMEM size overflows"))
    }

    pub(crate) fn frame_shift(&self) -> u32 {
        self.frame_size.trailing_zeros()
    }

    /// Byte offset of chunk `idx` within the UMEM.
    pub fn chunk_base(&self, idx: u32) -> u64 {
        u64::from(idx) << self.frame_shift()
    }
}
