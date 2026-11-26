//! Vectorized (column-batch) execution of checked [`PreparedQuery`] plans:
//! single-table scans, filters, projections and aggregates, hash joins
//! (equality conjuncts of `ON` as keys, the rest as a residual predicate
//! evaluated over candidate-pair batches), vectorized nested-loop joins
//! when `ON` has no equality between the two sides, and chains of three or
//! more inner joins reordered by a greedy cost model over the measured
//! input sizes and key distinct counts, with their result put back into
//! written order. Results can be written into AoSoA tiles ([`TileSet`])
//! that are reused across executions.
//!
//! The executor runs in Vesper (`vesper/src/vector.ves`, SH6 lane 6,
//! #1401/#1999): rows are kept as tuples of input row positions, so no
//! value is copied, predicates run as whole-batch loops producing SQL
//! three-valued masks, and the selected rows of each batch are counted to
//! answer `COUNT` aggregates. Rows, order, errors and the `work` counter
//! equal the Rust executor this replaced.
//!
//! Output order equals the row executor's: left-major, matches in right
//! order, an unmatched left row right after its (absent) matches, unmatched
//! right rows at the end.
use crate::execute::{DataSet, QueryResult};
use crate::port::{self, VMode};
use crate::plan::PreparedQuery;
use crate::{Error, ErrorKind, HermeticCatalog, Result, Value};
use std::collections::BTreeMap;
use vesper_simd::{IsaProfileKind, JitScanner};

mod tiles;
pub use tiles::{Tile, TileSet, TILE_LANES};

/// Rows per column batch unless configured otherwise.
pub const DEFAULT_BATCH: usize = 1024;
const MAX_BATCH: usize = 1 << 16;

/// Batch size plus a selection-mask counter (the `vesper-simd` Baseline128
/// JIT byte scanner when the host supports it, a portable count
/// otherwise). Not `Send` when it holds JIT code (the scanner keeps its
/// module in an `Rc`).
pub struct VectorEngine {
    scanner: Option<JitScanner>,
    batch: usize,
}
impl std::fmt::Debug for VectorEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorEngine")
            .field("simd", &self.simd_active())
            .field("batch", &self.batch)
            .finish()
    }
}
impl VectorEngine {
    /// Uses the Baseline128 JIT scanner when the host supports it.
    pub fn detect() -> Self {
        Self {
            scanner: JitScanner::compile_profile(IsaProfileKind::Baseline128).ok(),
            batch: DEFAULT_BATCH,
        }
    }
    /// Portable byte counting only (for differential tests and hosts
    /// without Baseline128).
    pub fn portable() -> Self {
        Self {
            scanner: None,
            batch: DEFAULT_BATCH,
        }
    }
    /// Batch size 1..=65,536 rows.
    pub fn with_batch(mut self, rows: usize) -> Result<Self> {
        if rows == 0 || rows > MAX_BATCH {
            return Err(Error::new(ErrorKind::Limit, "batch size out of range"));
        }
        self.batch = rows;
        Ok(self)
    }
    pub fn simd_active(&self) -> bool {
        self.scanner.is_some()
    }
    pub fn batch(&self) -> usize {
        self.batch
    }
    /// Number of bytes equal to 1 in a 0/1 mask.
    pub fn count_ones(&self, mask: &[u8]) -> usize {
        match &self.scanner {
            Some(s) => s.scan(mask, 1).matches as usize,
            None => mask.iter().filter(|&&b| b == 1).count(),
        }
    }
}

/// SQL aggregates over output column indexes of a query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    /// `COUNT(*)`.
    CountRows,
    /// `COUNT(col)`: non-null values.
    Count(usize),
    /// `SUM(col)` over U64, I64 or F64. Integer sums are exact and fail with
    /// `Overflow` iff the exact total does not fit; F64 sums add in row order.
    Sum(usize),
    /// Smallest non-null value; the first one wins among equals (+0/-0).
    Min(usize),
    Max(usize),
}

/// Reference row-at-a-time aggregation over a materialized result (for
/// example from the row executor). Same semantics as the vectorized path.
pub fn aggregate_rows(result: &QueryResult, aggs: &[Aggregate]) -> Result<Vec<Value>> {
    port::aggregate_rows(result, aggs)
}

impl PreparedQuery {
    /// The input orders the vectorized executor chooses for each chain of
    /// three or more inner joins (table names in execution order). Runs the
    /// plan's joins (not the projection) to measure input sizes.
    pub fn vectorized_join_orders(
        &self,
        catalog: &HermeticCatalog,
        data: &DataSet,
        params: &BTreeMap<String, Value>,
        engine: &VectorEngine,
    ) -> Result<Vec<Vec<String>>> {
        port::vexecute(self, catalog, data, params, engine.batch, VMode::Orders).orders()
    }

    /// Vectorized execution. Rows, order and column definitions equal
    /// [`PreparedQuery::execute`]; `work` counts batch cells and is not
    /// comparable with the row executor's counter.
    pub fn execute_vectorized(
        &self,
        catalog: &HermeticCatalog,
        data: &DataSet,
        params: &BTreeMap<String, Value>,
        engine: &VectorEngine,
    ) -> Result<QueryResult> {
        port::vexecute(self, catalog, data, params, engine.batch, VMode::Rows).rows(self)
    }

    /// Vectorized aggregates over the query's output columns, without
    /// materializing output rows.
    pub fn aggregate_vectorized(
        &self,
        catalog: &HermeticCatalog,
        data: &DataSet,
        params: &BTreeMap<String, Value>,
        engine: &VectorEngine,
        aggs: &[Aggregate],
    ) -> Result<Vec<Value>> {
        port::vexecute(
            self,
            catalog,
            data,
            params,
            engine.batch,
            VMode::Aggregates(aggs),
        )
        .aggregates(aggs.len())
    }
}
