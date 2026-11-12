//! Bounded host implementation of relational DBSP. Uses allocating BTreeMaps,
//! checked i128 accumulators and i64 stored weights, not SIMD/arena or compiler lowering.
//! Errors are transactional; tick input is borrowed and all prior integrals survive.
//! Representable accepted computations obey integer arithmetic. Intermediate budget
//! or range failures can reject a computation whose final mathematical result fits.
#![forbid(unsafe_code)]

mod circuit;
mod frontier;
mod zset;

pub use circuit::{
    CdcHeader, CdcPolicy, CheckpointLimits, Circuit, CircuitBuilder, Executor, Field, Ingested,
    NodeId, Predicate, SourceBinding, TickInput, WeightPolicy,
};
pub use frontier::{Frontier, FrontierTicket, TicketError};
pub use zset::{Row, ZSet};

/// Every limit is enforced before its associated growth/work operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_columns: usize,
    pub max_input_entries: usize,
    pub max_rows: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_work: usize,
    pub max_product: usize,
    pub max_output: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_columns: 32,
            max_input_entries: 4096,
            max_rows: 4096,
            max_nodes: 1024,
            max_depth: 128,
            max_work: 1_000_000,
            max_product: 100_000,
            max_output: 4096,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Limit(&'static str),
    Overflow,
    Schema,
    InvalidNode,
    InputCount,
    ForeignTicket,
    StaleTicket,
    WatermarkRegression,
    IdentityExhausted,
    Cdc(&'static str),
    SourceMode,
    Checkpoint(&'static str),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

struct Budget {
    remaining: usize,
}
impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            remaining: limits.max_work,
        }
    }
    fn charge(&mut self, count: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(count)
            .ok_or(Error::Limit("work"))?;
        Ok(())
    }
}
fn identity() -> Result<u64, Error> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut current = NEXT.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(1).ok_or(Error::IdentityExhausted)?;
        match NEXT.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(current),
            Err(observed) => current = observed,
        }
    }
}
