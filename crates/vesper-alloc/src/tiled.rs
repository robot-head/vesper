//! `Tiled<T, N>`: AoSoA storage for `tiled(N) struct` records (§15.5).
//!
//! A record type implements [`TiledRecord`], splitting itself into a tuple of
//! hot columns and one cold value. Hot columns are stored tile by tile: each
//! tile holds `N` lanes (8, 16, 32 or 64) of every hot column, and every column
//! array is a separate [`Col`] aligned to 64 bytes, so a column scan reads only
//! that column's cache lines. Cold values live in a separate side array that
//! hot scans never touch.
//!
//! This is safe Rust over host-heap `Vec` storage (fallible `try_reserve`,
//! bounded by [`TiledConfig::max_len`]); the region-backed two-column variant
//! is [`crate::RegionTiled2`]. Source `tiled(N) struct` syntax, schema
//! derivation and VVec SIMD loads are compiler work and are not provided here.
//! Hot column types must be `Copy + Default`: unused lanes of the last tile
//! hold `Default` values and are never exposed.
//!
//! ```
//! use vesper_alloc::{Col, Tiled, TiledRecord};
//!
//! #[derive(Clone, Copy, Debug, PartialEq)]
//! struct LogOffsetEntry { base_offset: u64, batch_crc: u32, byte_len: u32, producer_id: u64 }
//!
//! impl TiledRecord for LogOffsetEntry {
//!     type Hot = (u64, u32, u32);
//!     type Cold = u64;
//!     fn split(self) -> ((u64, u32, u32), u64) {
//!         ((self.base_offset, self.batch_crc, self.byte_len), self.producer_id)
//!     }
//!     fn join((base_offset, batch_crc, byte_len): (u64, u32, u32), producer_id: u64) -> Self {
//!         Self { base_offset, batch_crc, byte_len, producer_id }
//!     }
//! }
//!
//! let mut index = Tiled::<LogOffsetEntry, 16>::new();
//! for i in 0..40u32 {
//!     let e = LogOffsetEntry { base_offset: i as u64 * 100, batch_crc: i % 3, byte_len: 7, producer_id: 9 };
//!     index.push(e).unwrap();
//! }
//! // Scan only the CRC column, tile by tile (16 + 16 + 8 lanes).
//! let hits: usize = index.column(|t| &t.1).map(|lanes| lanes.iter().filter(|c| **c == 0).count()).sum();
//! assert_eq!(hits, 14);
//! assert_eq!(index.tile_count(), 3);
//! ```
//!
//! Tile widths other than 8, 16, 32 and 64 are rejected at compile time:
//!
//! ```compile_fail
//! use vesper_alloc::{Tiled, TiledRecord};
//! #[derive(Clone, Copy)]
//! struct R(u32);
//! impl TiledRecord for R {
//!     type Hot = (u32,);
//!     type Cold = ();
//!     fn split(self) -> ((u32,), ()) { ((self.0,), ()) }
//!     fn join(h: (u32,), (): ()) -> Self { R(h.0) }
//! }
//! let _ = Tiled::<R, 12>::new();
//! ```
//!
//! A tile cannot hand out the same column mutably twice:
//!
//! ```compile_fail
//! use vesper_alloc::{Tiled, TiledRecord};
//! #[derive(Clone, Copy)]
//! struct R(u32, u32);
//! impl TiledRecord for R {
//!     type Hot = (u32, u32);
//!     type Cold = ();
//!     fn split(self) -> ((u32, u32), ()) { ((self.0, self.1), ()) }
//!     fn join(h: (u32, u32), (): ()) -> Self { R(h.0, h.1) }
//! }
//! let mut t = Tiled::<R, 8>::new();
//! t.push(R(1, 2)).unwrap();
//! let tile = t.tile_mut(0).unwrap();
//! let _ = tile.cols2_mut(|x| (&mut x.0, &mut x.0));
//! ```
#![forbid(unsafe_code)]

use std::fmt;

/// One column of one tile: `N` lanes, starting on a 64-byte boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C, align(64))]
pub struct Col<A, const N: usize>([A; N]);

impl<A: fmt::Debug, const N: usize> fmt::Debug for Col<A, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.iter()).finish()
    }
}

impl<A: Copy + Default, const N: usize> Col<A, N> {
    fn empty() -> Self {
        Col([A::default(); N])
    }
    /// Address of lane 0 (for layout checks).
    pub fn addr(&self) -> usize {
        self.0.as_ptr() as usize
    }
}

/// A tuple of hot column types (arity 1 to 6).
pub trait HotColumns: Copy + 'static {
    /// The tile: one [`Col`] per column, in tuple order.
    type Tile<const N: usize>: Copy + 'static;
    fn empty_tile<const N: usize>() -> Self::Tile<N>;
    fn read<const N: usize>(tile: &Self::Tile<N>, lane: usize) -> Self;
    fn write<const N: usize>(self, tile: &mut Self::Tile<N>, lane: usize);
}

macro_rules! hot_columns {
    ($($t:ident $i:tt),+) => {
        impl<$($t: Copy + Default + 'static),+> HotColumns for ($($t,)+) {
            type Tile<const N: usize> = ($(Col<$t, N>,)+);
            fn empty_tile<const N: usize>() -> Self::Tile<N> {
                ($(Col::<$t, N>::empty(),)+)
            }
            fn read<const N: usize>(tile: &Self::Tile<N>, lane: usize) -> Self {
                ($(tile.$i.0[lane],)+)
            }
            fn write<const N: usize>(self, tile: &mut Self::Tile<N>, lane: usize) {
                $(tile.$i.0[lane] = self.$i;)+
            }
        }
    };
}
hot_columns!(A 0);
hot_columns!(A 0, B 1);
hot_columns!(A 0, B 1, C 2);
hot_columns!(A 0, B 1, C 2, D 3);
hot_columns!(A 0, B 1, C 2, D 3, E 4);
hot_columns!(A 0, B 1, C 2, D 3, E 4, F 5);

/// A record stored in [`Tiled`]: a hot tuple plus a cold remainder (`()` when
/// every field is hot). `join(split(x)) == x` must hold.
pub trait TiledRecord: Copy + 'static {
    type Hot: HotColumns;
    type Cold: Copy + 'static;
    fn split(self) -> (Self::Hot, Self::Cold);
    fn join(hot: Self::Hot, cold: Self::Cold) -> Self;
}

/// Bounds of a [`Tiled`] sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TiledConfig {
    pub max_len: usize,
}

impl Default for TiledConfig {
    fn default() -> Self {
        Self { max_len: 1 << 28 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TiledSeqError {
    /// More than `max_len` records.
    LengthLimit {
        limit: usize,
    },
    SizeOverflow,
    OutOfMemory,
    OutOfBounds {
        index: usize,
        len: usize,
    },
}

impl fmt::Display for TiledSeqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for TiledSeqError {}

/// Failed push; the record is returned.
#[derive(Debug, PartialEq, Eq)]
pub struct TiledPushError<T> {
    pub value: T,
    pub error: TiledSeqError,
}

/// Read view of one tile: only its initialized lanes are exposed.
pub struct TileRef<'a, H: HotColumns, const N: usize> {
    tile: &'a H::Tile<N>,
    len: usize,
}

impl<'a, H: HotColumns, const N: usize> TileRef<'a, H, N> {
    /// Initialized lanes in this tile (`N` except possibly in the last tile).
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// The whole tile, including unused `Default` lanes (for layout checks).
    pub fn raw(&self) -> &'a H::Tile<N> {
        self.tile
    }
    /// Projects one column: `tile.col(|t| &t.1)`.
    pub fn col<C: 'static>(
        &self,
        project: impl FnOnce(&'a H::Tile<N>) -> &'a Col<C, N>,
    ) -> &'a [C] {
        &project(self.tile).0[..self.len]
    }
}

/// Mutable view of one tile.
pub struct TileMut<'a, H: HotColumns, const N: usize> {
    tile: &'a mut H::Tile<N>,
    len: usize,
}

impl<'a, H: HotColumns, const N: usize> TileMut<'a, H, N> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// One mutable column: `tile.col_mut(|t| &mut t.0)`.
    pub fn col_mut<C: 'static>(
        self,
        project: impl FnOnce(&'a mut H::Tile<N>) -> &'a mut Col<C, N>,
    ) -> &'a mut [C] {
        &mut project(self.tile).0[..self.len]
    }
    /// Two disjoint mutable columns: `tile.cols2_mut(|t| (&mut t.0, &mut t.2))`.
    /// Rust's borrow checker rejects a projection that names one column twice.
    pub fn cols2_mut<C: 'static, D: 'static>(
        self,
        project: impl FnOnce(&'a mut H::Tile<N>) -> (&'a mut Col<C, N>, &'a mut Col<D, N>),
    ) -> (&'a mut [C], &'a mut [D]) {
        let (c, d) = project(self.tile);
        (&mut c.0[..self.len], &mut d.0[..self.len])
    }
}

/// AoSoA sequence of `T` records, `N` per tile (`TiledSeq[T]` in the design).
pub struct Tiled<T: TiledRecord, const N: usize> {
    tiles: Vec<<T::Hot as HotColumns>::Tile<N>>,
    cold: Vec<T::Cold>,
    len: usize,
    config: TiledConfig,
}

impl<T: TiledRecord + fmt::Debug, const N: usize> fmt::Debug for Tiled<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: TiledRecord, const N: usize> Default for Tiled<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: TiledRecord, const N: usize> Clone for Tiled<T, N> {
    fn clone(&self) -> Self {
        Self {
            tiles: self.tiles.clone(),
            cold: self.cold.clone(),
            len: self.len,
            config: self.config,
        }
    }
}

impl<T: TiledRecord, const N: usize> Tiled<T, N> {
    const WIDTH_OK: () = assert!(
        N == 8 || N == 16 || N == 32 || N == 64,
        "tiled(N) requires N = 8, 16, 32 or 64"
    );

    pub fn new() -> Self {
        Self::with_config(TiledConfig::default())
    }

    pub fn with_config(config: TiledConfig) -> Self {
        let () = Self::WIDTH_OK;
        Self {
            tiles: Vec::new(),
            cold: Vec::new(),
            len: 0,
            config,
        }
    }

    pub fn with_capacity(capacity: usize) -> Result<Self, TiledSeqError> {
        let mut s = Self::new();
        s.reserve(capacity)?;
        Ok(s)
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }
    /// Records that fit without allocating.
    pub fn capacity(&self) -> usize {
        self.tiles
            .capacity()
            .saturating_mul(N)
            .min(self.cold.capacity())
    }

    /// Ensures `additional` more pushes do not allocate.
    pub fn reserve(&mut self, additional: usize) -> Result<(), TiledSeqError> {
        let want = self
            .len
            .checked_add(additional)
            .ok_or(TiledSeqError::SizeOverflow)?;
        if want > self.config.max_len {
            return Err(TiledSeqError::LengthLimit {
                limit: self.config.max_len,
            });
        }
        let tiles = want.div_ceil(N);
        self.tiles
            .try_reserve(tiles.saturating_sub(self.tiles.len()))
            .map_err(|_| TiledSeqError::OutOfMemory)?;
        self.cold
            .try_reserve(want - self.len)
            .map_err(|_| TiledSeqError::OutOfMemory)?;
        Ok(())
    }

    pub fn push(&mut self, value: T) -> Result<(), TiledPushError<T>> {
        if let Err(error) = self.reserve(1) {
            return Err(TiledPushError { value, error });
        }
        let (hot, cold) = value.split();
        let lane = self.len % N;
        if lane == 0 {
            self.tiles.push(T::Hot::empty_tile());
        }
        let tile = self.tiles.last_mut().expect("tile for lane");
        hot.write(tile, lane);
        self.cold.push(cold);
        self.len += 1;
        Ok(())
    }

    pub fn pop(&mut self) -> Option<T> {
        let last = self.len.checked_sub(1)?;
        let value = self.get(last)?;
        self.len = last;
        self.cold.pop();
        if last % N == 0 {
            self.tiles.pop();
        } else {
            // Restore the vacated lane to its default.
            let tile = self.tiles.last_mut().expect("tile");
            T::Hot::read(&T::Hot::empty_tile::<N>(), 0).write(tile, last % N);
        }
        Some(value)
    }

    pub fn truncate(&mut self, len: usize) {
        while self.len > len {
            self.pop();
        }
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.cold.clear();
        self.len = 0;
    }

    pub fn get(&self, index: usize) -> Option<T> {
        if index >= self.len {
            return None;
        }
        let hot = T::Hot::read(&self.tiles[index / N], index % N);
        Some(T::join(hot, self.cold[index]))
    }

    pub fn hot(&self, index: usize) -> Option<T::Hot> {
        (index < self.len).then(|| T::Hot::read(&self.tiles[index / N], index % N))
    }

    pub fn cold(&self, index: usize) -> Option<&T::Cold> {
        self.cold.get(index)
    }

    pub fn cold_mut(&mut self, index: usize) -> Option<&mut T::Cold> {
        self.cold.get_mut(index)
    }

    /// The cold side array (one entry per record).
    pub fn cold_slice(&self) -> &[T::Cold] {
        &self.cold
    }

    pub fn set(&mut self, index: usize, value: T) -> Result<(), TiledSeqError> {
        if index >= self.len {
            return Err(TiledSeqError::OutOfBounds {
                index,
                len: self.len,
            });
        }
        let (hot, cold) = value.split();
        hot.write(&mut self.tiles[index / N], index % N);
        self.cold[index] = cold;
        Ok(())
    }

    fn tile_len(&self, t: usize) -> usize {
        (self.len - t * N).min(N)
    }

    pub fn tile(&self, t: usize) -> Option<TileRef<'_, T::Hot, N>> {
        let len = (t < self.tiles.len()).then(|| self.tile_len(t))?;
        Some(TileRef {
            tile: &self.tiles[t],
            len,
        })
    }

    pub fn tile_mut(&mut self, t: usize) -> Option<TileMut<'_, T::Hot, N>> {
        let len = (t < self.tiles.len()).then(|| self.tile_len(t))?;
        Some(TileMut {
            tile: &mut self.tiles[t],
            len,
        })
    }

    pub fn tiles(&self) -> impl ExactSizeIterator<Item = TileRef<'_, T::Hot, N>> + '_ {
        let len = self.len;
        self.tiles.iter().enumerate().map(move |(t, tile)| TileRef {
            tile,
            len: (len - t * N).min(N),
        })
    }

    pub fn tiles_mut(&mut self) -> impl ExactSizeIterator<Item = TileMut<'_, T::Hot, N>> + '_ {
        let len = self.len;
        self.tiles
            .iter_mut()
            .enumerate()
            .map(move |(t, tile)| TileMut {
                tile,
                len: (len - t * N).min(N),
            })
    }

    /// Initialized lanes of one column, one slice per tile.
    pub fn column<'a, C: 'static>(
        &'a self,
        project: impl Fn(&'a <T::Hot as HotColumns>::Tile<N>) -> &'a Col<C, N> + 'a,
    ) -> impl Iterator<Item = &'a [C]> + 'a {
        let len = self.len;
        self.tiles
            .iter()
            .enumerate()
            .map(move |(t, tile)| &project(tile).0[..(len - t * N).min(N)])
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = T> + '_ {
        (0..self.len).map(|i| self.get(i).expect("in range"))
    }
}
