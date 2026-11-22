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

use binding::{CdcErr, CpErr, FrErr, LogErr, ZErr};

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
    // Copy only what the codec reaches: it fails at the first input whose
    // rows overflow or exceed the limit (before its cells), or after the
    // first input that takes the payload over the byte limit. Inputs past
    // that point, and the input of another width, are only counted as 0.
    let (mut rows, mut bytes) = (0usize, 8usize);
    for d in &deltas[..schema_at] {
        counts.push(w(d.len()));
        match rows.checked_add(d.len()) {
            Some(total) if total <= limits.max_rows_per_frame => rows = total,
            _ => break,
        }
        cells.extend_from_slice(&d.cells);
        bytes = bytes.saturating_add(4 + d.cells.len().saturating_mul(8));
        if bytes > limits.max_frame_bytes {
            break;
        }
    }
    counts.resize(deltas.len(), 0);
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

fn cp_error(e: CpErr) -> Error {
    match e {
        CpErr::Work => Error::Limit("work"),
        CpErr::Overflow => Error::Overflow,
        CpErr::Bytes => Error::Limit("checkpoint bytes"),
        CpErr::Truncated => Error::Checkpoint("truncated"),
        CpErr::Checksum => Error::Checkpoint("checksum"),
        CpErr::Version => Error::Checkpoint("version"),
        CpErr::Identity => Error::Checkpoint("identity"),
        CpErr::SourceTag => Error::Checkpoint("source tag"),
        CpErr::Sources => Error::Limit("checkpoint sources"),
        CpErr::WeightPolicy => Error::Checkpoint("weight policy"),
        CpErr::NodeCount => Error::Checkpoint("node count"),
        CpErr::RowSchema => Error::Checkpoint("row schema"),
        CpErr::State => Error::Limit("checkpoint state"),
        CpErr::Noncanonical => Error::Checkpoint("noncanonical rows"),
        CpErr::Trailing => Error::Checkpoint("trailing bytes"),
        CpErr::ManifestVersion => Error::Checkpoint("manifest version"),
        CpErr::BatchVersion => Error::Checkpoint("batch version"),
        CpErr::BatchIdentity => Error::Checkpoint("batch identity"),
        CpErr::BatchHeader => Error::Checkpoint("batch header"),
        CpErr::SpineLevels => Error::Checkpoint("spine levels"),
        CpErr::SpineRanges => Error::Checkpoint("spine ranges"),
        CpErr::BatchRows => Error::Checkpoint("batch rows"),
        CpErr::NamedTwice => Error::Checkpoint("batch named twice"),
        CpErr::ParentTag => Error::Checkpoint("parent tag"),
    }
}

/// Runs `f` on the budget's remaining work as a word, and stores it back.
fn with_budget<T>(
    budget: &mut Budget,
    f: impl FnOnce(&mut u64) -> Result<T, CpErr>,
) -> Result<T, Error> {
    let mut left = w(budget.remaining);
    let out = f(&mut left).map_err(cp_error);
    budget.remaining = left as usize;
    out
}

/// The execution identity digest of a circuit's flattened words, charging
/// one unit of work per word.
pub(crate) fn cp_identity(words: &[u64], budget: &mut Budget) -> Result<[u8; 32], Error> {
    let d = with_budget(budget, |left| binding::cp_identity(words, left))?;
    d.try_into().map_err(|_| Error::Checkpoint("identity"))
}

/// A whole checkpoint image of `sets` (see `vesper/src/image.ves`).
/// `source` is empty for no source, else the batch byte limit, the record
/// limit, the binding count and six words per binding.
pub(crate) fn cp_image(
    id: &[u8; 32],
    ticks: u64,
    source: &[u64],
    sets: &[ZSet],
    limit: usize,
    budget: &mut Budget,
) -> Result<Vec<u8>, Error> {
    let widths: Vec<u64> = sets.iter().map(|s| w(s.width)).collect();
    let counts: Vec<u64> = sets.iter().map(|s| w(s.len())).collect();
    let mut cells = Vec::with_capacity(sets.iter().map(|s| s.cells.len()).sum());
    for s in sets {
        cells.extend_from_slice(&s.cells);
    }
    with_budget(budget, |left| {
        binding::cp_image(id, ticks, source, &widths, &counts, &cells, w(limit), left)
    })
}

/// Checks an image's trailing digest.
pub(crate) fn cp_check(image: &[u8]) -> Result<(), Error> {
    binding::cp_check(image).map_err(cp_error)
}

/// A checked body's head through its source: the cursor after it, the
/// ticks, the source tag, and for tag 1 the two policy limits, the binding
/// count and six words per binding.
pub(crate) fn cp_head(
    body: &[u8],
    expected: &[u8; 32],
    max_sources: usize,
    budget: &mut Budget,
) -> Result<Vec<u64>, Error> {
    with_budget(budget, |left| {
        binding::cp_head(body, expected, w(max_sources), left)
    })
}

/// A checked body's node sets from `cursor`, for nodes of `widths`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cp_sets(
    body: &[u8],
    cursor: u64,
    widths: &[usize],
    max_rows: usize,
    max_nodes: usize,
    max_total_rows: usize,
    max_total_cells: usize,
    budget: &mut Budget,
) -> Result<Vec<ZSet>, Error> {
    let ws: Vec<u64> = widths.iter().map(|x| w(*x)).collect();
    let out = with_budget(budget, |left| {
        binding::cp_sets(
            body,
            cursor,
            &ws,
            w(max_rows),
            w(max_nodes),
            w(max_total_rows),
            w(max_total_cells),
            left,
        )
    })?;
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

/// One durable spine batch blob of `set` (see `vesper/src/spine.ves`).
pub(crate) fn dur_batch(
    fingerprint: &[u8; 32],
    node: usize,
    lo: u64,
    hi: u64,
    set: &ZSet,
    limit: usize,
) -> Result<Vec<u8>, Error> {
    binding::dur_batch(
        fingerprint,
        w(node),
        w(set.width),
        lo,
        hi,
        w(set.len()),
        &set.cells,
        w(limit),
    )
    .map_err(cp_error)
}

/// The set a batch blob holds, checked against its manifest reference.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dur_batch_read(
    blob: &[u8],
    fingerprint: &[u8; 32],
    node: usize,
    width: usize,
    lo: u64,
    hi: u64,
    rows: usize,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let cells = with_budget(budget, |left| {
        binding::dur_batch_read(blob, fingerprint, w(node), w(width), lo, hi, w(rows), left)
    })?;
    Ok(ZSet::from_cells(width, cells))
}

/// A whole manifest blob: `spines` holds per node its batch count, then
/// per batch its digest (four little-endian words), lo, hi and row count;
/// `parent` is empty or the parent manifest's digest words.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dur_manifest(
    fingerprint: &[u8; 32],
    ticks: u64,
    source: &[u64],
    sets: &[ZSet],
    spines: &[u64],
    parent: &[u64],
    limit: usize,
    budget: &mut Budget,
) -> Result<Vec<u8>, Error> {
    let widths: Vec<u64> = sets.iter().map(|s| w(s.width)).collect();
    let counts: Vec<u64> = sets.iter().map(|s| w(s.len())).collect();
    with_budget(budget, |left| {
        binding::dur_manifest(
            fingerprint,
            ticks,
            source,
            &widths,
            &counts,
            spines,
            parent,
            w(limit),
            left,
        )
    })
}

/// A manifest's head through its source (as [`cp_head`]).
pub(crate) fn dur_head(
    body: &[u8],
    expected: &[u8; 32],
    max_sources: usize,
    budget: &mut Budget,
) -> Result<Vec<u64>, Error> {
    with_budget(budget, |left| {
        binding::dur_head(body, expected, w(max_sources), left)
    })
}

/// A manifest's nodes and parent from `cursor`, as words (see
/// `manifest_nodes` in `vesper/src/spine.ves`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn dur_nodes(
    body: &[u8],
    cursor: u64,
    widths: &[usize],
    ticks: u64,
    max_rows: usize,
    max_nodes: usize,
    max_total_rows: usize,
    max_total_cells: usize,
    budget: &mut Budget,
) -> Result<Vec<u64>, Error> {
    use std::hash::BuildHasher;
    let keys = std::hash::RandomState::new();
    let ws: Vec<u64> = widths.iter().map(|x| w(*x)).collect();
    with_budget(budget, |left| {
        binding::dur_nodes(
            body,
            cursor,
            &ws,
            ticks,
            w(max_rows),
            w(max_nodes),
            w(max_total_rows),
            w(max_total_cells),
            keys.hash_one(0u64),
            keys.hash_one(1u64),
            left,
        )
    })
}

/// Live allocations of the port's runtime (leak checks).
pub(crate) fn live_allocations() -> i64 {
    binding::live_allocations()
}
