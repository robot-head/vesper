//! Delay-compensated DSP/control DAG compiler (design §32.1 item 4, §32.5).
//!
//! A graph is a set of nodes, each with an intrinsic latency (in samples) and
//! an ordered list of inputs. A forward input `Input::Forward(src)` is a
//! same-block dependency; the forward edges must form a DAG. A feedback input
//! `Input::Feedback { from, delay }` is an explicit delayed edge: the value
//! read at sample `t` is `from`'s output at `t - delay`. Cycles are allowed
//! only through feedback edges; a cycle of forward edges is rejected with
//! [`PdcError::CycleWithoutDelay`], and a feedback edge with delay 0 is
//! rejected with [`PdcError::ZeroDelayFeedback`].
//!
//! Plugin delay compensation (PDC): for every node, `arrival` is the maximum
//! output latency over its forward inputs, each forward input gets a
//! compensation delay `arrival - latency(src)`, and the node's output latency
//! is `arrival + intrinsic`. Every forward path from a node `s` to a node `t`
//! therefore carries the same total latency `latency(t) - latency(s)`
//! (`lean/ResonanceSpsc.lean`, `pdc_paths_equal`). Feedback edges are not
//! aligned: their delay is the explicit one the author wrote.
//!
//! Buffers: each node's output and each compensated input needs a block
//! buffer. A linear scan over the schedule reuses a buffer once its last
//! reader has run; sink outputs (no forward consumer) stay live to the end so
//! callers can read them.
//!
//! The executor [`CompiledGraph`] preallocates every buffer and delay line at
//! compile time and processes blocks without allocating. Block execution
//! requires every feedback delay to be at least the block length
//! ([`PdcError::FeedbackShorterThanBlock`]); shorter feedback needs
//! per-sample evaluation, which this slice does not provide.

use std::collections::BTreeSet;
use std::fmt;

/// Node identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// One input of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Same-block dependency on another node's output.
    Forward(NodeId),
    /// Explicitly delayed edge (may close a cycle).
    Feedback {
        /// Source node.
        from: NodeId,
        /// Delay in samples (at least 1, and at least the block length to run).
        delay: u32,
    },
}

/// One node of the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeSpec {
    /// Identifier, unique in the graph.
    pub id: NodeId,
    /// Intrinsic latency in samples.
    pub latency: u32,
    /// Ordered inputs.
    pub inputs: Vec<Input>,
}

/// Where a scheduled node reads an input from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSource {
    /// Forward input read from `buffer`, delayed by `compensation` samples
    /// (via delay line `line` when `compensation > 0`).
    Forward {
        /// Source node.
        from: NodeId,
        /// Inserted PDC delay.
        compensation: u32,
        /// Buffer holding the source output this block.
        buffer: usize,
        /// Compensation delay line and its scratch buffer, when delayed.
        line: Option<(usize, usize)>,
    },
    /// Feedback input read from delay line `line` into scratch `buffer`.
    Feedback {
        /// Source node.
        from: NodeId,
        /// Explicit delay.
        delay: u32,
        /// Feedback delay line.
        line: usize,
        /// Scratch buffer the delayed block is read into.
        buffer: usize,
    },
}

impl InputSource {
    /// The buffer the node reads this input from.
    pub fn read_buffer(&self) -> usize {
        match *self {
            InputSource::Forward {
                buffer, line: None, ..
            } => buffer,
            InputSource::Forward {
                line: Some((_, scratch)),
                ..
            } => scratch,
            InputSource::Feedback { buffer, .. } => buffer,
        }
    }
}

/// One step of the schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledOp {
    /// The node.
    pub node: NodeId,
    /// Index of the node in the input spec slice.
    pub spec_index: usize,
    /// Inputs, in the node's declared order.
    pub inputs: Vec<InputSource>,
    /// Buffer the node writes its output to.
    pub output: usize,
    /// Feedback lines this node's output feeds (written after it runs).
    pub feeds: Vec<usize>,
    /// Arrival latency (max forward input latency).
    pub arrival_latency: u64,
    /// Output latency (`arrival + intrinsic`).
    pub output_latency: u64,
}

/// A delay line in the compiled schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineSpec {
    /// Source node whose output enters the line.
    pub from: NodeId,
    /// Delay in samples.
    pub delay: u32,
    /// `true` for a feedback line, `false` for PDC compensation.
    pub feedback: bool,
}

/// The compiled schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    /// Topological order (lowest spec index first among ready nodes).
    pub ops: Vec<ScheduledOp>,
    /// Delay lines (compensation and feedback).
    pub lines: Vec<LineSpec>,
    /// Number of distinct block buffers after liveness reuse.
    pub buffer_count: usize,
}

impl Schedule {
    /// The scheduled op for `node`.
    pub fn op(&self, node: NodeId) -> Option<&ScheduledOp> {
        self.ops.iter().find(|o| o.node == node)
    }
}

/// Graph compilation errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PdcError {
    /// Two nodes share an id.
    DuplicateNode(NodeId),
    /// An input names a node that does not exist.
    UnknownNode(NodeId),
    /// The forward edges contain a cycle; these nodes are on or behind it.
    CycleWithoutDelay {
        /// Nodes that could not be scheduled.
        nodes: Vec<NodeId>,
    },
    /// A feedback edge has delay 0.
    ZeroDelayFeedback {
        /// Source of the edge.
        from: NodeId,
        /// Destination of the edge.
        to: NodeId,
    },
    /// Block execution needs feedback delay >= block length.
    FeedbackShorterThanBlock {
        /// Source of the edge.
        from: NodeId,
        /// Destination of the edge.
        to: NodeId,
        /// The edge delay.
        delay: u32,
        /// The block length.
        block: usize,
    },
    /// Block length 0, or a delay line too long to allocate.
    BadBlockLength(usize),
    /// Latency sum overflowed u64, or a compensation exceeds u32.
    LatencyOverflow(NodeId),
    /// A delay line would exceed [`MAX_DELAY_SAMPLES`].
    DelayTooLong(u32),
    /// Processor count does not match node count.
    ProcessorCount {
        /// Nodes in the spec.
        nodes: usize,
        /// Processors supplied.
        processors: usize,
    },
}

impl fmt::Display for PdcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PdcError {}

/// Compile `nodes` into a delay-compensated schedule.
pub fn compile_schedule(nodes: &[NodeSpec]) -> Result<Schedule, PdcError> {
    let n = nodes.len();
    let mut index_of = std::collections::BTreeMap::new();
    for (i, node) in nodes.iter().enumerate() {
        if index_of.insert(node.id, i).is_some() {
            return Err(PdcError::DuplicateNode(node.id));
        }
    }
    let idx = |id: NodeId| index_of.get(&id).copied().ok_or(PdcError::UnknownNode(id));

    // Forward in-degrees and dependents.
    let mut indegree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, node) in nodes.iter().enumerate() {
        for input in &node.inputs {
            match *input {
                Input::Forward(src) => {
                    let s = idx(src)?;
                    indegree[i] += 1;
                    dependents[s].push(i);
                }
                Input::Feedback { from, delay } => {
                    idx(from)?;
                    if delay == 0 {
                        return Err(PdcError::ZeroDelayFeedback { from, to: node.id });
                    }
                }
            }
        }
    }

    // Kahn's algorithm, deterministic: lowest spec index first.
    let mut ready: BTreeSet<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(i) = ready.pop_first() {
        order.push(i);
        for &d in &dependents[i] {
            indegree[d] -= 1;
            if indegree[d] == 0 {
                ready.insert(d);
            }
        }
    }
    if order.len() != n {
        let mut stuck: Vec<NodeId> = (0..n)
            .filter(|&i| indegree[i] > 0)
            .map(|i| nodes[i].id)
            .collect();
        stuck.sort();
        return Err(PdcError::CycleWithoutDelay { nodes: stuck });
    }

    // Latencies.
    let mut out_lat = vec![0u64; n];
    let mut arrival = vec![0u64; n];
    for &i in &order {
        let node = &nodes[i];
        let mut a = 0u64;
        for input in &node.inputs {
            if let Input::Forward(src) = *input {
                a = a.max(out_lat[idx(src)?]);
            }
        }
        arrival[i] = a;
        out_lat[i] = a
            .checked_add(u64::from(node.latency))
            .ok_or(PdcError::LatencyOverflow(node.id))?;
    }

    // Position of each node in the order, and last forward reader position.
    let mut pos = vec![0usize; n];
    for (p, &i) in order.iter().enumerate() {
        pos[i] = p;
    }
    let mut last_read = vec![None::<usize>; n];
    for (i, node) in nodes.iter().enumerate() {
        for input in &node.inputs {
            if let Input::Forward(src) = *input {
                let s = idx(src)?;
                last_read[s] = Some(last_read[s].map_or(pos[i], |p: usize| p.max(pos[i])));
            }
        }
    }

    // Delay lines: one per compensated forward input and per feedback input.
    let mut lines = Vec::new();
    let mut feeds: Vec<Vec<usize>> = vec![Vec::new(); n];

    // Linear-scan buffer coloring.
    let mut free: BTreeSet<usize> = BTreeSet::new();
    let mut buffer_count = 0usize;
    let mut alloc = |free: &mut BTreeSet<usize>| {
        free.pop_first().unwrap_or_else(|| {
            buffer_count += 1;
            buffer_count - 1
        })
    };
    let mut out_buf = vec![usize::MAX; n];
    // Sources whose last reader is position p.
    let mut dying_at: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (s, lr) in last_read.iter().enumerate() {
        if let Some(p) = lr {
            dying_at[*p].push(s);
        }
    }

    let mut ops = Vec::with_capacity(n);
    for (p, &i) in order.iter().enumerate() {
        let node = &nodes[i];
        let mut inputs = Vec::with_capacity(node.inputs.len());
        let mut temps = Vec::new();
        for input in &node.inputs {
            match *input {
                Input::Forward(src) => {
                    let s = idx(src)?;
                    let comp64 = arrival[i] - out_lat[s];
                    let compensation =
                        u32::try_from(comp64).map_err(|_| PdcError::LatencyOverflow(node.id))?;
                    let line = if compensation > 0 {
                        let scratch = alloc(&mut free);
                        temps.push(scratch);
                        lines.push(LineSpec {
                            from: src,
                            delay: compensation,
                            feedback: false,
                        });
                        Some((lines.len() - 1, scratch))
                    } else {
                        None
                    };
                    inputs.push(InputSource::Forward {
                        from: src,
                        compensation,
                        buffer: out_buf[s],
                        line,
                    });
                }
                Input::Feedback { from, delay } => {
                    let s = idx(from)?;
                    let scratch = alloc(&mut free);
                    temps.push(scratch);
                    lines.push(LineSpec {
                        from,
                        delay,
                        feedback: true,
                    });
                    let line = lines.len() - 1;
                    feeds[s].push(line);
                    inputs.push(InputSource::Feedback {
                        from,
                        delay,
                        line,
                        buffer: scratch,
                    });
                }
            }
        }
        let output = alloc(&mut free);
        out_buf[i] = output;
        for s in std::mem::take(&mut dying_at[p]) {
            free.insert(out_buf[s]);
        }
        for t in temps {
            free.insert(t);
        }
        ops.push(ScheduledOp {
            node: node.id,
            spec_index: i,
            inputs,
            output,
            feeds: Vec::new(),
            arrival_latency: arrival[i],
            output_latency: out_lat[i],
        });
    }
    for op in &mut ops {
        op.feeds = std::mem::take(&mut feeds[op.spec_index]);
    }
    Ok(Schedule {
        ops,
        lines,
        buffer_count,
    })
}

/// Longest delay line the executor allocates (2^24 samples, about 5.8
/// minutes at 48 kHz). Longer compensation or feedback is a typed error.
pub const MAX_DELAY_SAMPLES: u32 = 1 << 24;

/// A block delay line of fixed delay (see the module docs). Capacity is
/// `delay + block`, so a block write and a block read of the same block
/// never overlap regardless of order.
#[derive(Clone, Debug)]
pub struct DelayLine {
    buf: Vec<f32>,
    delay: usize,
    head: usize,
}

impl DelayLine {
    /// A silent line of `delay` samples for blocks of `block` samples.
    pub fn new(delay: u32, block: usize) -> Result<Self, PdcError> {
        if delay > MAX_DELAY_SAMPLES {
            return Err(PdcError::DelayTooLong(delay));
        }
        if block > MAX_DELAY_SAMPLES as usize {
            return Err(PdcError::BadBlockLength(block));
        }
        let delay = delay as usize;
        let len = delay
            .checked_add(block)
            .filter(|_| block > 0)
            .ok_or(PdcError::BadBlockLength(block))?;
        Ok(DelayLine {
            buf: vec![0.0; len],
            delay,
            head: 0,
        })
    }

    /// Write this block's input samples.
    pub fn write(&mut self, input: &[f32]) {
        let len = self.buf.len();
        for (k, &x) in input.iter().enumerate() {
            self.buf[(self.head + k) % len] = x;
        }
    }

    /// Read this block's delayed samples.
    pub fn read(&self, out: &mut [f32]) {
        let len = self.buf.len();
        let start = self.head + len - self.delay;
        for (k, y) in out.iter_mut().enumerate() {
            *y = self.buf[(start + k) % len];
        }
    }

    /// Advance by one block.
    pub fn advance(&mut self, block: usize) {
        self.head = (self.head + block) % self.buf.len();
    }
}

/// A node's processing function. Implementations used on the realtime
/// thread must not allocate, lock or block.
pub trait DspProcessor: Send {
    /// Process one block: `inputs.get(k)` is the k-th declared input, already
    /// delay-compensated. `output` has the block length.
    fn process(&mut self, inputs: &Inputs<'_>, output: &mut [f32]);
}

/// Read-only view of a node's inputs for one block.
pub struct Inputs<'a> {
    buffers: &'a [Vec<f32>],
    which: &'a [InputSource],
}

impl Inputs<'_> {
    /// Number of inputs.
    pub fn len(&self) -> usize {
        self.which.len()
    }

    /// `true` when the node has no inputs.
    pub fn is_empty(&self) -> bool {
        self.which.is_empty()
    }

    /// The k-th input block.
    pub fn get(&self, k: usize) -> &[f32] {
        &self.buffers[self.which[k].read_buffer()]
    }
}

/// A compiled graph with preallocated buffers, ready to run blocks.
pub struct CompiledGraph {
    schedule: Schedule,
    processors: Vec<Box<dyn DspProcessor>>,
    buffers: Vec<Vec<f32>>,
    lines: Vec<DelayLine>,
    block: usize,
}

impl CompiledGraph {
    /// Compile `nodes` with one processor per node (same order) for blocks
    /// of `block` samples.
    pub fn new(
        nodes: &[NodeSpec],
        processors: Vec<Box<dyn DspProcessor>>,
        block: usize,
    ) -> Result<Self, PdcError> {
        if block == 0 || block > MAX_DELAY_SAMPLES as usize {
            return Err(PdcError::BadBlockLength(block));
        }
        if processors.len() != nodes.len() {
            return Err(PdcError::ProcessorCount {
                nodes: nodes.len(),
                processors: processors.len(),
            });
        }
        let schedule = compile_schedule(nodes)?;
        for node in nodes {
            for input in &node.inputs {
                if let Input::Feedback { from, delay } = *input
                    && (delay as usize) < block
                {
                    return Err(PdcError::FeedbackShorterThanBlock {
                        from,
                        to: node.id,
                        delay,
                        block,
                    });
                }
            }
        }
        let lines = schedule
            .lines
            .iter()
            .map(|l| DelayLine::new(l.delay, block))
            .collect::<Result<Vec<_>, _>>()?;
        let buffers = vec![vec![0.0f32; block]; schedule.buffer_count];
        Ok(CompiledGraph {
            schedule,
            processors,
            buffers,
            lines,
            block,
        })
    }

    /// The schedule.
    pub fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    /// Block length.
    pub fn block(&self) -> usize {
        self.block
    }

    /// Run one block. Does not allocate.
    pub fn process_block(&mut self) {
        for op in &self.schedule.ops {
            for input in &op.inputs {
                match *input {
                    InputSource::Forward {
                        buffer,
                        line: Some((line, scratch)),
                        ..
                    } => {
                        self.lines[line].write(&self.buffers[buffer]);
                        let mut tmp = std::mem::take(&mut self.buffers[scratch]);
                        self.lines[line].read(&mut tmp);
                        self.buffers[scratch] = tmp;
                    }
                    InputSource::Feedback { line, buffer, .. } => {
                        let mut tmp = std::mem::take(&mut self.buffers[buffer]);
                        self.lines[line].read(&mut tmp);
                        self.buffers[buffer] = tmp;
                    }
                    InputSource::Forward { line: None, .. } => {}
                }
            }
            let mut out = std::mem::take(&mut self.buffers[op.output]);
            let inputs = Inputs {
                buffers: &self.buffers,
                which: &op.inputs,
            };
            self.processors[op.spec_index].process(&inputs, &mut out);
            for &line in &op.feeds {
                self.lines[line].write(&out);
            }
            self.buffers[op.output] = out;
        }
        for line in &mut self.lines {
            line.advance(self.block);
        }
    }

    /// Output block of `node` from the last [`process_block`](Self::process_block).
    /// Only sink nodes (no forward consumer) are guaranteed to still hold
    /// their output; other buffers may have been reused.
    pub fn output(&self, node: NodeId) -> Option<&[f32]> {
        self.schedule
            .op(node)
            .map(|op| self.buffers[op.output].as_slice())
    }
}

/// Sums all inputs, then applies a gain.
#[derive(Clone, Copy, Debug)]
pub struct MixGain(pub f32);

impl DspProcessor for MixGain {
    fn process(&mut self, inputs: &Inputs<'_>, output: &mut [f32]) {
        output.fill(0.0);
        for k in 0..inputs.len() {
            for (y, &x) in output.iter_mut().zip(inputs.get(k)) {
                *y += x;
            }
        }
        for y in output.iter_mut() {
            *y *= self.0;
        }
    }
}

/// A node with intrinsic latency: sums its inputs and delays them by its
/// latency (a stand-in for a lookahead limiter or linear-phase filter).
#[derive(Clone, Debug)]
pub struct LatentMix {
    line: Option<DelayLine>,
    scratch: Vec<f32>,
}

impl LatentMix {
    /// `latency` samples of delay for blocks of `block` samples.
    pub fn new(latency: u32, block: usize) -> Result<Self, PdcError> {
        Ok(LatentMix {
            line: if latency == 0 {
                None
            } else {
                Some(DelayLine::new(latency, block)?)
            },
            scratch: vec![0.0; block],
        })
    }
}

impl DspProcessor for LatentMix {
    fn process(&mut self, inputs: &Inputs<'_>, output: &mut [f32]) {
        let n = output.len().min(self.scratch.len());
        let sum = &mut self.scratch[..n];
        sum.fill(0.0);
        for k in 0..inputs.len() {
            for (y, &x) in sum.iter_mut().zip(inputs.get(k)) {
                *y += x;
            }
        }
        match &mut self.line {
            None => output[..n].copy_from_slice(sum),
            Some(line) => {
                line.write(sum);
                line.read(&mut output[..n]);
                line.advance(n);
            }
        }
    }
}
