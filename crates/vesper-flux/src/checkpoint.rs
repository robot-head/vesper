//! Versioned in-memory checkpoint images. Checksums detect corruption, not a
//! hostile producer able to replace the image and checksum. No durable IO occurs.
use super::{
    Circuit, DOp, Executor, Field, Op, Predicate, apply,
    cdc::{CdcPolicy, SourceBinding, SourceState, WeightPolicy},
};
use crate::{Budget, Error, Limits, ZSet};
use std::collections::BTreeMap;

const MAGIC: &[u8; 8] = b"VFLUXCP1";
const VERSION: u16 = 1;
/// Image/parse/validation ceilings. Hard caps: 64MiB image, 1M rows, 4M cells,
/// 65,536 graph/program nodes, 1024 columns/sources, 16M charged structural operations.
#[derive(Clone, Copy, Debug)]
pub struct CheckpointLimits {
    pub max_bytes: usize,
    pub max_total_rows: usize,
    pub max_total_cells: usize,
    pub max_nodes: usize,
    pub max_sources: usize,
    pub max_work: usize,
}
impl Default for CheckpointLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_total_rows: 100_000,
            max_total_cells: 1_000_000,
            max_nodes: 4096,
            max_sources: 1024,
            max_work: 4_000_000,
        }
    }
}
impl CheckpointLimits {
    fn validate(self) -> Result<(), Error> {
        if self.max_bytes < 64
            || self.max_bytes > 64 * 1024 * 1024
            || self.max_total_rows > 1_000_000
            || self.max_total_cells > 4_000_000
            || self.max_nodes == 0
            || self.max_nodes > 65536
            || self.max_sources > 1024
            || self.max_work > 16_000_000
        {
            return Err(Error::Limit("checkpoint configuration"));
        }
        Ok(())
    }
    fn budget(self, circuit: &Circuit) -> Budget {
        Budget::new(Limits {
            max_work: self.max_work.min(circuit.limits.max_work),
            ..circuit.limits
        })
    }
}
struct Writer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Writer {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
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
    fn u8(&mut self, n: u8) -> Result<(), Error> {
        self.put(&[n])
    }
    fn u16(&mut self, n: u16) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
    fn u32(&mut self, n: u32) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
    fn u64(&mut self, n: u64) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
    fn usize(&mut self, n: usize) -> Result<(), Error> {
        self.u64(u64::try_from(n).map_err(|_| Error::Overflow)?)
    }
    fn i64(&mut self, n: i64) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
}
struct Reader<'a, 'b> {
    bytes: &'a [u8],
    cursor: usize,
    budget: &'b mut Budget,
}
impl<'a> Reader<'a, '_> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        self.budget.charge(1)?;
        self.take_prepaid(n)
    }
    fn take_prepaid(&mut self, n: usize) -> Result<&'a [u8], Error> {
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
    fn row(&mut self, width: usize) -> Result<(Vec<i64>, i64), Error> {
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
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| Error::Checkpoint("u16"))?,
        ))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| Error::Checkpoint("u32"))?,
        ))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| Error::Checkpoint("u64"))?,
        ))
    }
    fn usize(&mut self) -> Result<usize, Error> {
        usize::try_from(self.u64()?).map_err(|_| Error::Overflow)
    }
}
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
        }
        Ok(())
    }
}
fn identity(
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
fn count_rows(prior: &[ZSet], limits: CheckpointLimits) -> Result<(), Error> {
    let mut rows = 0usize;
    let mut cells = 0usize;
    for set in prior {
        rows = rows.checked_add(set.len()).ok_or(Error::Overflow)?;
        cells = cells
            .checked_add(set.len().checked_mul(set.width).ok_or(Error::Overflow)?)
            .ok_or(Error::Overflow)?;
        if rows > limits.max_total_rows || cells > limits.max_total_cells {
            return Err(Error::Limit("checkpoint state"));
        }
    }
    Ok(())
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
fn validate_state(
    c: &Circuit,
    prior: &[ZSet],
    ticks: u64,
    source: Option<&SourceState>,
    limits: CheckpointLimits,
    budget: &mut Budget,
) -> Result<(), Error> {
    count_rows(prior, limits)?;
    if prior.len() != c.nodes.len() {
        return Err(Error::Checkpoint("node count"));
    }
    if ticks == 0 && prior.iter().any(|p| !p.is_empty()) {
        return Err(Error::Checkpoint("initial state"));
    }
    if let Some(s) = source {
        s.policy.validate(c)?;
        if s.next.len() != s.policy.bindings.len() {
            return Err(Error::Checkpoint("offset count"));
        }
        let mut consumed = 0u128;
        for (b, next) in s.policy.bindings.iter().zip(&s.next) {
            consumed = consumed
                .checked_add(u128::from(
                    next.checked_sub(b.first_lsn)
                        .ok_or(Error::Checkpoint("offset regression"))?,
                ))
                .ok_or(Error::Overflow)?;
        }
        let max = u128::from(ticks)
            .checked_mul(u128::try_from(s.policy.max_records).map_err(|_| Error::Overflow)?)
            .ok_or(Error::Overflow)?;
        if consumed < u128::from(ticks) || consumed > max {
            return Err(Error::Checkpoint("offset tick count"));
        }
    }
    let eval_limits = Limits {
        max_rows: c.limits.max_rows.min(limits.max_total_rows),
        ..c.limits
    };
    for (i, n) in c.nodes.iter().enumerate() {
        budget.charge(1)?;
        if prior[i].width != n.width || prior[i].len() > c.limits.max_rows {
            return Err(Error::Checkpoint("schema"));
        }
        if !matches!(n.op, Op::Input(_)) && apply(&n.op, prior, eval_limits, budget)? != prior[i] {
            return Err(Error::Checkpoint("inconsistent integral"));
        }
    }
    if prior[c.output].len() > c.limits.max_output {
        return Err(Error::Limit("output"));
    }
    Ok(())
}
impl Executor {
    /// Snapshot all original-node integrals (including input integrals), tick
    /// count and source offsets in one bounded image. Frontier owners are absent.
    /// Creation revalidates relational consistency, so it can reject a state if
    /// full batch validation exceeds the configured row/product/work bounds.
    /// Reserves exact v1 restore parse work before output allocation, so a
    /// successful image is resumable under the same limits and matching graph.
    pub fn checkpoint(&self, limits: CheckpointLimits) -> Result<Vec<u8>, Error> {
        limits.validate()?;
        let mut budget = limits.budget(&self.circuit);
        let id = identity(&self.circuit, self.source.as_ref(), limits, &mut budget)?;
        validate_state(
            &self.circuit,
            &self.prior,
            self.ticks,
            self.source.as_ref(),
            limits,
            &mut budget,
        )?;
        budget.charge(parse_work(&self.prior, self.source.as_ref())?)?;
        let mut w = Writer {
            bytes: Vec::new(),
            limit: limits.max_bytes,
        };
        w.put(MAGIC)?;
        w.u16(VERSION)?;
        w.u16(0)?;
        w.put(&id)?;
        w.u64(self.ticks)?;
        match &self.source {
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
        w.u32(u32::try_from(self.prior.len()).map_err(|_| Error::Overflow)?)?;
        for set in &self.prior {
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
    pub fn restore_checkpoint(
        &mut self,
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
        let mut budget = limits.budget(&self.circuit);
        let expected = identity(&self.circuit, self.source.as_ref(), limits, &mut budget)?;
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
                policy.validate(&self.circuit)?;
                Some(SourceState { policy, next })
            }
            _ => return Err(Error::Checkpoint("source tag")),
        };
        if source.as_ref().map(|s| &s.policy) != self.source.as_ref().map(|s| &s.policy) {
            return Err(Error::Checkpoint("source policy"));
        }
        let count = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
        if count != self.circuit.nodes.len() || count > limits.max_nodes {
            return Err(Error::Checkpoint("node count"));
        }
        let mut prior = Vec::new();
        let mut total_rows = 0usize;
        let mut total_cells = 0usize;
        for n in &self.circuit.nodes {
            let width = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
            let count = r.usize()?;
            if width != n.width || count > self.circuit.limits.max_rows {
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
            prior.push(ZSet { width, rows });
        }
        if r.cursor != body.len() {
            return Err(Error::Checkpoint("trailing bytes"));
        }
        validate_state(
            &self.circuit,
            &prior,
            ticks,
            source.as_ref(),
            limits,
            &mut budget,
        )?;
        self.prior = prior;
        self.ticks = ticks;
        self.source = source;
        Ok(())
    }
}
