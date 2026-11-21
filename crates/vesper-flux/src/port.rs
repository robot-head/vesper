//! The Vesper build of the Z-set kernels, the frontier, the accumulator
//! and the CDC decoder: thin adapters
//! from the crate's types to the Vesper library `flux` (`vesper/`, port wave
//! P3b lane 7, #1069) through the generated binding (`port/binding.rs`).
//! A set's cells are lent in place; each kernel returns the new set's cells.

use crate::zset::ZSet;
use crate::{Budget, Error, Limits};

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, missing_docs, clippy::all)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
pub(crate) mod binding;

use binding::{CdcErr, FrErr, LogErr, ZErr};

fn zset_error(e: ZErr) -> Error {
    match e {
        ZErr::Work => Error::Limit("work"),
        ZErr::Rows => Error::Limit("rows"),
        ZErr::Overflow => Error::Overflow,
        ZErr::Product => Error::Limit("join product"),
        ZErr::Columns => Error::Limit("columns"),
        ZErr::InputEntries => Error::Limit("input entries"),
    }
}

pub(crate) fn frontier_error(e: FrErr) -> Error {
    match e {
        FrErr::Regression => Error::WatermarkRegression,
        FrErr::Holds => Error::Limit("frontier holds"),
        FrErr::Exhausted => Error::IdentityExhausted,
        FrErr::Stale => Error::StaleTicket,
    }
}

fn w(x: usize) -> u64 {
    x as u64
}

/// Runs a kernel on the budget's remaining work, keeping its charges.
fn charged<T>(
    budget: &mut Budget,
    f: impl FnOnce(&mut u64) -> Result<T, ZErr>,
) -> Result<T, Error> {
    let mut left = w(budget.remaining);
    let out = f(&mut left);
    budget.remaining = left as usize;
    out.map_err(zset_error)
}

/// `ZSet::consolidate` over `n` entries of the right width, as cells.
pub(crate) fn consolidate(
    width: usize,
    cells: &[i64],
    n: usize,
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let out = charged(budget, |b| {
        binding::zs_consolidate(
            w(width),
            cells,
            w(n),
            w(limits.max_columns),
            w(limits.max_rows),
            b,
        )
    })?;
    Ok(ZSet::from_cells(width, out))
}

/// The sum of one to three sets of `width`.
pub(crate) fn union(
    width: usize,
    sets: &[&ZSet],
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let (cols, rows) = (w(limits.max_columns), w(limits.max_rows));
    let mut nonempty = sets.iter().filter(|s| !s.is_empty());
    let first = nonempty.next();
    if nonempty.next().is_none() {
        // At most one set has rows: the sum is a copy of it. The kernel
        // charges it, and the cells are copied here.
        let n = first.map_or(0, |s| s.len());
        charged(budget, |b| {
            binding::zs_copy_check(w(width), w(n), cols, rows, b)
        })?;
        return Ok(match first {
            Some(s) => (*s).clone(),
            None => ZSet::from_cells(width, Vec::new()),
        });
    }
    let empty: &[i64] = &[];
    let cells = |i: usize| sets.get(i).map_or(empty, |s| s.cells.as_slice());
    let len = |i: usize| sets.get(i).map_or(0, |s| w(s.len()));
    let out = charged(budget, |b| {
        binding::zs_union3(
            w(width),
            cells(0),
            len(0),
            cells(1),
            len(1),
            cells(2),
            len(2),
            cols,
            rows,
            b,
        )
    })?;
    Ok(ZSet::from_cells(width, out))
}

/// The `Filter` operator: `at_least` is `Predicate::AtLeast`.
pub(crate) fn filter(
    a: &ZSet,
    at_least: bool,
    column: usize,
    value: i64,
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let out = charged(budget, |b| {
        binding::zs_filter(
            w(a.width),
            &a.cells,
            w(a.len()),
            u64::from(at_least),
            w(column),
            value,
            w(limits.max_columns),
            w(limits.max_rows),
            b,
        )
    })?;
    Ok(ZSet::from_cells(a.width, out))
}

/// The `Map` operator: `fields` holds (0, column) or (1, constant) pairs.
pub(crate) fn map(
    a: &ZSet,
    fields: &[i64],
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let nf = fields.len() / 2;
    let out = charged(budget, |b| {
        binding::zs_map(
            w(a.width),
            &a.cells,
            w(a.len()),
            fields,
            w(nf),
            w(limits.max_columns),
            w(limits.max_rows),
            b,
        )
    })?;
    Ok(ZSet::from_cells(nf, out))
}

/// The `Join` operator.
pub(crate) fn join(
    a: &ZSet,
    b: &ZSet,
    ka: usize,
    kb: usize,
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let out = charged(budget, |left| {
        binding::zs_join(
            w(a.width),
            &a.cells,
            w(a.len()),
            w(b.width),
            &b.cells,
            w(b.len()),
            w(ka),
            w(kb),
            w(limits.max_columns),
            w(limits.max_product),
            w(limits.max_rows),
            left,
        )
    })?;
    Ok(ZSet::from_cells(a.width + b.width, out))
}

/// The Vesper accumulator of every input's rows (`zset::Accumulator`).
pub(crate) struct Acc(pub(crate) binding::ZAcc);
impl Acc {
    pub(crate) fn new(widths: &[usize], max_rows: usize) -> Self {
        use std::hash::BuildHasher;
        let widths: Vec<u64> = widths.iter().map(|x| w(*x)).collect();
        // A random key for the row table, as `HashMap` draws one, so that
        // rows chosen to collide cannot slow lookups down. Results do not
        // depend on it: `finish` returns rows in order.
        let key = std::hash::RandomState::new();
        Self(binding::zacc_new(
            &widths,
            w(max_rows),
            key.hash_one(0u64),
            key.hash_one(1u64),
        ))
    }
    pub(crate) fn add(&mut self, input: usize, row: &[i64], weight: i128) -> Result<(), Error> {
        binding::zacc_add(
            &mut self.0,
            w(input),
            row,
            (weight >> 64) as i64,
            weight as u64,
        )
        .map_err(zset_error)
    }
    pub(crate) fn finish(&self, input: usize) -> Result<Vec<i64>, Error> {
        binding::zacc_finish(&self.0, w(input)).map_err(zset_error)
    }
}

/// `Executor::ingest_cdc`'s record loop over a nonempty batch within the
/// byte limit: every record is checked and added to `acc`, and `next`
/// advances. Returns the record count and the largest event time.
pub(crate) fn cdc_ingest(
    acc: &mut crate::zset::Accumulator,
    bytes: &[u8],
    policy: &crate::CdcPolicy,
    inputs: &[usize],
    next: &mut Vec<u64>,
    budget: &mut Budget,
) -> Result<(usize, i64), Error> {
    let parts: Vec<u64> = policy
        .bindings
        .iter()
        .map(|b| u64::from(b.partition_id))
        .collect();
    let mut binds = Vec::with_capacity(4 * policy.bindings.len());
    for b in &policy.bindings {
        binds.extend([
            u64::from(b.schema_id),
            w(b.input),
            u64::from(b.weights == crate::WeightPolicy::Unit),
            w(inputs[b.input]),
        ]);
    }
    let mut left = w(budget.remaining);
    let mut stats = Vec::with_capacity(2);
    let out = binding::cdc_ingest(
        &mut acc.acc.0,
        bytes,
        &parts,
        &binds,
        next,
        w(policy.max_records),
        &mut left,
        &mut stats,
    );
    budget.remaining = left as usize;
    out.map_err(|e| match e {
        CdcErr::Work => Error::Limit("work"),
        CdcErr::Rows => Error::Limit("rows"),
        CdcErr::Overflow => Error::Overflow,
        CdcErr::Records => Error::Limit("CDC records"),
        CdcErr::TruncatedHeader => Error::Cdc("truncated header"),
        CdcErr::Partition => Error::Cdc("partition"),
        CdcErr::Schema => Error::Cdc("schema"),
        CdcErr::Flags => Error::Cdc("reserved flags"),
        CdcErr::Weight => Error::Cdc("weight"),
        CdcErr::Offset => Error::Cdc("source offset"),
        CdcErr::TruncatedRow => Error::Cdc("truncated row"),
    })?;
    Ok((stats[0] as usize, stats[1]))
}

fn log_error(e: LogErr) -> Error {
    match e {
        LogErr::Overflow => Error::Overflow,
        LogErr::Schema => Error::Schema,
        LogErr::Rows => Error::Limit("log rows"),
        LogErr::FrameBytes => Error::Limit("log frame bytes"),
        LogErr::Frame => Error::Storage("log frame"),
        LogErr::Epoch => Error::Storage("log epoch"),
        LogErr::Noncanonical => Error::Storage("noncanonical log rows"),
    }
}

/// The input log's header for inputs of these widths (each fits a u32).
pub(crate) fn log_header(widths: &[usize]) -> Vec<u8> {
    let widths: Vec<u64> = widths.iter().map(|x| w(*x)).collect();
    binding::log_header(&widths)
}

/// One whole frame (length, payload, checksum) of `epoch`.
pub(crate) fn log_frame(
    epoch: u64,
    deltas: &[ZSet],
    widths: &[usize],
    limits: crate::LogLimits,
) -> Result<Vec<u8>, Error> {
    let schema_at = deltas
        .iter()
        .zip(widths)
        .position(|(d, w)| d.width != *w)
        .unwrap_or(deltas.len());
    let mut cells = Vec::new();
    let mut counts = Vec::with_capacity(deltas.len());
    let ws: Vec<u64> = widths.iter().map(|x| w(*x)).collect();
    for d in &deltas[..schema_at] {
        cells.extend_from_slice(&d.cells);
        counts.push(w(d.len()));
    }
    // The input of another width only has to be reached.
    counts.extend(std::iter::repeat_n(0, deltas.len() - schema_at));
    binding::log_frame(
        epoch,
        w(schema_at),
        &cells,
        &counts,
        &ws,
        w(limits.max_rows_per_frame),
        w(limits.max_frame_bytes),
    )
    .map_err(log_error)
}

/// Whether `body` (a payload of `len` bytes and its checksum) checks.
pub(crate) fn log_frame_ok(body: &[u8], len: u32) -> bool {
    binding::log_frame_ok(body, u64::from(len))
}

/// The sets of a checked payload of `epoch`.
pub(crate) fn log_decode(
    payload: &[u8],
    epoch: u64,
    widths: &[usize],
    limits: crate::LogLimits,
) -> Result<Vec<ZSet>, Error> {
    let ws: Vec<u64> = widths.iter().map(|x| w(*x)).collect();
    let out = binding::log_decode(payload, epoch, &ws, w(limits.max_rows_per_frame))
        .map_err(log_error)?;
    let mut at = 0;
    Ok(widths
        .iter()
        .map(|width| {
            let n = out[at] as usize * (width + 1);
            let set = ZSet::from_cells(*width, out[at + 1..at + 1 + n].to_vec());
            at += 1 + n;
            set
        })
        .collect())
}

/// Live allocations of the port's runtime (leak checks).
pub(crate) fn live_allocations() -> i64 {
    binding::live_allocations()
}
