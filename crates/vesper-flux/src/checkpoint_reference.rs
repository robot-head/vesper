//! The Rust checkpoint image codec that `vesper/src/image.ves` replaced,
//! kept as the reference of the differential test in `checkpoint.rs` (the
//! Aura pattern). Only the codec is here: state validation is shared.
use super::super::cdc::{SourceState, WeightPolicy};
use super::super::{Circuit, DOp, Executor, Field, Op, Predicate};
use super::codec::{Reader, Writer, read_source, write_source};
use super::{CheckpointLimits, validate_state};
use crate::{Budget, Error, ZSet};
use std::collections::BTreeMap;

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
