//! Region-resident two-column AoSoA tiles.
//!
//! Each live tile stores two actual contiguous field arrays, separately aligned
//! to 64 bytes. Only initialized row prefixes are exposed. There is no cold
//! side array or source-language/schema lowering in this host profile.

use super::{Region, RegionId, RegionVec};
use crate::{AllocError, ChunkBacking, HandleError, InsertError, OwnerError, RegionError};
use std::alloc::Layout;
use std::fmt;
use std::mem::{MaybeUninit, align_of, size_of};

// Private spelling aliases only; the public result remains an optional tuple
// of slices borrowed from the region, without a separate projection owner.
type Columns<'a, A, B> = Result<Option<(&'a [A], &'a [B])>, HandleError>;
type ColumnsMut<'a, A, B> = Result<Option<(&'a mut [A], &'a mut [B])>, HandleError>;

/// Maximum complete tile size, including both columns, row count and padding.
pub const HARD_MAX_TILE_BYTES: usize = 64 * 1024;

/// Constructor refusal for unsupported host tile shapes or region allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TiledError {
    UnsupportedTileWidth { width: usize },
    UnsupportedFieldLayout,
    Region(RegionError),
}

impl fmt::Display for TiledError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTileWidth { width } => write!(f, "unsupported tile width: {width}"),
            Self::UnsupportedFieldLayout => f.write_str("unsupported tile field layout"),
            Self::Region(error) => write!(f, "tiled region operation: {error}"),
        }
    }
}

impl std::error::Error for TiledError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Region(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RegionError> for TiledError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}

#[repr(C, align(64))]
struct Column<T: Copy, const N: usize>([MaybeUninit<T>; N]);

#[repr(C)]
struct Tile2<A: Copy, B: Copy, const N: usize> {
    first: Column<A, N>,
    second: Column<B, N>,
    initialized: usize,
}

impl<A: Copy, B: Copy, const N: usize> Tile2<A, B, N> {
    // Called only for an owner whose constructor already bounded this complete
    // type to HARD_MAX_TILE_BYTES. Constructors never create a Tile2 value.
    fn one(first: A, second: B) -> Self {
        let mut tile = Self {
            first: Column([MaybeUninit::uninit(); N]),
            second: Column([MaybeUninit::uninit(); N]),
            initialized: 0,
        };
        tile.append(first, second);
        tile
    }

    fn append(&mut self, first: A, second: B) {
        debug_assert!(self.initialized < N);
        self.first.0[self.initialized].write(first);
        self.second.0[self.initialized].write(second);
        // No user code or fallible work occurs between these writes and the
        // publication of the common initialized-row count.
        self.initialized += 1;
    }

    fn columns(&self) -> (&[A], &[B]) {
        // SAFETY: a live Tile2's first `initialized` lanes in BOTH columns
        // hold values supplied by append; the counter is published only after
        // both writes. The actual arrays provide aligned contiguous storage.
        // `&self` excludes changes for the entire returned borrow.
        unsafe {
            (
                std::slice::from_raw_parts(self.first.0.as_ptr().cast::<A>(), self.initialized),
                std::slice::from_raw_parts(self.second.0.as_ptr().cast::<B>(), self.initialized),
            )
        }
    }

    fn columns_mut(&mut self) -> (&mut [A], &mut [B]) {
        let Self {
            first,
            second,
            initialized,
        } = self;
        // SAFETY: as for columns, every exposed lane is initialized. These
        // field borrows name distinct nonzero arrays in one repr(C) tile;
        // each field has its own 64-byte-aligned column wrapper. They cannot
        // alias even when A == B. The single exclusive tile borrow excludes
        // all other region access for the lifetime of both returned slices.
        unsafe {
            (
                std::slice::from_raw_parts_mut(first.0.as_mut_ptr().cast::<A>(), *initialized),
                std::slice::from_raw_parts_mut(second.0.as_mut_ptr().cast::<B>(), *initialized),
            )
        }
    }
}

/// Unique owner of region-backed two-column tiles of width 8, 16, 32 or 64.
///
/// Fields must be nonzero-sized Copy values with alignment at most 64; Default
/// is unnecessary. Every logical row count lives in the region, not this owner.
/// Dropping/forgetting the owner does not release its allocation; region
/// rewind/reset/drop reclaim storage. Mutations of premark reserved storage
/// survive rewind; growth of a premark buffer is protected by the watermark.
///
/// This owner cannot be cloned into a mutable alias:
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let tiles = r.tiled2_new::<u64, u32, 16>().unwrap();
/// let alias = tiles.clone();
/// ```
/// Drop-bearing fields are outside this Copy-only profile:
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let tiles = r.tiled2_new::<String, u32, 16>().unwrap();
/// ```
/// Nonstatic fields cannot outlive their borrows through a region owner:
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let local = String::from("temporary");
/// let mut tiles = r.tiled2_new::<&str, u32, 16>().unwrap();
/// r.tiled2_push(&mut tiles, local.as_str(), 1).unwrap();
/// ```
/// Function-pointer fields themselves satisfy Copy+'static. Ordinary function
/// pointers can narrow their accepted input lifetime:
/// ```
/// fn narrow(value: fn(&str)) -> fn(&'static str) { value }
/// fn inspect(_: &str) {}
/// narrow(inspect)("example");
/// ```
/// The invariant owner prevents that same field-type coercion:
/// ```compile_fail
/// use vesper_alloc::RegionTiled2;
/// fn narrow(owner: RegionTiled2<fn(&str), u32, 16>)
///     -> RegionTiled2<fn(&'static str), u32, 16> { owner }
/// ```
/// A projected view prevents mutation and rewind while it remains live:
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let mut tiles = r.tiled2_new::<u64, u32, 16>().unwrap();
/// r.tiled2_push(&mut tiles, 7, 9).unwrap();
/// let mark = r.checkpoint().unwrap();
/// let (first, _) = r.tiled2_columns(&tiles, 0).unwrap().unwrap();
/// r.rewind(mark).unwrap();
/// assert_eq!(first, [7]);
/// ```
pub struct RegionTiled2<A: Copy + 'static, B: Copy + 'static, const N: usize> {
    tiles: RegionVec<Tile2<A, B, N>>,
}

impl<A: Copy, B: Copy, const N: usize> RegionTiled2<A, B, N> {
    /// Inspect the minting region identity without authorizing any access.
    pub fn region(&self) -> RegionId {
        self.tiles.region()
    }
}

impl<A: Copy, B: Copy, const N: usize> fmt::Debug for RegionTiled2<A, B, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegionTiled2")
            .field("tiles", &self.tiles)
            .field("width", &N)
            .finish()
    }
}

fn rounded_column<T>(width: usize) -> Option<usize> {
    size_of::<T>()
        .checked_mul(width)?
        .checked_add(63)
        .map(|size| size & !63)
}

fn validate_layout<A: Copy, B: Copy, const N: usize>() -> Result<(), TiledError> {
    if !matches!(N, 8 | 16 | 32 | 64) {
        return Err(TiledError::UnsupportedTileWidth { width: N });
    }
    let unsupported = TiledError::UnsupportedFieldLayout;
    if size_of::<A>() == 0 || size_of::<B>() == 0 || align_of::<A>() > 64 || align_of::<B>() > 64 {
        return Err(unsupported);
    }
    let bytes = rounded_column::<A>(N)
        .and_then(|first| rounded_column::<B>(N).and_then(|second| first.checked_add(second)))
        .and_then(|columns| columns.checked_add(size_of::<usize>()))
        .and_then(|size| size.checked_add(63))
        .map(|size| size & !63)
        .ok_or(unsupported)?;
    if bytes > HARD_MAX_TILE_BYTES
        || bytes != size_of::<Tile2<A, B, N>>()
        || align_of::<Tile2<A, B, N>>() != 64
        || Layout::from_size_align(bytes, 64).is_err()
    {
        return Err(unsupported);
    }
    Ok(())
}

fn tile_capacity<const N: usize>(rows: usize) -> Result<usize, AllocError> {
    (rows / N)
        .checked_add(usize::from(!rows.is_multiple_of(N)))
        .ok_or(AllocError::SizeOverflow)
}

impl<R: ChunkBacking> Region<R> {
    /// Create an empty two-column collection without reserving row storage.
    pub fn tiled2_new<A: Copy, B: Copy, const N: usize>(
        &mut self,
    ) -> Result<RegionTiled2<A, B, N>, TiledError> {
        self.tiled2_with_capacity(0)
    }

    /// Reserve initial row capacity, rounded up to complete tiles.
    /// Unsupported representable shapes fail before allocation or construction
    /// of a full tile value. Rust may reject unrepresentable type layouts first.
    pub fn tiled2_with_capacity<A: Copy, B: Copy, const N: usize>(
        &mut self,
        rows: usize,
    ) -> Result<RegionTiled2<A, B, N>, TiledError> {
        validate_layout::<A, B, N>()?;
        let cap = tile_capacity::<N>(rows).map_err(RegionError::from)?;
        cap.checked_mul(size_of::<Tile2<A, B, N>>())
            .filter(|bytes| *bytes <= isize::MAX as usize)
            .ok_or(RegionError::Alloc(AllocError::SizeOverflow))?;
        let tiles = self.vec_with_capacity(cap).map_err(RegionError::from)?;
        Ok(RegionTiled2 { tiles })
    }

    pub fn tiled2_len<A: Copy, B: Copy, const N: usize>(
        &self,
        owner: &RegionTiled2<A, B, N>,
    ) -> Result<usize, HandleError> {
        let tiles = self.vec_as_slice(&owner.tiles)?;
        // Each allocated tile is larger than N bytes and total allocated
        // bytes fit isize, so this sealed live-capacity product cannot overflow.
        Ok(tiles
            .last()
            .map_or(0, |last| (tiles.len() - 1) * N + last.initialized))
    }

    pub fn tiled2_is_empty<A: Copy, B: Copy, const N: usize>(
        &self,
        owner: &RegionTiled2<A, B, N>,
    ) -> Result<bool, HandleError> {
        Ok(self.vec_len(&owner.tiles)? == 0)
    }

    pub fn tiled2_capacity<A: Copy, B: Copy, const N: usize>(
        &self,
        owner: &RegionTiled2<A, B, N>,
    ) -> Result<usize, HandleError> {
        // Nonzero tiles are larger than N and bounded to isize byte capacity.
        Ok(self.vec_capacity(&owner.tiles)? * N)
    }

    pub fn tiled2_tile_count<A: Copy, B: Copy, const N: usize>(
        &self,
        owner: &RegionTiled2<A, B, N>,
    ) -> Result<usize, HandleError> {
        self.vec_len(&owner.tiles)
    }

    /// Copy one logical row; inactive tile lanes are never readable.
    pub fn tiled2_get<A: Copy, B: Copy, const N: usize>(
        &self,
        owner: &RegionTiled2<A, B, N>,
        row: usize,
    ) -> Result<Option<(A, B)>, HandleError> {
        let tiles = self.vec_as_slice(&owner.tiles)?;
        Ok(tiles.get(row / N).and_then(|tile| {
            let (first, second) = tile.columns();
            first
                .get(row % N)
                .zip(second.get(row % N))
                .map(|(&a, &b)| (a, b))
        }))
    }

    /// Shared initialized columns for one tile, borrowing the region.
    pub fn tiled2_columns<'a, A: Copy, B: Copy, const N: usize>(
        &'a self,
        owner: &RegionTiled2<A, B, N>,
        tile: usize,
    ) -> Columns<'a, A, B> {
        Ok(self
            .vec_as_slice(&owner.tiles)?
            .get(tile)
            .map(Tile2::columns))
    }

    /// Both disjoint initialized columns from a single exclusive region borrow.
    pub fn tiled2_columns_mut<'a, A: Copy, B: Copy, const N: usize>(
        &'a mut self,
        owner: &RegionTiled2<A, B, N>,
        tile: usize,
    ) -> ColumnsMut<'a, A, B> {
        Ok(self
            .vec_as_mut_slice(&owner.tiles)?
            .get_mut(tile)
            .map(Tile2::columns_mut))
    }

    /// Reserve for additional logical rows using RegionVec growth semantics.
    pub fn tiled2_reserve<A: Copy, B: Copy, const N: usize>(
        &mut self,
        owner: &mut RegionTiled2<A, B, N>,
        additional_rows: usize,
    ) -> Result<(), RegionError> {
        let required = self
            .tiled2_len(owner)?
            .checked_add(additional_rows)
            .ok_or(AllocError::SizeOverflow)?;
        let required_tiles = tile_capacity::<N>(required)?;
        let live_tiles = self.vec_len(&owner.tiles)?;
        self.vec_reserve(&mut owner.tiles, required_tiles - live_tiles)
    }

    /// Append both actual field values, returning the original pair on failure.
    pub fn tiled2_push<A: Copy, B: Copy, const N: usize>(
        &mut self,
        owner: &mut RegionTiled2<A, B, N>,
        first: A,
        second: B,
    ) -> Result<(), InsertError<(A, B)>> {
        let tiles = self
            .vec_as_mut_slice(&owner.tiles)
            .map_err(|error| InsertError {
                value: (first, second),
                error: error.into(),
            })?;
        if let Some(tile) = tiles.last_mut()
            && tile.initialized < N
        {
            tile.append(first, second);
            return Ok(());
        }
        self.vec_push(&mut owner.tiles, Tile2::one(first, second))
            .map_err(|error| InsertError {
                value: (first, second),
                error: error.error,
            })
    }

    /// Remove the last logical pair; remove its tile when it becomes empty.
    pub fn tiled2_pop<A: Copy, B: Copy, const N: usize>(
        &mut self,
        owner: &mut RegionTiled2<A, B, N>,
    ) -> Result<Option<(A, B)>, HandleError> {
        let tiles = self.vec_as_mut_slice(&owner.tiles)?;
        let Some(last) = tiles.last_mut() else {
            return Ok(None);
        };
        let (first, second) = last.columns();
        let row = (first[last.initialized - 1], second[last.initialized - 1]);
        if last.initialized > 1 {
            last.initialized -= 1;
        } else {
            // This same checked owner was just accessed; no user code occurs
            // between validation and removing the now-empty logical tile.
            self.vec_pop(&mut owner.tiles)?;
        }
        Ok(Some(row))
    }

    /// Release storage through the region, preserving the owner on failure.
    pub fn tiled2_release<A: Copy, B: Copy, const N: usize>(
        &mut self,
        owner: RegionTiled2<A, B, N>,
    ) -> Result<(), OwnerError<RegionTiled2<A, B, N>>> {
        self.vec_release(owner.tiles).map_err(|error| OwnerError {
            owner: RegionTiled2 { tiles: error.owner },
            error: error.error,
        })
    }
}
