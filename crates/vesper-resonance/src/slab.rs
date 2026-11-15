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

#[derive(Clone, Copy, Debug)]
struct DelaySlotMeta {
    offset: usize,
    delay: usize,
    cap: usize,
    stride: usize,
    head: usize,
}

/// One 64-byte-aligned allocation packing all block delay lines of a
/// [`crate::pdc::CompiledGraph`].
///
/// Each line `i` starts at a 64-byte cache-line boundary with capacity
/// `delay_i + block` samples and stride `ceil((delay_i + block) / 16) * 16`
/// samples, so consecutive lines never share a cache line and steady-state
/// block reads/writes allocate nothing.
#[derive(Debug)]
pub struct DelaySlab {
    data: Vec<f32>,
    base: usize,
    slots: Vec<DelaySlotMeta>,
    block: usize,
    total_stride: usize,
}

impl DelaySlab {
    /// Allocate one zeroed slab holding a delay line for each delay length in
    /// `delays`, each sized for blocks of `block` samples. Allocates once.
    pub fn new(delays: &[u32], block: usize) -> Result<Self, SlabError> {
        let too_large = SlabError::TooLarge {
            buffers: delays.len(),
            block,
        };
        if block == 0 {
            return Err(too_large);
        }
        if delays.is_empty() {
            return Ok(DelaySlab {
                data: Vec::new(),
                base: 0,
                slots: Vec::new(),
                block,
                total_stride: 0,
            });
        }
        let mut slots = Vec::with_capacity(delays.len());
        let mut total_stride = 0usize;
        for &d in delays {
            let delay = d as usize;
            let cap = delay.checked_add(block).ok_or(too_large)?;
            let stride =
                cap.checked_add(TILE_SAMPLES - 1).ok_or(too_large)? / TILE_SAMPLES * TILE_SAMPLES;
            slots.push(DelaySlotMeta {
                offset: total_stride,
                delay,
                cap,
                stride,
                head: 0,
            });
            total_stride = total_stride.checked_add(stride).ok_or(too_large)?;
        }
        let len = total_stride
            .checked_add(TILE_SAMPLES - 1)
            .ok_or(too_large)?;
        len.checked_mul(std::mem::size_of::<f32>())
            .filter(|&b| b <= isize::MAX as usize)
            .ok_or(too_large)?;
        let data = vec![0.0f32; len];
        let base = data.as_ptr().align_offset(TILE_BYTES);
        if base >= TILE_SAMPLES {
            return Err(SlabError::Unalignable);
        }
        Ok(DelaySlab {
            data,
            base,
            slots,
            block,
            total_stride,
        })
    }

    /// Number of packed delay lines.
    pub fn lines(&self) -> usize {
        self.slots.len()
    }

    /// Block length each line is sized for.
    pub fn block(&self) -> usize {
        self.block
    }

    /// Total bytes of the packed delay-line working set (`total_stride * 4`).
    pub fn working_set_bytes(&self) -> usize {
        self.total_stride * std::mem::size_of::<f32>()
    }

    /// Whether the packed delay working set fits in `bytes`.
    pub fn fits(&self, bytes: usize) -> bool {
        self.working_set_bytes() <= bytes
    }

    /// Delay length (in samples) of `line`.
    pub fn line_delay(&self, line: usize) -> Option<usize> {
        self.slots.get(line).map(|s| s.delay)
    }

    /// Padded sample stride of `line` (always a multiple of [`TILE_SAMPLES`]).
    pub fn line_stride(&self, line: usize) -> Option<usize> {
        self.slots.get(line).map(|s| s.stride)
    }

    /// Byte addresses of every delay line start, for layout verification.
    pub fn line_addresses(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots
            .iter()
            .map(|s| self.data[self.base + s.offset..].as_ptr() as usize)
    }

    /// Write `input` samples into `line` at its current head.
    pub fn write(&mut self, line: usize, input: &[f32]) {
        let Some(&slot) = self.slots.get(line) else {
            return;
        };
        let ring = &mut self.data[self.base + slot.offset..self.base + slot.offset + slot.cap];
        if input.len() <= slot.cap {
            let first = input.len().min(slot.cap - slot.head);
            ring[slot.head..slot.head + first].copy_from_slice(&input[..first]);
            if first < input.len() {
                ring[..input.len() - first].copy_from_slice(&input[first..]);
            }
        } else {
            for (k, &x) in input.iter().enumerate() {
                ring[(slot.head + k) % slot.cap] = x;
            }
        }
    }

    /// Read delayed samples from `line` into `out`.
    pub fn read(&self, line: usize, out: &mut [f32]) {
        let Some(&slot) = self.slots.get(line) else {
            return;
        };
        let ring = &self.data[self.base + slot.offset..self.base + slot.offset + slot.cap];
        let start = if slot.head >= slot.delay {
            slot.head - slot.delay
        } else if slot.delay <= slot.cap {
            slot.head + slot.cap - slot.delay
        } else {
            (slot.head + slot.cap - (slot.delay % slot.cap)) % slot.cap
        };
        let n = out.len();
        if n <= slot.cap {
            let first = n.min(slot.cap - start);
            out[..first].copy_from_slice(&ring[start..start + first]);
            if first < n {
                out[first..].copy_from_slice(&ring[..n - first]);
            }
        } else {
            for (k, y) in out.iter_mut().enumerate() {
                *y = ring[(start + k) % slot.cap];
            }
        }
    }

    /// Advance every delay line's ring head by `block` samples.
    pub fn advance_all(&mut self, block: usize) {
        for slot in &mut self.slots {
            let next = slot.head + block;
            slot.head = if next < slot.cap {
                next
            } else {
                next % slot.cap
            };
        }
    }
}

impl Clone for DelaySlab {
    /// Allocate a fresh 64-byte-aligned backing buffer and copy each line's state.
    fn clone(&self) -> Self {
        if self.slots.is_empty() {
            return DelaySlab {
                data: Vec::new(),
                base: 0,
                slots: Vec::new(),
                block: self.block,
                total_stride: 0,
            };
        }
        let len = self.total_stride + TILE_SAMPLES - 1;
        let mut data = vec![0.0f32; len];
        let base = data.as_ptr().align_offset(TILE_BYTES);
        assert!(
            base < TILE_SAMPLES,
            "cloned delay slab cannot be aligned to 64 bytes"
        );
        for slot in &self.slots {
            let src = self.base + slot.offset;
            let dst = base + slot.offset;
            data[dst..dst + slot.cap].copy_from_slice(&self.data[src..src + slot.cap]);
        }
        DelaySlab {
            data,
            base,
            slots: self.slots.clone(),
            block: self.block,
            total_stride: self.total_stride,
        }
    }
}
