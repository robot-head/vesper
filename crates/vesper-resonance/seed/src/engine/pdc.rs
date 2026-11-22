//! The frozen Rust built-in graph runner (`pdc::render_builtin_graph`)
//! and schedule compiler (`pdc::compile_schedule`), which the Vesper port
//! (`vesper/src/graph.ves`, `vesper/src/pdc.ves`) replaced in the package
//! `vesper-resonance` (SH6 lane 9, #1404).

use crate::pdc::{
    BuiltinKind, CompiledGraph, DspProcessor, Input, InputSource, Inputs, LatentMix, LineSpec,
    MixGain, NodeId, NodeSpec, PdcError, Schedule, ScheduledOp,
};
use std::collections::{BTreeMap, BTreeSet};

/// The deterministic source of [`BuiltinKind::Source`].
struct BuiltinSource {
    seed: u64,
    t: u64,
}

impl DspProcessor for BuiltinSource {
    fn process(&mut self, _inputs: &Inputs<'_>, output: &mut [f32]) {
        for y in output.iter_mut() {
            let x = self
                .t
                .wrapping_mul(7919)
                .wrapping_add(self.seed.wrapping_mul(31))
                % 97;
            *y = x as f32 / 97.0 - 0.5;
            self.t += 1;
        }
    }
}

/// `render_builtin_graph` after its kind-count check: a [`CompiledGraph`] of
/// `Box<dyn DspProcessor>`.
pub(crate) fn render_builtin_graph(
    nodes: &[NodeSpec],
    kinds: &[BuiltinKind],
    block: usize,
    blocks: usize,
    sink: NodeId,
) -> Result<Vec<f32>, PdcError> {
    let mut processors: Vec<Box<dyn DspProcessor>> = Vec::with_capacity(nodes.len());
    for (n, k) in nodes.iter().zip(kinds) {
        processors.push(match *k {
            BuiltinKind::Source(seed) => Box::new(BuiltinSource {
                seed: u64::from(seed),
                t: 0,
            }),
            BuiltinKind::Latent => Box::new(LatentMix::new(n.latency, block)?),
            BuiltinKind::Gain(g) => Box::new(MixGain(g)),
        });
    }
    let mut graph = CompiledGraph::new(nodes, processors, block)?;
    for _ in 0..blocks {
        graph.process_block();
    }
    graph
        .output(sink)
        .map(<[f32]>::to_vec)
        .ok_or(PdcError::UnknownNode(sink))
}

/// Compile `nodes` into a delay-compensated schedule.
pub(crate) fn compile_schedule(nodes: &[NodeSpec]) -> Result<Schedule, PdcError> {
    let n = nodes.len();
    let mut index_of = BTreeMap::new();
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
