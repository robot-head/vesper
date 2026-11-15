#[path = "cdc.rs"]
mod cdc;
#[path = "checkpoint.rs"]
mod checkpoint;
pub use cdc::{CdcHeader, CdcPolicy, Ingested, SourceBinding, WeightPolicy};
pub use checkpoint::CheckpointLimits;
pub use checkpoint::durable::{
    DurableCheckpointer, DurableReport, DurableRestore, MAX_SPINE_LEVELS,
};
#[path = "cluster.rs"]
mod cluster;
pub use cluster::{
    CheckpointMode, CheckpointPhase, ClusterConfig, ClusterRun, CommittedCheckpoint, EpochSource,
    Fault, FaultKind, GcReport, LiveMigration, LiveRescale, Recovery, Rescale, run_cluster,
    run_cluster_with,
};
#[path = "kafka.rs"]
mod kafka;
pub use kafka::{
    KafkaBinding, KafkaConfig, KafkaConnection, KafkaPosition, KafkaSink, KafkaSinkConfig,
    KafkaSinkStats, KafkaSource, KafkaSourceLimits, KafkaSourceStats, cdc_value, kafka_record,
    read_kafka_output,
};
#[path = "log.rs"]
mod log;
pub use log::{InputLog, LogLimits, LogRecovery};
#[path = "sink.rs"]
mod sink;
pub use sink::{CommitRecord, CommitSink, CrashMedium, MemorySink, QuarrySink, SinkRecovery};

use crate::{
    Budget, Error, Limits, Row, ZSet, identity,
    zset::{Accumulator, union},
};

/// Typed, total row expressions. Columns are validated when building the DAG.
#[derive(Clone, Debug)]
pub enum Predicate {
    Equal { column: usize, value: i64 },
    AtLeast { column: usize, value: i64 },
}
impl Predicate {
    fn column(&self) -> usize {
        match self {
            Self::Equal { column, .. } | Self::AtLeast { column, .. } => *column,
        }
    }
    fn test(&self, row: &Row) -> bool {
        match self {
            Self::Equal { column, value } => row[*column] == *value,
            Self::AtLeast { column, value } => row[*column] >= *value,
        }
    }
}
#[derive(Clone, Debug)]
pub enum Field {
    Column(usize),
    Constant(i64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeId {
    origin: u64,
    index: usize,
}
#[derive(Clone, Debug)]
enum Op {
    Input(usize),
    Filter(usize, Predicate),
    Map(usize, Vec<Field>),
    Union(usize, usize),
    Join {
        a: usize,
        b: usize,
        ka: usize,
        kb: usize,
    },
    /// Re-partition by `key`. The identity on one executor; in a cluster the
    /// delta is shuffled so that every row lives on `route(row[key])`.
    /// Inserted only by the cluster planner, never by [`CircuitBuilder`].
    Exchange {
        a: usize,
        key: usize,
    },
}
#[derive(Clone, Debug)]
struct Node {
    op: Op,
    width: usize,
    depth: usize,
}
/// Append-only typed DAG: sharing is explicit through NodeIds, never recursive trees.
pub struct CircuitBuilder {
    origin: u64,
    limits: Limits,
    nodes: Vec<Node>,
    inputs: Vec<usize>,
}
impl CircuitBuilder {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        Ok(Self {
            origin: identity()?,
            limits,
            nodes: Vec::new(),
            inputs: Vec::new(),
        })
    }
    fn get(&self, id: NodeId) -> Result<&Node, Error> {
        if id.origin != self.origin {
            return Err(Error::InvalidNode);
        }
        self.nodes.get(id.index).ok_or(Error::InvalidNode)
    }
    fn push(&mut self, op: Op, width: usize, depth: usize) -> Result<NodeId, Error> {
        if self.nodes.len() >= self.limits.max_nodes {
            return Err(Error::Limit("nodes"));
        }
        if width > self.limits.max_columns {
            return Err(Error::Limit("columns"));
        }
        if depth > self.limits.max_depth {
            return Err(Error::Limit("depth"));
        }
        let id = NodeId {
            origin: self.origin,
            index: self.nodes.len(),
        };
        self.nodes.push(Node { op, width, depth });
        Ok(id)
    }
    pub fn input(&mut self, width: usize) -> Result<NodeId, Error> {
        let slot = self.inputs.len();
        let id = self.push(Op::Input(slot), width, 1)?;
        self.inputs.push(width);
        Ok(id)
    }
    pub fn filter(&mut self, input: NodeId, pred: Predicate) -> Result<NodeId, Error> {
        let n = self.get(input)?;
        if pred.column() >= n.width {
            return Err(Error::Schema);
        }
        self.push(
            Op::Filter(input.index, pred),
            n.width,
            n.depth.checked_add(1).ok_or(Error::Overflow)?,
        )
    }
    pub fn map(&mut self, input: NodeId, fields: Vec<Field>) -> Result<NodeId, Error> {
        let n = self.get(input)?;
        if fields.len() > self.limits.max_columns {
            return Err(Error::Limit("columns"));
        }
        for field in &fields {
            if matches!(field, Field::Column(c) if *c >= n.width) {
                return Err(Error::Schema);
            }
        }
        let depth = n.depth.checked_add(1).ok_or(Error::Overflow)?;
        self.push(Op::Map(input.index, fields.clone()), fields.len(), depth)
    }
    pub fn union(&mut self, a: NodeId, b: NodeId) -> Result<NodeId, Error> {
        let (a_node, b_node) = (self.get(a)?, self.get(b)?);
        if a_node.width != b_node.width {
            return Err(Error::Schema);
        }
        self.push(
            Op::Union(a.index, b.index),
            a_node.width,
            a_node
                .depth
                .max(b_node.depth)
                .checked_add(1)
                .ok_or(Error::Overflow)?,
        )
    }
    /// Concatenates matching rows (including both join-key columns).
    pub fn equijoin(
        &mut self,
        a: NodeId,
        b: NodeId,
        ka: usize,
        kb: usize,
    ) -> Result<NodeId, Error> {
        let (na, nb) = (self.get(a)?, self.get(b)?);
        if ka >= na.width || kb >= nb.width {
            return Err(Error::Schema);
        }
        self.push(
            Op::Join {
                a: a.index,
                b: b.index,
                ka,
                kb,
            },
            na.width.checked_add(nb.width).ok_or(Error::Overflow)?,
            na.depth
                .max(nb.depth)
                .checked_add(1)
                .ok_or(Error::Overflow)?,
        )
    }
    pub fn finish(&self, output: NodeId) -> Result<Circuit, Error> {
        self.get(output)?;
        Circuit::assemble(
            self.nodes.clone(),
            self.inputs.clone(),
            output.index,
            self.limits,
        )
    }
}
#[derive(Clone, Debug)]
enum DOp {
    Delta(usize),
    Previous(usize),
    Apply(Op),
    Sum3(usize, usize, usize),
}
#[derive(Clone, Debug)]
struct DNode {
    op: DOp,
    depth: usize,
}
fn emit(diff: &mut Vec<DNode>, op: DOp, limits: Limits) -> Result<usize, Error> {
    if diff.len() >= limits.max_nodes {
        return Err(Error::Limit("differentiated nodes"));
    }
    let parent_depth = match &op {
        DOp::Delta(_) | DOp::Previous(_) => 0,
        DOp::Apply(Op::Filter(a, _) | Op::Map(a, _) | Op::Exchange { a, .. }) => diff[*a].depth,
        DOp::Apply(Op::Union(a, b) | Op::Join { a, b, .. }) => diff[*a].depth.max(diff[*b].depth),
        DOp::Sum3(a, b, c) => diff[*a].depth.max(diff[*b].depth).max(diff[*c].depth),
        DOp::Apply(Op::Input(_)) => return Err(Error::InvalidNode),
    };
    let depth = parent_depth.checked_add(1).ok_or(Error::Overflow)?;
    if depth > limits.max_depth {
        return Err(Error::Limit("differentiated depth"));
    }
    let id = diff.len();
    diff.push(DNode { op, depth });
    Ok(id)
}
#[derive(Clone, Debug)]
pub struct Circuit {
    nodes: Vec<Node>,
    inputs: Vec<usize>,
    output: usize,
    limits: Limits,
    diff: Vec<DNode>,
    delta: Vec<usize>,
}
impl Circuit {
    /// Differentiate an append-only node list (shared by the builder and the
    /// cluster planner).
    fn assemble(
        nodes: Vec<Node>,
        inputs: Vec<usize>,
        output: usize,
        limits: Limits,
    ) -> Result<Self, Error> {
        let mut diff = Vec::new();
        let mut delta: Vec<usize> = Vec::new();
        for node in &nodes {
            let id = match &node.op {
                Op::Input(slot) => emit(&mut diff, DOp::Delta(*slot), limits)?,
                Op::Filter(a, p) => emit(
                    &mut diff,
                    DOp::Apply(Op::Filter(delta[*a], p.clone())),
                    limits,
                )?,
                Op::Map(a, f) => {
                    emit(&mut diff, DOp::Apply(Op::Map(delta[*a], f.clone())), limits)?
                }
                Op::Union(a, b) => emit(
                    &mut diff,
                    DOp::Apply(Op::Union(delta[*a], delta[*b])),
                    limits,
                )?,
                // D(exchange) = exchange(D): it is linear.
                Op::Exchange { a, key } => emit(
                    &mut diff,
                    DOp::Apply(Op::Exchange {
                        a: delta[*a],
                        key: *key,
                    }),
                    limits,
                )?,
                Op::Join { a, b, ka, kb } => {
                    // D(join) = join(dA,prevB) + join(prevA,dB) + join(dA,dB).
                    // References share dA/dB even in deeply nested/shared joins.
                    let pa = emit(&mut diff, DOp::Previous(*a), limits)?;
                    let pb = emit(&mut diff, DOp::Previous(*b), limits)?;
                    let t1 = emit(
                        &mut diff,
                        DOp::Apply(Op::Join {
                            a: delta[*a],
                            b: pb,
                            ka: *ka,
                            kb: *kb,
                        }),
                        limits,
                    )?;
                    let t2 = emit(
                        &mut diff,
                        DOp::Apply(Op::Join {
                            a: pa,
                            b: delta[*b],
                            ka: *ka,
                            kb: *kb,
                        }),
                        limits,
                    )?;
                    let t3 = emit(
                        &mut diff,
                        DOp::Apply(Op::Join {
                            a: delta[*a],
                            b: delta[*b],
                            ka: *ka,
                            kb: *kb,
                        }),
                        limits,
                    )?;
                    emit(&mut diff, DOp::Sum3(t1, t2, t3), limits)?
                }
            };
            delta.push(id);
        }
        Ok(Circuit {
            nodes,
            inputs,
            output,
            limits,
            diff,
            delta,
        })
    }

    /// Plan this circuit for a cluster partitioned by `cols` (one key column
    /// per input): propagate partition keys and insert an [`Op::Exchange`]
    /// before every join input whose key is not its join key. Returns the
    /// planned circuit and every planned node's partition key (`None` when
    /// rows are not placed by any column).
    pub(crate) fn plan(&self, cols: &[usize]) -> Result<(Circuit, Vec<Option<usize>>), Error> {
        if cols.len() != self.inputs.len() {
            return Err(Error::Partition("partition column count"));
        }
        for (col, width) in cols.iter().zip(&self.inputs) {
            if col >= width {
                return Err(Error::Partition("partition column out of range"));
            }
        }
        let mut nodes: Vec<Node> = Vec::with_capacity(self.nodes.len());
        let mut keys: Vec<Option<usize>> = Vec::with_capacity(self.nodes.len());
        let mut map: Vec<usize> = Vec::with_capacity(self.nodes.len());
        let limits = self.limits;
        fn push(
            nodes: &mut Vec<Node>,
            keys: &mut Vec<Option<usize>>,
            limits: Limits,
            op: Op,
            width: usize,
            key: Option<usize>,
        ) -> Result<usize, Error> {
            if nodes.len() >= limits.max_nodes {
                return Err(Error::Limit("nodes"));
            }
            let depth = match &op {
                Op::Input(_) => 0,
                Op::Filter(a, _) | Op::Map(a, _) | Op::Exchange { a, .. } => nodes[*a].depth,
                Op::Union(a, b) | Op::Join { a, b, .. } => nodes[*a].depth.max(nodes[*b].depth),
            }
            .checked_add(1)
            .ok_or(Error::Overflow)?;
            if depth > limits.max_depth {
                return Err(Error::Limit("depth"));
            }
            nodes.push(Node { op, width, depth });
            keys.push(key);
            Ok(nodes.len() - 1)
        }
        for n in &self.nodes {
            let id = match &n.op {
                Op::Input(slot) => push(
                    &mut nodes,
                    &mut keys,
                    limits,
                    Op::Input(*slot),
                    n.width,
                    Some(cols[*slot]),
                )?,
                Op::Filter(a, p) => {
                    let key = keys[map[*a]];
                    push(
                        &mut nodes,
                        &mut keys,
                        limits,
                        Op::Filter(map[*a], p.clone()),
                        n.width,
                        key,
                    )?
                }
                Op::Map(a, fields) => {
                    let key = keys[map[*a]].and_then(|k| {
                        fields
                            .iter()
                            .position(|f| matches!(f, Field::Column(c) if *c == k))
                    });
                    push(
                        &mut nodes,
                        &mut keys,
                        limits,
                        Op::Map(map[*a], fields.clone()),
                        n.width,
                        key,
                    )?
                }
                Op::Union(a, b) => {
                    let (a, b) = (map[*a], map[*b]);
                    let key = keys[a].filter(|k| keys[b] == Some(*k));
                    push(&mut nodes, &mut keys, limits, Op::Union(a, b), n.width, key)?
                }
                Op::Exchange { .. } => return Err(Error::InvalidNode),
                Op::Join { a, b, ka, kb } => {
                    let mut side = |side: usize, k: usize| -> Result<usize, Error> {
                        if keys[side] == Some(k) {
                            return Ok(side);
                        }
                        let width = nodes[side].width;
                        push(
                            &mut nodes,
                            &mut keys,
                            limits,
                            Op::Exchange { a: side, key: k },
                            width,
                            Some(k),
                        )
                    };
                    let a = side(map[*a], *ka)?;
                    let b = side(map[*b], *kb)?;
                    push(
                        &mut nodes,
                        &mut keys,
                        limits,
                        Op::Join {
                            a,
                            b,
                            ka: *ka,
                            kb: *kb,
                        },
                        n.width,
                        Some(*ka),
                    )?
                }
            };
            map.push(id);
        }
        let planned = Circuit::assemble(nodes, self.inputs.clone(), map[self.output], limits)?;
        Ok((planned, keys))
    }

    /// Differentiated index and row width of every exchange.
    pub(crate) fn exchange_widths(&self) -> std::collections::BTreeMap<usize, usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| matches!(n.op, Op::Exchange { .. }))
            .map(|(i, n)| (self.delta[i], n.width))
            .collect()
    }
    /// Exchange nodes inserted by [`Circuit::plan`].
    pub(crate) fn exchange_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n.op, Op::Exchange { .. }))
            .count()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
    pub fn differentiated_node_count(&self) -> usize {
        self.diff.len()
    }
    /// Independent full batch evaluator over the original relational DAG.
    pub fn batch(&self, inputs: &[ZSet]) -> Result<ZSet, Error> {
        self.validate_inputs(inputs)?;
        let mut budget = Budget::new(self.limits);
        let mut values = Vec::new();
        for node in &self.nodes {
            budget.charge(1)?;
            let value = match &node.op {
                Op::Input(slot) => copy(&inputs[*slot], self.limits, &mut budget)?,
                op => apply(op, &values, self.limits, &mut budget)?,
            };
            values.push(value);
        }
        let out = values.swap_remove(self.output);
        self.check_output(out)
    }
    fn check_output(&self, out: ZSet) -> Result<ZSet, Error> {
        if out.len() > self.limits.max_output {
            return Err(Error::Limit("output"));
        }
        Ok(out)
    }
    fn validate_inputs(&self, inputs: &[ZSet]) -> Result<(), Error> {
        if inputs.len() != self.inputs.len() {
            return Err(Error::InputCount);
        }
        for (input, width) in inputs.iter().zip(&self.inputs) {
            if input.width != *width {
                return Err(Error::Schema);
            }
            if input.len() > self.limits.max_rows {
                return Err(Error::Limit("rows"));
            }
        }
        Ok(())
    }
    pub fn executor(self) -> Executor {
        let prior = self
            .nodes
            .iter()
            .map(|n| ZSet {
                width: n.width,
                rows: Default::default(),
            })
            .collect();
        let versions = vec![0; self.nodes.len()];
        Executor {
            circuit: self,
            prior,
            ticks: 0,
            source: None,
            lineage: fresh_lineage(),
            versions,
        }
    }
}
/// One canonical delta per declared input, in declaration order.
#[derive(Clone, Debug)]
pub struct TickInput {
    pub deltas: Vec<ZSet>,
}
#[derive(Debug)]
pub struct Executor {
    circuit: Circuit,
    prior: Vec<ZSet>,
    ticks: u64,
    source: Option<cdc::SourceState>,
    /// Process-local identity of this state history; replaced on every restore,
    /// so an incremental checkpointer never trusts change counters across them.
    /// 0 (identity exhausted) disables incremental reuse.
    lineage: u64,
    /// Per-original-node change counters: bumped when a tick's delta for that
    /// node's integral is nonzero. Durable checkpoints rewrite only changed nodes.
    versions: Vec<u64>,
}
fn fresh_lineage() -> u64 {
    identity().unwrap_or(0)
}
impl Executor {
    /// The circuit this executor runs.
    pub fn circuit(&self) -> &Circuit {
        &self.circuit
    }
    pub fn ticks(&self) -> u64 {
        self.ticks
    }
    pub fn materialized(&self) -> &ZSet {
        &self.prior[self.circuit.output]
    }
    /// Commits only after every differentiated operator and integral passes.
    /// Borrowed input remains caller-owned on both success and failure.
    pub fn tick(&mut self, input: &TickInput) -> Result<ZSet, Error> {
        if self.source.is_some() {
            return Err(Error::SourceMode);
        }
        self.tick_inner(input)
    }
    fn tick_inner(&mut self, input: &TickInput) -> Result<ZSet, Error> {
        let mut staged = self.stage(input)?;
        if self.run_stage(&mut staged, false)?.is_some() {
            return Err(Error::InvalidNode);
        }
        self.finish_stage(staged)
    }
    /// Begin a tick whose exchanges may be served by a cluster shuffle.
    pub(crate) fn stage(&self, input: &TickInput) -> Result<Staged, Error> {
        let c = &self.circuit;
        c.validate_inputs(&input.deltas)?;
        let next_tick = self.ticks.checked_add(1).ok_or(Error::Overflow)?;
        Ok(Staged {
            deltas: input.deltas.clone(),
            values: Vec::with_capacity(c.diff.len()),
            budget: Budget::new(c.limits),
            next_tick,
        })
    }
    /// Evaluate differentiated nodes in order. With `remote`, stop before an
    /// exchange and return `(node, key)`: the caller scatters
    /// `staged.local(node)` by `key` and calls [`Executor::provide`] with
    /// the gathered delta. Without it, an exchange is the identity.
    pub(crate) fn run_stage(
        &self,
        st: &mut Staged,
        remote: bool,
    ) -> Result<Option<(usize, usize)>, Error> {
        let c = &self.circuit;
        while st.values.len() < c.diff.len() {
            let node = &c.diff[st.values.len()];
            st.budget.charge(1)?;
            let value = match &node.op {
                DOp::Delta(slot) => copy(&st.deltas[*slot], c.limits, &mut st.budget)?,
                DOp::Previous(node) => copy(&self.prior[*node], c.limits, &mut st.budget)?,
                DOp::Apply(Op::Exchange { a, key }) if remote => {
                    return Ok(Some((*a, *key)));
                }
                DOp::Apply(op) => apply(op, &st.values, c.limits, &mut st.budget)?,
                DOp::Sum3(a, b, d) => union(
                    &[&st.values[*a], &st.values[*b], &st.values[*d]],
                    c.limits,
                    &mut st.budget,
                )?,
            };
            st.values.push(value);
        }
        Ok(None)
    }
    /// Supply the gathered value of the exchange `run_stage` stopped at.
    pub(crate) fn provide(&self, st: &mut Staged, gathered: ZSet) -> Result<(), Error> {
        let c = &self.circuit;
        let node = c.diff.get(st.values.len()).ok_or(Error::InvalidNode)?;
        let DOp::Apply(Op::Exchange { a, .. }) = node.op else {
            return Err(Error::InvalidNode);
        };
        if gathered.width != st.values[a].width {
            return Err(Error::Schema);
        }
        if gathered.len() > c.limits.max_rows {
            return Err(Error::Limit("rows"));
        }
        st.budget.charge(gathered.len())?;
        st.values.push(gathered);
        Ok(())
    }
    pub(crate) fn finish_stage(&mut self, mut st: Staged) -> Result<ZSet, Error> {
        let c = &self.circuit;
        if st.values.len() != c.diff.len() {
            return Err(Error::InvalidNode);
        }
        let values = &st.values;
        let budget = &mut st.budget;
        let mut versions = self.versions.clone();
        for (id, version) in versions.iter_mut().enumerate() {
            if !values[c.delta[id]].is_empty() {
                *version = version.checked_add(1).ok_or(Error::Overflow)?;
            }
        }
        let mut next = Vec::new();
        for (id, prior) in self.prior.iter().enumerate() {
            next.push(union(&[prior, &values[c.delta[id]]], c.limits, budget)?);
        }
        let out = c.check_output(copy(&values[c.delta[c.output]], c.limits, budget)?)?;
        // Bound the materialized output too, before any state mutation.
        if next[c.output].len() > c.limits.max_output {
            return Err(Error::Limit("output"));
        }
        self.prior = next;
        self.ticks = st.next_tick;
        self.versions = versions;
        Ok(out)
    }
    /// Build an executor from integrals computed elsewhere (cluster
    /// rescaling). The state is revalidated as a checkpoint would be.
    pub(crate) fn from_state(
        circuit: Circuit,
        prior: Vec<ZSet>,
        ticks: u64,
        limits: CheckpointLimits,
    ) -> Result<Self, Error> {
        let mut budget = limits.budget(&circuit);
        checkpoint::validate_state(&circuit, &prior, ticks, None, limits, &mut budget)?;
        let versions = vec![0; prior.len()];
        Ok(Executor {
            circuit,
            prior,
            ticks,
            source: None,
            lineage: fresh_lineage(),
            versions,
        })
    }
    /// Integrals of every original node, in node order.
    pub(crate) fn integrals(&self) -> &[ZSet] {
        &self.prior
    }
}
/// A tick in progress (see [`Executor::run_stage`]).
pub(crate) struct Staged {
    deltas: Vec<ZSet>,
    values: Vec<ZSet>,
    budget: Budget,
    next_tick: u64,
}
impl Staged {
    /// Value of an already evaluated differentiated node.
    pub(crate) fn local(&self, node: usize) -> Result<&ZSet, Error> {
        self.values.get(node).ok_or(Error::InvalidNode)
    }
    /// Index of the next differentiated node to evaluate.
    pub(crate) fn position(&self) -> usize {
        self.values.len()
    }
}
fn copy(set: &ZSet, limits: Limits, budget: &mut Budget) -> Result<ZSet, Error> {
    union(&[set], limits, budget)
}
fn apply(op: &Op, values: &[ZSet], limits: Limits, budget: &mut Budget) -> Result<ZSet, Error> {
    match op {
        Op::Input(_) => Err(Error::InvalidNode),
        Op::Union(a, b) => union(&[&values[*a], &values[*b]], limits, budget),
        Op::Exchange { a, .. } => copy(&values[*a], limits, budget),
        Op::Filter(a, p) => {
            let mut out = Accumulator::new(values[*a].width, limits)?;
            for (row, w) in values[*a].entries() {
                budget.charge(1)?;
                if p.test(row) {
                    out.add(row, i128::from(w), budget)?;
                }
            }
            out.finish()
        }
        Op::Map(a, fields) => {
            let mut out = Accumulator::new(fields.len(), limits)?;
            for (row, w) in values[*a].entries() {
                budget.charge(fields.len())?;
                let mapped = fields
                    .iter()
                    .map(|f| match f {
                        Field::Column(c) => row[*c],
                        Field::Constant(v) => *v,
                    })
                    .collect();
                out.add(&mapped, i128::from(w), budget)?;
            }
            out.finish()
        }
        Op::Join { a, b, ka, kb } => {
            let (a, b) = (&values[*a], &values[*b]);
            let product = a.len().checked_mul(b.len()).ok_or(Error::Overflow)?;
            if product > limits.max_product {
                return Err(Error::Limit("join product"));
            }
            budget.charge(product)?;
            let mut out =
                Accumulator::new(a.width.checked_add(b.width).ok_or(Error::Overflow)?, limits)?;
            for (ar, aw) in a.entries() {
                for (br, bw) in b.entries() {
                    if ar[*ka] == br[*kb] {
                        budget.charge(ar.len().checked_add(br.len()).ok_or(Error::Overflow)?)?;
                        let mut row = ar.clone();
                        row.extend_from_slice(br);
                        out.add(
                            &row,
                            i128::from(aw)
                                .checked_mul(i128::from(bw))
                                .ok_or(Error::Overflow)?,
                            budget,
                        )?;
                    }
                }
            }
            out.finish()
        }
    }
}
