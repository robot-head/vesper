#[path = "cdc.rs"]
mod cdc;
#[path = "checkpoint.rs"]
mod checkpoint;
pub use cdc::{CdcHeader, CdcPolicy, Ingested, SourceBinding, WeightPolicy};
pub use checkpoint::CheckpointLimits;

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
        let mut diff = Vec::new();
        let mut delta = Vec::new();
        for node in &self.nodes {
            let id = match &node.op {
                Op::Input(slot) => emit(&mut diff, DOp::Delta(*slot), self.limits)?,
                Op::Filter(a, p) => emit(
                    &mut diff,
                    DOp::Apply(Op::Filter(delta[*a], p.clone())),
                    self.limits,
                )?,
                Op::Map(a, f) => emit(
                    &mut diff,
                    DOp::Apply(Op::Map(delta[*a], f.clone())),
                    self.limits,
                )?,
                Op::Union(a, b) => emit(
                    &mut diff,
                    DOp::Apply(Op::Union(delta[*a], delta[*b])),
                    self.limits,
                )?,
                Op::Join { a, b, ka, kb } => {
                    // D(join) = join(dA,prevB) + join(prevA,dB) + join(dA,dB).
                    // References share dA/dB even in deeply nested/shared joins.
                    let pa = emit(&mut diff, DOp::Previous(*a), self.limits)?;
                    let pb = emit(&mut diff, DOp::Previous(*b), self.limits)?;
                    let t1 = emit(
                        &mut diff,
                        DOp::Apply(Op::Join {
                            a: delta[*a],
                            b: pb,
                            ka: *ka,
                            kb: *kb,
                        }),
                        self.limits,
                    )?;
                    let t2 = emit(
                        &mut diff,
                        DOp::Apply(Op::Join {
                            a: pa,
                            b: delta[*b],
                            ka: *ka,
                            kb: *kb,
                        }),
                        self.limits,
                    )?;
                    let t3 = emit(
                        &mut diff,
                        DOp::Apply(Op::Join {
                            a: delta[*a],
                            b: delta[*b],
                            ka: *ka,
                            kb: *kb,
                        }),
                        self.limits,
                    )?;
                    emit(&mut diff, DOp::Sum3(t1, t2, t3), self.limits)?
                }
            };
            delta.push(id);
        }
        Ok(Circuit {
            nodes: self.nodes.clone(),
            inputs: self.inputs.clone(),
            output: output.index,
            limits: self.limits,
            diff,
            delta,
        })
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
        DOp::Apply(Op::Filter(a, _) | Op::Map(a, _)) => diff[*a].depth,
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
        Executor {
            circuit: self,
            prior,
            ticks: 0,
            source: None,
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
}
impl Executor {
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
        let c = &self.circuit;
        c.validate_inputs(&input.deltas)?;
        let next_tick = self.ticks.checked_add(1).ok_or(Error::Overflow)?;
        let mut budget = Budget::new(c.limits);
        let mut values = Vec::new();
        for node in &c.diff {
            budget.charge(1)?;
            let value = match &node.op {
                DOp::Delta(slot) => copy(&input.deltas[*slot], c.limits, &mut budget)?,
                DOp::Previous(node) => copy(&self.prior[*node], c.limits, &mut budget)?,
                DOp::Apply(op) => apply(op, &values, c.limits, &mut budget)?,
                DOp::Sum3(a, b, d) => union(
                    &[&values[*a], &values[*b], &values[*d]],
                    c.limits,
                    &mut budget,
                )?,
            };
            values.push(value);
        }
        let mut next = Vec::new();
        for (id, prior) in self.prior.iter().enumerate() {
            next.push(union(
                &[prior, &values[c.delta[id]]],
                c.limits,
                &mut budget,
            )?);
        }
        let out = c.check_output(copy(&values[c.delta[c.output]], c.limits, &mut budget)?)?;
        // Bound the materialized output too, before any state mutation.
        if next[c.output].len() > c.limits.max_output {
            return Err(Error::Limit("output"));
        }
        self.prior = next;
        self.ticks = next_tick;
        Ok(out)
    }
}
fn copy(set: &ZSet, limits: Limits, budget: &mut Budget) -> Result<ZSet, Error> {
    union(&[set], limits, budget)
}
fn apply(op: &Op, values: &[ZSet], limits: Limits, budget: &mut Budget) -> Result<ZSet, Error> {
    match op {
        Op::Input(_) => Err(Error::InvalidNode),
        Op::Union(a, b) => union(&[&values[*a], &values[*b]], limits, budget),
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
