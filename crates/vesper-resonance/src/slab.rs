//! TiledSeq slab packing for the delay-compensated graph's block buffers
//! (design §32.1 item 4, §15.5).
//!
//! Every block buffer of a [`crate::pdc::CompiledGraph`] lives in one
//! contiguous allocation. Buffer `i` starts at a 64-byte boundary (one cache
//! line, 16 `f32` samples, is one tile) and occupies
//! `stride = ceil(block / 16) * 16` samples, so no two buffers share a cache
//! line and the whole working set is `buffers * stride * 4` bytes. A 64-input
//! mixer at 64 samples per block is 65 buffers = 16,640 bytes, inside a
//! 32 KiB L1 data cache (`slab_fits_l1` in the tests).
//!
//! The slab is allocated once and never grows. Reading one buffer while
//! writing another goes through [`TiledSlab::split_out`], which hands out the
//! output buffer mutably and every other buffer read-only, with no copying
//! and no unsafe code.

/// Samples per tile (one 64-byte cache line of `f32`).
pub const TILE_SAMPLES: usize = 16;
/// Tile size in bytes.
pub const TILE_BYTES: usize = 64;

/// Slab construction errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlabError {
    /// `buffers * stride` overflows `usize`.
    TooLarge {
        /// Buffers requested.
        buffers: usize,
        /// Block length requested.
        block: usize,
    },
    /// The allocator returned memory that cannot be aligned to 64 bytes.
    Unalignable,
}

impl std::fmt::Display for SlabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlabError::TooLarge { buffers, block } => {
                write!(f, "{buffers} buffers of {block} samples overflow the slab")
            }
            SlabError::Unalignable => write!(f, "slab cannot be aligned to 64 bytes"),
        }
    }
}

impl std::error::Error for SlabError {}

/// One allocation holding `buffers` cache-line-aligned blocks of `block`
/// samples.
#[derive(Debug)]
pub struct TiledSlab {
    data: Vec<f32>,
    base: usize,
    stride: usize,
    buffers: usize,
    block: usize,
}

impl TiledSlab {
    /// A zeroed slab. Allocates once.
    pub fn new(buffers: usize, block: usize) -> Result<Self, SlabError> {
        let too_large = SlabError::TooLarge { buffers, block };
        let stride =
            block.checked_add(TILE_SAMPLES - 1).ok_or(too_large)? / TILE_SAMPLES * TILE_SAMPLES;
        let len = buffers
            .checked_mul(stride)
            .and_then(|n| n.checked_add(TILE_SAMPLES - 1))
            .ok_or(too_large)?;
        len.checked_mul(std::mem::size_of::<f32>())
            .filter(|&b| b <= isize::MAX as usize)
            .ok_or(too_large)?;
        let data = vec![0.0f32; len];
        // `align_offset` counts elements; at most 15 for a 4-byte-aligned
        // pointer and a 64-byte target.
        let base = data.as_ptr().align_offset(TILE_BYTES);
        if base >= TILE_SAMPLES {
            return Err(SlabError::Unalignable);
        }
        Ok(TiledSlab {
            data,
            base,
            stride,
            buffers,
            block,
        })
    }

    /// Number of buffers.
    pub fn buffers(&self) -> usize {
        self.buffers
    }

    /// Samples per buffer.
    pub fn block(&self) -> usize {
        self.block
    }

    /// Samples between consecutive buffer starts (a whole number of tiles).
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Bytes of the packed working set (`buffers * stride * 4`).
    pub fn working_set_bytes(&self) -> usize {
        self.buffers * self.stride * std::mem::size_of::<f32>()
    }

    /// Whether the working set fits a data cache of `l1_bytes`.
    pub fn fits(&self, l1_bytes: usize) -> bool {
        self.working_set_bytes() <= l1_bytes
    }

    fn start(&self, i: usize) -> usize {
        self.base + i * self.stride
    }

    /// Buffer `i`, or `None` when out of range.
    pub fn buffer(&self, i: usize) -> Option<&[f32]> {
        (i < self.buffers).then(|| &self.data[self.start(i)..self.start(i) + self.block])
    }

    /// Buffer `i` mutably, or `None` when out of range.
    pub fn buffer_mut(&mut self, i: usize) -> Option<&mut [f32]> {
        if i < self.buffers {
            let s = self.start(i);
            Some(&mut self.data[s..s + self.block])
        } else {
            None
        }
    }

    /// Buffer `out` mutably together with a read-only view of every other
    /// buffer; `None` when `out` is out of range.
    pub fn split_out(&mut self, out: usize) -> Option<(SlabRead<'_>, &mut [f32])> {
        if out >= self.buffers {
            return None;
        }
        let s = self.start(out);
        let (geom_base, stride, block, buffers) =
            (self.base, self.stride, self.block, self.buffers);
        let (before, rest) = self.data.split_at_mut(s);
        let (mid, after) = rest.split_at_mut(block);
        Some((
            SlabRead {
                before,
                after,
                out,
                base: geom_base,
                stride,
                block,
                buffers,
            },
            mid,
        ))
    }

    /// Addresses of every buffer start, for layout tests.
    pub fn buffer_addresses(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.buffers).map(|i| self.data[self.start(i)..].as_ptr() as usize)
    }
}

impl Clone for TiledSlab {
    /// A new allocation re-aligned to 64 bytes (a plain `Vec` clone could
    /// land at a different offset modulo 64), with every buffer copied.
    fn clone(&self) -> Self {
        let mut copy = TiledSlab::new(self.buffers, self.block)
            .expect("an existing slab's geometry is valid and alignable");
        for i in 0..self.buffers {
            let (src, dst) = (self.start(i), copy.start(i));
            copy.data[dst..dst + self.block].copy_from_slice(&self.data[src..src + self.block]);
        }
        copy
    }
}

/// Read-only view of every slab buffer except the one handed out mutably.
#[derive(Debug)]
pub struct SlabRead<'a> {
    before: &'a [f32],
    after: &'a [f32],
    out: usize,
    base: usize,
    stride: usize,
    block: usize,
    buffers: usize,
}

impl SlabRead<'_> {
    /// Buffer `i`; `None` for the excluded output buffer or out of range.
    pub fn get(&self, i: usize) -> Option<&[f32]> {
        if i >= self.buffers || i == self.out {
            return None;
        }
        let s = self.base + i * self.stride;
        if i < self.out {
            self.before.get(s..s + self.block)
        } else {
            let a = s - (self.base + self.out * self.stride + self.block);
            self.after.get(a..a + self.block)
        }
    }
}
