//! The Rust checkpoint codecs that `vesper/src/image.ves` and
//! `vesper/src/spine.ves` replaced (the in-memory image, and the durable
//! manifests and batches), kept as the reference of the differential tests
//! in `checkpoint_differential.rs` (the Aura pattern). Only the codecs are
//! here: state validation and the spine policy are shared.
use super::super::cdc::{CdcPolicy, SourceBinding, SourceState, WeightPolicy};
use super::super::{Circuit, DOp, Executor, Field, Op, Predicate};
use super::durable::{
    BATCH_MAGIC, BatchRef, MANIFEST_MAGIC, MAX_SPINE_LEVELS, ParsedManifest, blob_id,
};
use super::{CheckpointLimits, validate_state};
use crate::{Budget, Error, ZSet};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use vesper_loom_cas::ObjectId;

const MAGIC: &[u8; 8] = b"VFLUXCP1";
const VERSION: u16 = 1;

struct Identity<'a> {
    hash: blake3::Hasher,
    budget: &'a mut Budget,
}
impl Identity<'_> {
    fn u64(&mut self, n: u64) -> Result<(), Error> {
        self.budget.charge(1)?;
        self.hash.update(&n.to_le_bytes());
        Ok(())
    }
    fn usize(&mut self, n: usize) -> Result<(), Error> {
        self.u64(u64::try_from(n).map_err(|_| Error::Overflow)?)
    }
    fn i64(&mut self, n: i64) -> Result<(), Error> {
        self.budget.charge(1)?;
        self.hash.update(&n.to_le_bytes());
        Ok(())
    }
    fn op(&mut self, op: &Op) -> Result<(), Error> {
        match op {
            Op::Input(a) => {
                self.u64(0)?;
                self.usize(*a)?;
            }
            Op::Filter(a, p) => {
                self.u64(1)?;
                self.usize(*a)?;
                match p {
                    Predicate::Equal { column, value } => {
                        self.u64(0)?;
                        self.usize(*column)?;
                        self.i64(*value)?;
                    }
                    Predicate::AtLeast { column, value } => {
                        self.u64(1)?;
                        self.usize(*column)?;
                        self.i64(*value)?;
                    }
                }
            }
            Op::Map(a, fields) => {
                self.u64(2)?;
                self.usize(*a)?;
                self.usize(fields.len())?;
                for f in fields {
                    match f {
                        Field::Column(c) => {
                            self.u64(0)?;
                            self.usize(*c)?;
                        }
                        Field::Constant(v) => {
                            self.u64(1)?;
                            self.i64(*v)?;
                        }
                    }
                }
            }
            Op::Union(a, b) => {
                self.u64(3)?;
                self.usize(*a)?;
                self.usize(*b)?;
            }
            Op::Join { a, b, ka, kb } => {
                self.u64(4)?;
                self.usize(*a)?;
                self.usize(*b)?;
                self.usize(*ka)?;
                self.usize(*kb)?;
            }
            Op::Exchange { a, key } => {
                self.u64(5)?;
                self.usize(*a)?;
                self.usize(*key)?;
            }
        }
        Ok(())
    }
}
pub(crate) fn identity(
    c: &Circuit,
    source: Option<&SourceState>,
    limits: CheckpointLimits,
    budget: &mut Budget,
) -> Result<[u8; 32], Error> {
    if c.nodes.len() > limits.max_nodes
        || c.diff.len() > limits.max_nodes
        || c.nodes.iter().any(|n| n.width > 1024)
        || source.is_some_and(|s| s.policy.bindings.len() > limits.max_sources)
    {
        return Err(Error::Limit("checkpoint graph"));
    }
    let mut h = Identity {
        hash: blake3::Hasher::new(),
        budget,
    };
    h.hash
        .update(b"vesper.flux.execution-identity/1\0vesper.arith.checked/1\0row.i64.le/1");
    for n in [
        c.limits.max_columns,
        c.limits.max_input_entries,
        c.limits.max_rows,
        c.limits.max_nodes,
        c.limits.max_depth,
        c.limits.max_work,
        c.limits.max_product,
        c.limits.max_output,
        c.output,
        c.inputs.len(),
    ] {
        h.usize(n)?;
    }
    for w in &c.inputs {
        h.usize(*w)?;
    }
    h.usize(c.nodes.len())?;
    for n in &c.nodes {
        h.usize(n.width)?;
        h.usize(n.depth)?;
        h.op(&n.op)?;
    }
    h.usize(c.diff.len())?;
    for n in &c.diff {
        h.usize(n.depth)?;
        match &n.op {
            DOp::Delta(a) => {
                h.u64(0)?;
                h.usize(*a)?;
            }
            DOp::Previous(a) => {
                h.u64(1)?;
                h.usize(*a)?;
            }
            DOp::Apply(op) => {
                h.u64(2)?;
                h.op(op)?;
            }
            DOp::Sum3(a, b, c) => {
                h.u64(3)?;
                h.usize(*a)?;
                h.usize(*b)?;
                h.usize(*c)?;
            }
        }
    }
    h.usize(c.delta.len())?;
    for d in &c.delta {
        h.usize(*d)?;
    }
    match source {
        None => h.u64(0)?,
        Some(s) => {
            h.u64(1)?;
            h.usize(s.policy.max_batch_bytes)?;
            h.usize(s.policy.max_records)?;
            h.usize(s.policy.bindings.len())?;
            for b in &s.policy.bindings {
                h.u64(u64::from(b.partition_id))?;
                h.u64(u64::from(b.schema_id))?;
                h.usize(b.input)?;
                h.u64(b.first_lsn)?;
                h.u64(match b.weights {
                    WeightPolicy::Unit => 0,
                    WeightPolicy::NonZero => 1,
                })?;
            }
        }
    }
    Ok(*h.hash.finalize().as_bytes())
}
/// Exact charged reads of the v1 parser for a canonical image. Creation
/// reserves this instead of its smaller serialization walk, so the same work
/// budget admits restore (identity + parsing + relational validation).
fn parse_work(prior: &[ZSet], source: Option<&SourceState>) -> Result<usize, Error> {
    // Magic/version/flags/identity/ticks/source tag/node count, then width/count
    // per node. Source metadata adds two policy limits, count and six fields
    // per binding. A row adds one scalar read per cell and one weight read.
    let mut work = 7usize
        .checked_add(prior.len().checked_mul(2).ok_or(Error::Overflow)?)
        .ok_or(Error::Overflow)?;
    if let Some(source) = source {
        work = work
            .checked_add(3)
            .and_then(|n| {
                source
                    .policy
                    .bindings
                    .len()
                    .checked_mul(6)
                    .and_then(|count| n.checked_add(count))
            })
            .ok_or(Error::Overflow)?;
    }
    for set in prior {
        let reads = set.width.checked_add(1).ok_or(Error::Overflow)?;
        work = work
            .checked_add(set.len().checked_mul(reads).ok_or(Error::Overflow)?)
            .ok_or(Error::Overflow)?;
    }
    Ok(work)
}
/// Snapshot all original-node integrals (including input integrals), tick
/// count and source offsets in one bounded image. Frontier owners are absent.
/// Creation revalidates relational consistency, so it can reject a state if
/// full batch validation exceeds the configured row/product/work bounds.
/// Reserves exact v1 restore parse work before output allocation, so a
/// successful image is resumable under the same limits and matching graph.
pub(crate) fn checkpoint(e: &Executor, limits: CheckpointLimits) -> Result<Vec<u8>, Error> {
    limits.validate()?;
    let mut budget = limits.budget(&e.circuit);
    let id = identity(&e.circuit, e.source.as_ref(), limits, &mut budget)?;
    validate_state(
        &e.circuit,
        &e.prior,
        e.ticks,
        e.source.as_ref(),
        limits,
        &mut budget,
    )?;
    budget.charge(parse_work(&e.prior, e.source.as_ref())?)?;
    let mut w = Writer {
        bytes: Vec::new(),
        limit: limits.max_bytes,
    };
    w.put(MAGIC)?;
    w.u16(VERSION)?;
    w.u16(0)?;
    w.put(&id)?;
    w.u64(e.ticks)?;
    write_source(&mut w, e.source.as_ref())?;
    w.u32(u32::try_from(e.prior.len()).map_err(|_| Error::Overflow)?)?;
    for set in &e.prior {
        w.u32(u32::try_from(set.width).map_err(|_| Error::Overflow)?)?;
        w.usize(set.len())?;
        for (row, weight) in set.entries() {
            for value in row {
                w.i64(*value)?;
            }
            w.i64(weight)?;
        }
    }
    let checksum = blake3::hash(&w.bytes);
    w.put(checksum.as_bytes())?;
    Ok(w.bytes)
}
/// Restore only into the identical circuit, limits and prebound CDC policy
/// (bind expected sources on a fresh executor first). Every check precedes
/// publication; errors preserve live state/offsets. No frontier IDs, tickets,
/// holds or tracker origins are serialized, renewed or resurrected.
pub(crate) fn restore(
    e: &mut Executor,
    image: &[u8],
    limits: CheckpointLimits,
) -> Result<(), Error> {
    limits.validate()?;
    if image.len() > limits.max_bytes {
        return Err(Error::Limit("checkpoint bytes"));
    }
    let split = image
        .len()
        .checked_sub(32)
        .ok_or(Error::Checkpoint("truncated"))?;
    let (body, checksum) = image.split_at(split);
    if blake3::hash(body).as_bytes() != checksum {
        return Err(Error::Checkpoint("checksum"));
    }
    let mut budget = limits.budget(&e.circuit);
    let expected = identity(&e.circuit, e.source.as_ref(), limits, &mut budget)?;
    let mut r = Reader {
        bytes: body,
        cursor: 0,
        budget: &mut budget,
    };
    if r.take(8)? != MAGIC || r.u16()? != VERSION || r.u16()? != 0 {
        return Err(Error::Checkpoint("version"));
    }
    if r.take(32)? != expected {
        return Err(Error::Checkpoint("identity"));
    }
    let ticks = r.u64()?;
    let source = read_source(&mut r, &e.circuit, limits)?;
    if source.as_ref().map(|s| &s.policy) != e.source.as_ref().map(|s| &s.policy) {
        return Err(Error::Checkpoint("source policy"));
    }
    let count = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
    if count != e.circuit.nodes.len() || count > limits.max_nodes {
        return Err(Error::Checkpoint("node count"));
    }
    let mut prior = Vec::new();
    let mut total_rows = 0usize;
    let mut total_cells = 0usize;
    for n in &e.circuit.nodes {
        let width = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
        let count = r.usize()?;
        if width != n.width || count > e.circuit.limits.max_rows {
            return Err(Error::Checkpoint("row schema"));
        }
        total_rows = total_rows.checked_add(count).ok_or(Error::Overflow)?;
        total_cells = total_cells
            .checked_add(count.checked_mul(width).ok_or(Error::Overflow)?)
            .ok_or(Error::Overflow)?;
        if total_rows > limits.max_total_rows || total_cells > limits.max_total_cells {
            return Err(Error::Limit("checkpoint state"));
        }
        let mut rows = BTreeMap::new();
        for _ in 0..count {
            let (row, weight) = r.row(width)?;
            if weight == 0 || rows.last_key_value().is_some_and(|(last, _)| last >= &row) {
                return Err(Error::Checkpoint("noncanonical rows"));
            }
            rows.insert(row, weight);
        }
        prior.push(ZSet::from_rows(width, rows));
    }
    if r.cursor != body.len() {
        return Err(Error::Checkpoint("trailing bytes"));
    }
    validate_state(
        &e.circuit,
        &prior,
        ticks,
        source.as_ref(),
        limits,
        &mut budget,
    )?;
    e.prior = prior;
    e.ticks = ticks;
    e.source = source;
    e.lineage = super::super::fresh_lineage();
    Ok(())
}

// The byte writer and reader, and the source section.
pub(crate) struct Writer {
    pub(crate) bytes: Vec<u8>,
    pub(crate) limit: usize,
}
impl Writer {
    pub(crate) fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(Error::Overflow)?;
        if end > self.limit {
            return Err(Error::Limit("checkpoint bytes"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    pub(crate) fn u8(&mut self, n: u8) -> Result<(), Error> {
        self.put(&[n])
    }
    pub(crate) fn u16(&mut self, n: u16) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
    pub(crate) fn u32(&mut self, n: u32) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
    pub(crate) fn u64(&mut self, n: u64) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
    pub(crate) fn usize(&mut self, n: usize) -> Result<(), Error> {
        self.u64(u64::try_from(n).map_err(|_| Error::Overflow)?)
    }
    pub(crate) fn i64(&mut self, n: i64) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
}
pub(crate) struct Reader<'a, 'b> {
    pub(crate) bytes: &'a [u8],
    pub(crate) cursor: usize,
    pub(crate) budget: &'b mut Budget,
}
impl<'a> Reader<'a, '_> {
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        self.budget.charge(1)?;
        self.take_prepaid(n)
    }
    pub(crate) fn take_prepaid(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.cursor.checked_add(n).ok_or(Error::Overflow)?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or(Error::Checkpoint("truncated"))?;
        self.cursor = end;
        Ok(bytes)
    }
    /// Charge all scalar reads and validate the complete encoded row before
    /// allocating its vector; no partial row growth precedes those checks.
    pub(crate) fn row(&mut self, width: usize) -> Result<(Vec<i64>, i64), Error> {
        let reads = width.checked_add(1).ok_or(Error::Overflow)?;
        self.budget.charge(reads)?;
        let size = reads.checked_mul(8).ok_or(Error::Overflow)?;
        let bytes = self.take_prepaid(size)?;
        let cells = width.checked_mul(8).ok_or(Error::Overflow)?;
        let row = bytes[..cells]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|cell| i64::from_le_bytes(*cell))
            .collect();
        let weight = i64::from_le_bytes(
            bytes[cells..]
                .try_into()
                .map_err(|_| Error::Checkpoint("weight"))?,
        );
        Ok((row, weight))
    }
    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| Error::Checkpoint("u16"))?,
        ))
    }
    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| Error::Checkpoint("u32"))?,
        ))
    }
    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| Error::Checkpoint("u64"))?,
        ))
    }
    pub(crate) fn usize(&mut self) -> Result<usize, Error> {
        usize::try_from(self.u64()?).map_err(|_| Error::Overflow)
    }
}

pub(crate) fn write_source(w: &mut Writer, source: Option<&SourceState>) -> Result<(), Error> {
    match source {
        None => w.u8(0)?,
        Some(s) => {
            w.u8(1)?;
            w.usize(s.policy.max_batch_bytes)?;
            w.usize(s.policy.max_records)?;
            w.u32(u32::try_from(s.policy.bindings.len()).map_err(|_| Error::Overflow)?)?;
            for (b, next) in s.policy.bindings.iter().zip(&s.next) {
                w.u32(b.partition_id)?;
                w.u16(b.schema_id)?;
                w.usize(b.input)?;
                w.u64(b.first_lsn)?;
                w.u8(match b.weights {
                    WeightPolicy::Unit => 0,
                    WeightPolicy::NonZero => 1,
                })?;
                w.u64(*next)?;
            }
        }
    }
    Ok(())
}
pub(crate) fn read_source(
    r: &mut Reader<'_, '_>,
    circuit: &Circuit,
    limits: CheckpointLimits,
) -> Result<Option<SourceState>, Error> {
    let source = match r.u8()? {
        0 => None,
        1 => {
            let max_batch_bytes = r.usize()?;
            let max_records = r.usize()?;
            let count = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
            if count > limits.max_sources || count > 1024 {
                return Err(Error::Limit("checkpoint sources"));
            }
            let mut bindings = Vec::new();
            let mut next = Vec::new();
            for _ in 0..count {
                let partition_id = r.u32()?;
                let schema_id = r.u16()?;
                let input = r.usize()?;
                let first_lsn = r.u64()?;
                let weights = match r.u8()? {
                    0 => WeightPolicy::Unit,
                    1 => WeightPolicy::NonZero,
                    _ => return Err(Error::Checkpoint("weight policy")),
                };
                let next_lsn = r.u64()?;
                bindings.push(SourceBinding {
                    partition_id,
                    schema_id,
                    input,
                    first_lsn,
                    weights,
                });
                next.push(next_lsn);
            }
            let policy = CdcPolicy {
                bindings,
                max_batch_bytes,
                max_records,
            };
            policy.validate(circuit)?;
            Some(SourceState { policy, next })
        }
        _ => return Err(Error::Checkpoint("source tag")),
    };
    Ok(source)
}

// The durable codecs (durable.rs).
const DURABLE_VERSION: u16 = 1;

pub(crate) fn encode_batch(
    fingerprint: &[u8; 32],
    node: usize,
    lo: u64,
    hi: u64,
    set: &ZSet,
    limits: CheckpointLimits,
) -> Result<Vec<u8>, Error> {
    let mut w = Writer {
        bytes: Vec::new(),
        limit: limits.max_bytes,
    };
    w.put(BATCH_MAGIC)?;
    w.u16(DURABLE_VERSION)?;
    w.u16(0)?;
    w.put(fingerprint)?;
    w.u32(u32::try_from(node).map_err(|_| Error::Overflow)?)?;
    w.u32(u32::try_from(set.width).map_err(|_| Error::Overflow)?)?;
    w.u64(lo)?;
    w.u64(hi)?;
    w.usize(set.len())?;
    for (row, weight) in set.entries() {
        for value in row {
            w.i64(*value)?;
        }
        w.i64(weight)?;
    }
    Ok(w.bytes)
}

pub(crate) fn decode_batch(
    blob: &[u8],
    b: &BatchRef,
    fingerprint: &[u8; 32],
    node: usize,
    width: usize,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let mut r = Reader {
        bytes: blob,
        cursor: 0,
        budget,
    };
    if r.take(8)? != BATCH_MAGIC || r.u16()? != DURABLE_VERSION || r.u16()? != 0 {
        return Err(Error::Checkpoint("batch version"));
    }
    if r.take(32)? != fingerprint {
        return Err(Error::Checkpoint("batch identity"));
    }
    if usize::try_from(r.u32()?).map_err(|_| Error::Overflow)? != node
        || usize::try_from(r.u32()?).map_err(|_| Error::Overflow)? != width
        || r.u64()? != b.lo
        || r.u64()? != b.hi
        || r.usize()? != b.rows
    {
        return Err(Error::Checkpoint("batch header"));
    }
    let mut set = BTreeMap::new();
    for _ in 0..b.rows {
        let (row, weight) = r.row(width)?;
        if weight == 0 || set.last_key_value().is_some_and(|(last, _)| last >= &row) {
            return Err(Error::Checkpoint("noncanonical rows"));
        }
        set.insert(row, weight);
    }
    if r.cursor != blob.len() {
        return Err(Error::Checkpoint("trailing bytes"));
    }
    Ok(ZSet::from_rows(width, set))
}

/// Upper bound of the charged reads of [`DurableCheckpointer::restore`]'s
/// parser and spine sums for these spines.
fn durable_parse_work(exec: &Executor, spines: &[Vec<BatchRef>]) -> Result<usize, Error> {
    let mut work = 16usize;
    if let Some(source) = &exec.source {
        work = source
            .policy
            .bindings
            .len()
            .checked_mul(6)
            .and_then(|n| n.checked_add(work + 8))
            .ok_or(Error::Overflow)?;
    }
    for (set, spine) in exec.prior.iter().zip(spines) {
        // Per node: width, rows, batch count; per reference: four reads; per
        // batch: nine header reads; per row: width + 1 reads and width + 1
        // accumulator charges.
        let per_row = set
            .width
            .checked_add(1)
            .and_then(|n| n.checked_mul(2))
            .ok_or(Error::Overflow)?;
        work = work.checked_add(3).ok_or(Error::Overflow)?;
        for b in spine {
            work = b
                .rows
                .checked_mul(per_row)
                .and_then(|n| n.checked_add(13))
                .and_then(|n| n.checked_add(work))
                .ok_or(Error::Overflow)?;
        }
    }
    Ok(work)
}

pub(crate) fn decode_manifest(
    image: &[u8],
    c: &Circuit,
    fingerprint: &[u8; 32],
    limits: CheckpointLimits,
    budget: &mut Budget,
) -> Result<ParsedManifest, Error> {
    let mut r = Reader {
        bytes: image,
        cursor: 0,
        budget,
    };
    if r.take(8)? != MANIFEST_MAGIC || r.u16()? != DURABLE_VERSION || r.u16()? != 0 {
        return Err(Error::Checkpoint("manifest version"));
    }
    if r.take(32)? != fingerprint {
        return Err(Error::Checkpoint("identity"));
    }
    let ticks = r.u64()?;
    let source = read_source(&mut r, c, limits)?;
    let count = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
    if count != c.nodes.len() || count > limits.max_nodes {
        return Err(Error::Checkpoint("node count"));
    }
    let mut nodes = Vec::with_capacity(count);
    let (mut total_rows, mut total_cells, mut batch_rows) = (0usize, 0usize, 0usize);
    let mut seen = BTreeSet::new();
    for n in &c.nodes {
        let width = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
        let rows = r.usize()?;
        let batches = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
        if width != n.width || rows > c.limits.max_rows {
            return Err(Error::Checkpoint("row schema"));
        }
        if batches > MAX_SPINE_LEVELS {
            return Err(Error::Checkpoint("spine levels"));
        }
        total_rows = total_rows.checked_add(rows).ok_or(Error::Overflow)?;
        total_cells = total_cells
            .checked_add(rows.checked_mul(width).ok_or(Error::Overflow)?)
            .ok_or(Error::Overflow)?;
        if total_rows > limits.max_total_rows || total_cells > limits.max_total_cells {
            return Err(Error::Limit("checkpoint state"));
        }
        let mut spine: Vec<BatchRef> = Vec::with_capacity(batches);
        for _ in 0..batches {
            let digest: [u8; 32] = r
                .take(32)?
                .try_into()
                .map_err(|_| Error::Checkpoint("digest"))?;
            let lo = r.u64()?;
            let hi = r.u64()?;
            let rows = r.usize()?;
            if lo >= hi || hi > ticks || spine.last().is_some_and(|p| p.hi > lo) {
                return Err(Error::Checkpoint("spine ranges"));
            }
            if rows == 0 || rows > c.limits.max_rows {
                return Err(Error::Checkpoint("batch rows"));
            }
            batch_rows = batch_rows.checked_add(rows).ok_or(Error::Overflow)?;
            if batch_rows > limits.max_total_rows {
                return Err(Error::Limit("checkpoint state"));
            }
            let id = blob_id(digest);
            // Batches are bound to node and range, so no object can be named twice.
            if !seen.insert(id) {
                return Err(Error::Checkpoint("batch named twice"));
            }
            spine.push(BatchRef { id, lo, hi, rows });
        }
        nodes.push((width, rows, spine));
    }
    let parent = match r.u8()? {
        0 => None,
        1 => Some(blob_id(
            r.take(32)?
                .try_into()
                .map_err(|_| Error::Checkpoint("digest"))?,
        )),
        _ => return Err(Error::Checkpoint("parent tag")),
    };
    if r.cursor != image.len() {
        return Err(Error::Checkpoint("trailing bytes"));
    }
    Ok(ParsedManifest {
        ticks,
        source,
        nodes,
        parent,
    })
}

pub(crate) fn encode_manifest(
    exec: &Executor,
    fingerprint: &[u8; 32],
    spines: &[Vec<BatchRef>],
    parent: Option<&ObjectId>,
    limits: CheckpointLimits,
    budget: &mut Budget,
) -> Result<Vec<u8>, Error> {
    // Reserve the restore parse work, so a successful checkpoint is
    // restorable under the same limits (as the in-memory image does).
    budget.charge(durable_parse_work(exec, spines)?)?;
    let mut w = Writer {
        bytes: Vec::new(),
        limit: limits.max_bytes,
    };
    w.put(MANIFEST_MAGIC)?;
    w.u16(DURABLE_VERSION)?;
    w.u16(0)?;
    w.put(fingerprint)?;
    w.u64(exec.ticks)?;
    write_source(&mut w, exec.source.as_ref())?;
    w.u32(u32::try_from(exec.prior.len()).map_err(|_| Error::Overflow)?)?;
    for (set, spine) in exec.prior.iter().zip(spines) {
        w.u32(u32::try_from(set.width).map_err(|_| Error::Overflow)?)?;
        w.usize(set.len())?;
        w.u32(u32::try_from(spine.len()).map_err(|_| Error::Overflow)?)?;
        for b in spine {
            w.put(b.id.digest.as_bytes())?;
            w.u64(b.lo)?;
            w.u64(b.hi)?;
            w.usize(b.rows)?;
        }
    }
    match parent {
        Some(p) => {
            w.u8(1)?;
            w.put(p.digest.as_bytes())?;
        }
        None => w.u8(0)?,
    }
    Ok(w.bytes)
}
