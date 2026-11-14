//! AoSoA result tiles (§24.3 `tiled(16)` output): rows are grouped in tiles
//! of 16 lanes; inside a tile each fixed-width ("hot") column is one
//! contiguous `[u64; 16]` lane array, and text/bytes ("cold") columns are
//! `(offset, len)` lane arrays into one byte arena of the tile set.
//!
//! A [`TileSet`] is reused across executions: `execute_tiled` clears it but
//! keeps its tiles and arena, so refilling a warmed set with a result of the
//! same shape and size allocates no tiles and no arena space.
use super::{BatchSink, Column, Data, Operand, VectorEngine};
use crate::execute::DataSet;
use crate::plan::PreparedQuery;
use crate::{add, bound, ColumnDef, Error, ErrorKind, HermeticCatalog, Result, ScalarType, Value};
use std::collections::BTreeMap;

/// Rows per tile.
pub const TILE_LANES: usize = 16;

/// One tile: up to 16 rows of every output column.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    lanes: usize,
    hot: Vec<[u64; TILE_LANES]>,
    cold: Vec<[(u32, u32); TILE_LANES]>,
    /// Bit `lane` of `nulls[column]` is set for SQL NULL.
    nulls: Vec<u16>,
}
impl Tile {
    fn new(hot: usize, cold: usize, columns: usize) -> Self {
        Self {
            lanes: 0,
            hot: vec![[0; TILE_LANES]; hot],
            cold: vec![[(0, 0); TILE_LANES]; cold],
            nulls: vec![0; columns],
        }
    }
    fn reset(&mut self) {
        self.lanes = 0;
        self.nulls.iter_mut().for_each(|m| *m = 0);
    }
    pub fn lanes(&self) -> usize {
        self.lanes
    }
    /// Lane array of hot column `k` (the k-th fixed-width output column):
    /// u64 values, i64 and f64 bit patterns, booleans as 0/1.
    pub fn hot(&self, k: usize) -> &[u64; TILE_LANES] {
        &self.hot[k]
    }
    pub fn null_mask(&self, column: usize) -> u16 {
        self.nulls[column]
    }
}

/// A borrowed cell of a projected batch.
enum Cell<'a> {
    Null,
    Word(u64),
    Bytes(&'a [u8]),
}
fn column_cell(c: &Column, i: usize) -> Cell<'_> {
    if c.nulls[i] == 1 {
        return Cell::Null;
    }
    match &c.data {
        Data::U64(x) => Cell::Word(x[i]),
        Data::I64(x) => Cell::Word(x[i] as u64),
        Data::F64(x) => Cell::Word(x[i].to_bits()),
        Data::Bool(x) => Cell::Word(u64::from(x[i])),
        Data::Text(x) => Cell::Bytes(x[i].as_bytes()),
        Data::Bytes(x) => Cell::Bytes(&x[i]),
    }
}
fn value_cell(v: &Value) -> Cell<'_> {
    match v {
        Value::Null => Cell::Null,
        Value::U64(x) => Cell::Word(*x),
        Value::I64(x) => Cell::Word(*x as u64),
        Value::F64(x) => Cell::Word(x.to_bits()),
        Value::Bool(x) => Cell::Word(u64::from(*x)),
        Value::Text(s) => Cell::Bytes(s.as_bytes()),
        Value::Bytes(b) => Cell::Bytes(b),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Hot(usize),
    Cold(usize),
}

/// Reusable AoSoA output of a query.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TileSet {
    columns: Vec<ColumnDef>,
    places: Vec<Place>,
    tiles: Vec<Tile>,
    used: usize,
    len: usize,
    arena: Vec<u8>,
}
impl TileSet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn columns(&self) -> &[ColumnDef] {
        &self.columns
    }
    /// Tiles holding the current rows (the last one may be partial).
    pub fn tiles(&self) -> &[Tile] {
        &self.tiles[..self.used]
    }
    /// Tiles allocated, including ones kept for reuse.
    pub fn allocated_tiles(&self) -> usize {
        self.tiles.len()
    }
    pub fn arena_capacity(&self) -> usize {
        self.arena.capacity()
    }
    /// Index among hot columns of output column `c`, if it is fixed-width.
    pub fn hot_index(&self, c: usize) -> Option<usize> {
        match self.places.get(c) {
            Some(Place::Hot(k)) => Some(*k),
            _ => None,
        }
    }
    fn shape(columns: &[ColumnDef]) -> (Vec<Place>, usize, usize) {
        let (mut hot, mut cold) = (0, 0);
        let places = columns
            .iter()
            .map(|c| match c.ty {
                ScalarType::Text | ScalarType::Bytes => {
                    cold += 1;
                    Place::Cold(cold - 1)
                }
                _ => {
                    hot += 1;
                    Place::Hot(hot - 1)
                }
            })
            .collect();
        (places, hot, cold)
    }
    /// Empties the set for `columns`, keeping tiles of the same shape.
    pub fn reset(&mut self, columns: &[ColumnDef]) {
        let (places, _, _) = Self::shape(columns);
        if places != self.places {
            self.tiles.clear();
        }
        self.columns = columns.to_vec();
        self.places = places;
        self.used = 0;
        self.len = 0;
        self.arena.clear();
    }
    fn push<'c>(&mut self, cell: &dyn Fn(usize) -> Cell<'c>) -> Result<()> {
        let lane = self.len % TILE_LANES;
        if lane == 0 {
            if self.used == self.tiles.len() {
                let (_, hot, cold) = Self::shape(&self.columns);
                self.tiles.push(Tile::new(hot, cold, self.columns.len()));
            }
            self.tiles[self.used].reset();
            self.used += 1;
        }
        let tile = &mut self.tiles[self.used - 1];
        for (c, place) in self.places.iter().enumerate() {
            match (place, cell(c)) {
                (Place::Hot(k), Cell::Null) => {
                    tile.nulls[c] |= 1 << lane;
                    tile.hot[*k][lane] = 0;
                }
                (Place::Cold(k), Cell::Null) => {
                    tile.nulls[c] |= 1 << lane;
                    tile.cold[*k][lane] = (0, 0);
                }
                (Place::Hot(k), Cell::Word(w)) => tile.hot[*k][lane] = w,
                (Place::Cold(k), Cell::Bytes(bytes)) => {
                    let off = u32::try_from(self.arena.len())
                        .map_err(|_| Error::new(ErrorKind::Limit, "tile arena limit"))?;
                    self.arena.extend_from_slice(bytes);
                    tile.cold[*k][lane] = (off, bytes.len() as u32);
                }
                _ => return Err(Error::new(ErrorKind::Type, "tile column type")),
            }
        }
        tile.lanes = lane + 1;
        self.len += 1;
        Ok(())
    }
    /// Value of output column `c` in row `row`.
    pub fn value(&self, row: usize, c: usize) -> Option<Value> {
        if row >= self.len || c >= self.columns.len() {
            return None;
        }
        let tile = &self.tiles[row / TILE_LANES];
        let lane = row % TILE_LANES;
        if tile.nulls[c] & (1 << lane) != 0 {
            return Some(Value::Null);
        }
        Some(match (self.places[c], self.columns[c].ty) {
            (Place::Hot(k), ScalarType::U64) => Value::U64(tile.hot[k][lane]),
            (Place::Hot(k), ScalarType::I64) => Value::I64(tile.hot[k][lane] as i64),
            (Place::Hot(k), ScalarType::F64) => Value::F64(f64::from_bits(tile.hot[k][lane])),
            (Place::Hot(k), ScalarType::Bool) => Value::Bool(tile.hot[k][lane] == 1),
            (Place::Cold(k), ty) => {
                let (off, len) = tile.cold[k][lane];
                let bytes = &self.arena[off as usize..(off + len) as usize];
                if ty == ScalarType::Text {
                    Value::Text(String::from_utf8(bytes.to_vec()).expect("text stored as UTF-8"))
                } else {
                    Value::Bytes(bytes.to_vec())
                }
            }
            _ => unreachable!("places follow column types"),
        })
    }
    pub fn row(&self, row: usize) -> Option<Vec<Value>> {
        (row < self.len).then(|| {
            (0..self.columns.len())
                .map(|c| self.value(row, c).expect("in range"))
                .collect()
        })
    }
    pub fn rows(&self) -> Vec<Vec<Value>> {
        (0..self.len)
            .map(|r| self.row(r).expect("in range"))
            .collect()
    }
}

impl PreparedQuery {
    /// Vectorized execution into reusable AoSoA tiles. Rows and order equal
    /// [`PreparedQuery::execute`]; returns the batch work counter.
    pub fn execute_tiled(
        &self,
        catalog: &HermeticCatalog,
        data: &DataSet,
        params: &BTreeMap<String, Value>,
        engine: &VectorEngine,
        out: &mut TileSet,
    ) -> Result<usize> {
        out.reset(&self.output);
        let limits = self.limits;
        let mut bytes = 0usize;
        let sink: &mut BatchSink<'_> = &mut |cols: &[Operand<'_>], sel: &[usize]| {
            for &i in sel {
                bound(add(out.len(), 1)?, limits.output_rows, "output row limit")?;
                let cell = |c: usize| match &cols[c] {
                    Operand::Col(col) => column_cell(col, i),
                    Operand::Scalar(v) => value_cell(v),
                };
                // Output bytes are accounted like the row result's.
                let mut row = size_of::<Vec<Value>>();
                for c in 0..cols.len() {
                    row = add(row, size_of::<Value>())?;
                    if let Cell::Bytes(b) = cell(c) {
                        row = add(row, b.len())?;
                    }
                }
                bytes = add(bytes, row)?;
                bound(bytes, limits.output_bytes, "output byte limit")?;
                out.push(&cell)?;
            }
            Ok(())
        };
        self.drive(catalog, data, params, engine, sink)
    }
}
