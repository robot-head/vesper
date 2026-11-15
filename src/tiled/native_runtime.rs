//! Thread-local native JIT runtime for `TiledSeq[T]` (§12.5).
//!
//! Each `TiledSeq` allocates 64-byte cache-line-aligned AoSoA buffers for its
//! `hot` tile columns and a separate word-addressed slab for its `cold` fields.
//! `vesper_tiled_tile_col_ptr` exposes the aligned tile column address directly
//! to Cranelift `v128` vector loads without gather instructions.
use crate::tiled::CACHE_LINE_BYTES;
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::cell::RefCell;
use std::collections::BTreeMap;

struct AlignedHotBuffer {
    ptr: *mut u8,
    bytes: usize,
}

impl AlignedHotBuffer {
    fn empty() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            bytes: 0,
        }
    }

    fn alloc_zeroed(bytes: usize) -> Result<Self, String> {
        if bytes == 0 {
            return Ok(Self::empty());
        }
        let layout = Layout::from_size_align(bytes, CACHE_LINE_BYTES)
            .map_err(|e| format!("invalid hot tile buffer layout: {e}"))?;
        // SAFETY: `bytes > 0` and `CACHE_LINE_BYTES` (64) is a power of two.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            return Err("out of memory allocating 64-byte aligned AoSoA tile buffer".into());
        }
        Ok(Self { ptr, bytes })
    }

    fn resize_zeroed(&mut self, new_bytes: usize) -> Result<(), String> {
        if new_bytes <= self.bytes {
            return Ok(());
        }
        let next = Self::alloc_zeroed(new_bytes)?;
        if !self.ptr.is_null() && self.bytes > 0 {
            // SAFETY: both buffers are non-null, non-overlapping, and valid for `self.bytes`.
            unsafe {
                std::ptr::copy_nonoverlapping(self.ptr, next.ptr, self.bytes);
            }
        }
        *self = next;
        Ok(())
    }

    fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }
}

impl Drop for AlignedHotBuffer {
    fn drop(&mut self) {
        if !self.ptr.is_null()
            && self.bytes > 0
            && let Ok(layout) = Layout::from_size_align(self.bytes, CACHE_LINE_BYTES)
        {
            // SAFETY: `self.ptr` was allocated with `layout`.
            unsafe { dealloc(self.ptr, layout) };
        }
    }
}

struct NativeSeq {
    tile_width: usize,
    tile_stride: usize,
    cold_words_per_row: usize,
    len: usize,
    tile_cap: usize,
    hot: AlignedHotBuffer,
    cold: Vec<u64>,
    region_id: Option<u64>,
    charged_bytes: usize,
    row_charge_bytes: usize,
}

impl NativeSeq {
    fn ensure_region_active(&self) -> Result<(), String> {
        if let Some(id) = self.region_id
            && !crate::regions::native_runtime::is_open(id)
        {
            return Err("tiled sequence region is no longer active".into());
        }
        Ok(())
    }

    fn charge_region(&mut self, rows: usize) -> Result<(), String> {
        let Some(id) = self.region_id else {
            return Ok(());
        };
        if !crate::regions::native_runtime::is_open(id) {
            return Err("tiled sequence region is no longer active".into());
        }
        let target = rows.saturating_mul(self.row_charge_bytes);
        if target > self.charged_bytes {
            crate::regions::native_runtime::alloc_bytes(id, target - self.charged_bytes)
                .map_err(|e| e.to_string())?;
            self.charged_bytes = target;
        }
        Ok(())
    }

    fn ensure_tiles(&mut self, needed_tiles: usize) -> Result<(), String> {
        if needed_tiles <= self.tile_cap {
            return Ok(());
        }
        let new_tiles = self.tile_cap.max(1).saturating_mul(2).max(needed_tiles);
        let new_hot_bytes = new_tiles
            .checked_mul(self.tile_stride)
            .ok_or_else(|| "AoSoA tile buffer size overflow".to_string())?;
        self.hot.resize_zeroed(new_hot_bytes)?;
        let new_cold_words = new_tiles
            .checked_mul(self.tile_width)
            .and_then(|r| r.checked_mul(self.cold_words_per_row))
            .ok_or_else(|| "AoSoA cold slab size overflow".to_string())?;
        if new_cold_words > self.cold.len() {
            self.cold.resize(new_cold_words, 0);
        }
        self.tile_cap = new_tiles;
        Ok(())
    }

    fn hot_offset(
        &self,
        row: usize,
        col_tile_offset: usize,
        elem_bytes: usize,
    ) -> Result<usize, String> {
        self.ensure_region_active()?;
        if row >= self.len {
            return Err(format!(
                "TiledSeq index {row} out of bounds (len {})",
                self.len
            ));
        }
        let tile = row / self.tile_width;
        let lane = row % self.tile_width;
        tile.checked_mul(self.tile_stride)
            .and_then(|t| t.checked_add(col_tile_offset))
            .and_then(|c| c.checked_add(lane * elem_bytes))
            .filter(|off| off + elem_bytes <= self.hot.bytes)
            .ok_or_else(|| "AoSoA hot column offset out of bounds".to_string())
    }

    fn cold_index(&self, row: usize, word_offset: usize) -> Result<usize, String> {
        self.ensure_region_active()?;
        if row >= self.len {
            return Err(format!(
                "TiledSeq index {row} out of bounds (len {})",
                self.len
            ));
        }
        row.checked_mul(self.cold_words_per_row)
            .and_then(|b| b.checked_add(word_offset))
            .filter(|idx| *idx < self.cold.len())
            .ok_or_else(|| "AoSoA cold slab offset out of bounds".to_string())
    }
}

#[derive(Default)]
struct NativeTiled {
    seqs: BTreeMap<u64, NativeSeq>,
    next: u64,
    fault: Option<String>,
}

thread_local! {
    static TILED: RefCell<NativeTiled> = RefCell::new(NativeTiled::default());
}

fn with<R: Default>(f: impl FnOnce(&mut NativeTiled) -> Result<R, String>) -> R {
    TILED.with(|t| {
        let mut t = t.borrow_mut();
        if t.fault.is_some() {
            return R::default();
        }
        match f(&mut t) {
            Ok(v) => v,
            Err(e) => {
                t.fault = Some(e);
                R::default()
            }
        }
    })
}

/// JIT-imported symbols for `TiledSeq[T]`.
pub fn symbols() -> [(&'static str, *const u8); 9] {
    [
        ("vesper_tiled_new", vesper_tiled_new as *const u8),
        ("vesper_tiled_len", vesper_tiled_len as *const u8),
        (
            "vesper_tiled_tile_count",
            vesper_tiled_tile_count as *const u8,
        ),
        (
            "vesper_tiled_push_begin",
            vesper_tiled_push_begin as *const u8,
        ),
        ("vesper_tiled_hot_load", vesper_tiled_hot_load as *const u8),
        (
            "vesper_tiled_hot_store",
            vesper_tiled_hot_store as *const u8,
        ),
        (
            "vesper_tiled_cold_load",
            vesper_tiled_cold_load as *const u8,
        ),
        (
            "vesper_tiled_cold_store",
            vesper_tiled_cold_store as *const u8,
        ),
        (
            "vesper_tiled_tile_col_ptr",
            vesper_tiled_tile_col_ptr as *const u8,
        ),
    ]
}

pub extern "C" fn vesper_tiled_new(
    tile_width: u64,
    tile_stride: u64,
    cold_words_per_row: u64,
    region_id: u64,
    cap: u64,
) -> u64 {
    with(|t| {
        let w = usize::try_from(tile_width).map_err(|_| "tile width overflow")?;
        let stride = usize::try_from(tile_stride).map_err(|_| "tile stride overflow")?;
        let cold_w = usize::try_from(cold_words_per_row).map_err(|_| "cold width overflow")?;
        let cap = usize::try_from(cap).map_err(|_| "capacity overflow")?;
        if w == 0 {
            return Err("tile width must be non-zero".into());
        }
        let region_id = if region_id == 0 {
            None
        } else {
            Some(region_id)
        };
        let hot_per_row = stride.div_ceil(w);
        let row_charge_bytes = (hot_per_row + cold_w * 8).max(8);
        let mut seq = NativeSeq {
            tile_width: w,
            tile_stride: stride,
            cold_words_per_row: cold_w,
            len: 0,
            tile_cap: 0,
            hot: AlignedHotBuffer::empty(),
            cold: Vec::new(),
            region_id,
            charged_bytes: 0,
            row_charge_bytes,
        };
        if cap > 0 {
            seq.charge_region(cap)?;
            let tiles = cap.div_ceil(w);
            seq.ensure_tiles(tiles)?;
        }
        t.next = t.next.checked_add(1).ok_or("tiled sequence id overflow")?;
        let id = t.next;
        t.seqs.insert(id, seq);
        Ok(id)
    })
}

pub extern "C" fn vesper_tiled_len(seq_id: u64) -> u64 {
    with(|t| {
        let seq = t.seqs.get(&seq_id).ok_or("unknown TiledSeq handle")?;
        seq.ensure_region_active()?;
        Ok(seq.len as u64)
    })
}

pub extern "C" fn vesper_tiled_tile_count(seq_id: u64) -> u64 {
    with(|t| {
        let seq = t.seqs.get(&seq_id).ok_or("unknown TiledSeq handle")?;
        seq.ensure_region_active()?;
        Ok(seq.len.div_ceil(seq.tile_width) as u64)
    })
}

pub extern "C" fn vesper_tiled_push_begin(seq_id: u64) -> u64 {
    with(|t| {
        let seq = t.seqs.get_mut(&seq_id).ok_or("unknown TiledSeq handle")?;
        let next_len = seq.len + 1;
        seq.charge_region(next_len)?;
        let needed_tiles = next_len.div_ceil(seq.tile_width);
        seq.ensure_tiles(needed_tiles)?;
        let row = seq.len;
        seq.len = next_len;
        Ok(row as u64)
    })
}

pub extern "C" fn vesper_tiled_hot_load(
    seq_id: u64,
    row: u64,
    col_tile_offset: u64,
    elem_bytes: u64,
) -> u64 {
    with(|t| {
        let seq = t.seqs.get(&seq_id).ok_or("unknown TiledSeq handle")?;
        let row = usize::try_from(row).map_err(|_| "row index overflow")?;
        let col_off = usize::try_from(col_tile_offset).map_err(|_| "column offset overflow")?;
        let eb = usize::try_from(elem_bytes).map_err(|_| "elem size overflow")?;
        let off = seq.hot_offset(row, col_off, eb)?;
        // SAFETY: `hot_offset` checks `off + eb <= seq.hot.bytes`.
        unsafe {
            let p = seq.hot.as_ptr().add(off);
            Ok(match eb {
                1 => u64::from(*p),
                4 => u64::from((p as *const u32).read_unaligned()),
                8 => (p as *const u64).read_unaligned(),
                _ => return Err(format!("unsupported hot column width {eb}")),
            })
        }
    })
}

pub extern "C" fn vesper_tiled_hot_store(
    seq_id: u64,
    row: u64,
    col_tile_offset: u64,
    elem_bytes: u64,
    val: u64,
) -> u64 {
    with(|t| {
        let seq = t.seqs.get_mut(&seq_id).ok_or("unknown TiledSeq handle")?;
        let row = usize::try_from(row).map_err(|_| "row index overflow")?;
        let col_off = usize::try_from(col_tile_offset).map_err(|_| "column offset overflow")?;
        let eb = usize::try_from(elem_bytes).map_err(|_| "elem size overflow")?;
        let off = seq.hot_offset(row, col_off, eb)?;
        // SAFETY: `hot_offset` checks `off + eb <= seq.hot.bytes`.
        unsafe {
            let p = seq.hot.as_mut_ptr().add(off);
            match eb {
                1 => *p = val as u8,
                4 => (p as *mut u32).write_unaligned(val as u32),
                8 => (p as *mut u64).write_unaligned(val),
                _ => return Err(format!("unsupported hot column width {eb}")),
            }
        }
        Ok(0)
    })
}

pub extern "C" fn vesper_tiled_cold_load(seq_id: u64, row: u64, word_offset: u64) -> u64 {
    with(|t| {
        let seq = t.seqs.get(&seq_id).ok_or("unknown TiledSeq handle")?;
        let row = usize::try_from(row).map_err(|_| "row index overflow")?;
        let woff = usize::try_from(word_offset).map_err(|_| "cold offset overflow")?;
        let idx = seq.cold_index(row, woff)?;
        Ok(seq.cold[idx])
    })
}

pub extern "C" fn vesper_tiled_cold_store(
    seq_id: u64,
    row: u64,
    word_offset: u64,
    word: u64,
) -> u64 {
    with(|t| {
        let seq = t.seqs.get_mut(&seq_id).ok_or("unknown TiledSeq handle")?;
        let row = usize::try_from(row).map_err(|_| "row index overflow")?;
        let woff = usize::try_from(word_offset).map_err(|_| "cold offset overflow")?;
        let idx = seq.cold_index(row, woff)?;
        seq.cold[idx] = word;
        Ok(0)
    })
}

pub extern "C" fn vesper_tiled_tile_col_ptr(
    seq_id: u64,
    tile_idx: u64,
    col_tile_offset: u64,
) -> u64 {
    with(|t| {
        let seq = t.seqs.get(&seq_id).ok_or("unknown TiledSeq handle")?;
        seq.ensure_region_active()?;
        let tile_idx = usize::try_from(tile_idx).map_err(|_| "tile index overflow")?;
        let col_off = usize::try_from(col_tile_offset).map_err(|_| "column offset overflow")?;
        let tile_count = seq.len.div_ceil(seq.tile_width);
        if tile_idx >= tile_count {
            return Err(format!(
                "TiledSeq::load_tile_vec tile index {tile_idx} out of bounds (tile_count {tile_count})"
            ));
        }
        let off = tile_idx
            .checked_mul(seq.tile_stride)
            .and_then(|t| t.checked_add(col_off))
            .filter(|off| *off < seq.hot.bytes)
            .ok_or_else(|| "AoSoA tile column pointer out of bounds".to_string())?;
        // SAFETY: `off < seq.hot.bytes` and `seq` remains in `TILED` until `finish()`.
        Ok(unsafe { seq.hot.as_ptr().add(off) as u64 })
    })
}

/// Clears thread-local JIT `TiledSeq` allocations and returns the first fault, if any.
pub fn finish() -> Result<(), String> {
    TILED.with(|t| {
        let mut t = t.borrow_mut();
        let fault = t.fault.take();
        *t = NativeTiled::default();
        fault.map_or(Ok(()), Err)
    })
}
