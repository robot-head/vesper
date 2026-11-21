//! Versioned in-memory checkpoint images. Checksums detect corruption, not a
//! hostile producer able to replace the image and checksum. No durable IO occurs.
//!
//! The image codec and the identity digest run in Vesper
//! (`vesper/src/image.ves`, through `crate::port`); this module flattens the
//! circuit and policy into words for it and rebuilds them from its words.
use super::{
    Circuit, DOp, Executor, Field, Op, Predicate, apply,
    cdc::{CdcPolicy, SourceBinding, SourceState, WeightPolicy},
};
use crate::{Budget, Error, Limits, ZSet, port};

#[path = "checkpoint_codec.rs"]
mod codec;
#[cfg(test)]
#[path = "checkpoint_differential.rs"]
mod differential;
#[path = "durable.rs"]
pub(super) mod durable;
#[cfg(test)]
#[path = "checkpoint_reference.rs"]
mod reference;

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
    pub(crate) fn validate(self) -> Result<(), Error> {
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
    pub(crate) fn budget(self, circuit: &Circuit) -> Budget {
        Budget::new(Limits {
            max_work: self.max_work.min(circuit.limits.max_work),
            ..circuit.limits
        })
    }
}
/// The circuit, its limits and the source policy as the words the identity
/// digest covers, in order.
struct Words(Vec<u64>);
impl Words {
    fn usize(&mut self, n: usize) {
        self.0.push(n as u64);
    }
    fn op(&mut self, op: &Op) {
        let w = &mut self.0;
        match op {
            Op::Input(a) => w.extend([0, *a as u64]),
            Op::Filter(a, p) => {
                w.extend([1, *a as u64]);
                match p {
                    Predicate::Equal { column, value } => {
                        w.extend([0, *column as u64, *value as u64]);
                    }
                    Predicate::AtLeast { column, value } => {
                        w.extend([1, *column as u64, *value as u64]);
                    }
                }
            }
            Op::Map(a, fields) => {
                w.extend([2, *a as u64, fields.len() as u64]);
                for f in fields {
                    match f {
                        Field::Column(c) => w.extend([0, *c as u64]),
                        Field::Constant(v) => w.extend([1, *v as u64]),
                    }
                }
            }
            Op::Union(a, b) => w.extend([3, *a as u64, *b as u64]),
            Op::Join { a, b, ka, kb } => {
                w.extend([4, *a as u64, *b as u64, *ka as u64, *kb as u64]);
            }
            Op::Exchange { a, key } => w.extend([5, *a as u64, *key as u64]),
        }
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
    let mut h = Words(Vec::new());
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
        h.usize(n);
    }
    for w in &c.inputs {
        h.usize(*w);
    }
    h.usize(c.nodes.len());
    for n in &c.nodes {
        h.usize(n.width);
        h.usize(n.depth);
        h.op(&n.op);
    }
    h.usize(c.diff.len());
    for n in &c.diff {
        h.usize(n.depth);
        match &n.op {
            DOp::Delta(a) => h.0.extend([0, *a as u64]),
            DOp::Previous(a) => h.0.extend([1, *a as u64]),
            DOp::Apply(op) => {
                h.0.push(2);
                h.op(op);
            }
            DOp::Sum3(a, b, c) => h.0.extend([3, *a as u64, *b as u64, *c as u64]),
        }
    }
    h.usize(c.delta.len());
    for d in &c.delta {
        h.usize(*d);
    }
    match source {
        None => h.0.push(0),
        Some(s) => {
            h.0.push(1);
            h.usize(s.policy.max_batch_bytes);
            h.usize(s.policy.max_records);
            h.usize(s.policy.bindings.len());
            for b in &s.policy.bindings {
                h.0.extend([
                    u64::from(b.partition_id),
                    u64::from(b.schema_id),
                    b.input as u64,
                    b.first_lsn,
                    weight_tag(b.weights),
                ]);
            }
        }
    }
    port::cp_identity(&h.0, budget)
}
fn weight_tag(w: WeightPolicy) -> u64 {
    match w {
        WeightPolicy::Unit => 0,
        WeightPolicy::NonZero => 1,
    }
}
pub(crate) fn count_rows(prior: &[ZSet], limits: CheckpointLimits) -> Result<(), Error> {
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
pub(crate) fn validate_state(
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
        // Input and exchange integrals are received, not derived: on a
        // cluster worker an exchange holds the rows routed to it.
        if !matches!(n.op, Op::Input(_) | Op::Exchange { .. })
            && apply(&n.op, prior, eval_limits, budget)? != prior[i]
        {
            return Err(Error::Checkpoint("inconsistent integral"));
        }
    }
    if prior[c.output].len() > c.limits.max_output {
        return Err(Error::Limit("output"));
    }
    Ok(())
}
/// The source as the image codec's words: empty for none, else the two
/// policy limits, the binding count and six words per binding.
fn source_words(source: Option<&SourceState>) -> Vec<u64> {
    let Some(s) = source else {
        return Vec::new();
    };
    let mut w = vec![
        s.policy.max_batch_bytes as u64,
        s.policy.max_records as u64,
        s.policy.bindings.len() as u64,
    ];
    for (b, next) in s.policy.bindings.iter().zip(&s.next) {
        w.extend([
            u64::from(b.partition_id),
            u64::from(b.schema_id),
            b.input as u64,
            b.first_lsn,
            weight_tag(b.weights),
            *next,
        ]);
    }
    w
}
/// The source a checked image holds (from the codec's head words), checked
/// against the circuit as `CdcPolicy::validate` does.
fn read_source(head: &[u64], circuit: &Circuit) -> Result<Option<SourceState>, Error> {
    if head[2] == 0 {
        return Ok(None);
    }
    let mut bindings = Vec::new();
    let mut next = Vec::new();
    for b in head[6..].as_chunks::<6>().0 {
        bindings.push(SourceBinding {
            partition_id: b[0] as u32,
            schema_id: b[1] as u16,
            input: usize::try_from(b[2]).map_err(|_| Error::Overflow)?,
            first_lsn: b[3],
            weights: if b[4] == 0 {
                WeightPolicy::Unit
            } else {
                WeightPolicy::NonZero
            },
        });
        next.push(b[5]);
    }
    let policy = CdcPolicy {
        bindings,
        max_batch_bytes: usize::try_from(head[3]).map_err(|_| Error::Overflow)?,
        max_records: usize::try_from(head[4]).map_err(|_| Error::Overflow)?,
    };
    policy.validate(circuit)?;
    Ok(Some(SourceState { policy, next }))
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
        port::cp_image(
            &id,
            self.ticks,
            &source_words(self.source.as_ref()),
            &self.prior,
            limits.max_bytes,
            &mut budget,
        )
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
        port::cp_check(image)?;
        let body = &image[..image.len() - 32];
        let mut budget = limits.budget(&self.circuit);
        let expected = identity(&self.circuit, self.source.as_ref(), limits, &mut budget)?;
        let head = port::cp_head(body, &expected, limits.max_sources, &mut budget)?;
        let ticks = head[1];
        let source = read_source(&head, &self.circuit)?;
        if source.as_ref().map(|s| &s.policy) != self.source.as_ref().map(|s| &s.policy) {
            return Err(Error::Checkpoint("source policy"));
        }
        let widths: Vec<usize> = self.circuit.nodes.iter().map(|n| n.width).collect();
        let prior = port::cp_sets(
            body,
            head[0],
            &widths,
            self.circuit.limits.max_rows,
            limits.max_nodes,
            limits.max_total_rows,
            limits.max_total_cells,
            &mut budget,
        )?;
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
        self.lineage = super::fresh_lineage();
        Ok(())
    }
}
