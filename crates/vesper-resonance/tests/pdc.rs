//! Delay-compensated DAG compiler and block executor against a per-sample
//! reference evaluator.

use vesper_resonance::pdc::{
    CompiledGraph, DspProcessor, Input, InputSource, Inputs, LatentMix, MixGain, NodeId, NodeSpec,
    PdcError, compile_schedule,
};

#[derive(Clone, Copy, Debug)]
enum Kind {
    /// Deterministic generator; ignores inputs.
    Source(u32),
    /// Sum inputs, delay by the node's latency.
    Latent,
    /// Sum inputs, multiply.
    Gain(f32),
}

fn source_sample(seed: u32, t: u64) -> f32 {
    let x = (t.wrapping_mul(7919).wrapping_add(u64::from(seed) * 31)) % 97;
    x as f32 / 97.0 - 0.5
}

struct Generator {
    seed: u32,
    t: u64,
}

impl DspProcessor for Generator {
    fn process(&mut self, _inputs: &Inputs<'_>, output: &mut [f32]) {
        for y in output.iter_mut() {
            *y = source_sample(self.seed, self.t);
            self.t += 1;
        }
    }
}

fn processors(nodes: &[NodeSpec], kinds: &[Kind], block: usize) -> Vec<Box<dyn DspProcessor>> {
    nodes
        .iter()
        .zip(kinds)
        .map(|(n, k)| -> Box<dyn DspProcessor> {
            match *k {
                Kind::Source(seed) => Box::new(Generator { seed, t: 0 }),
                Kind::Latent => Box::new(LatentMix::new(n.latency, block).unwrap()),
                Kind::Gain(g) => Box::new(MixGain(g)),
            }
        })
        .collect()
}

/// Per-sample reference: evaluates every node at every sample in schedule
/// order, reading forward inputs at `t - compensation` and feedback inputs
/// at `t - delay`, both from full output histories.
fn reference(nodes: &[NodeSpec], kinds: &[Kind], samples: usize) -> Vec<Vec<f32>> {
    let schedule = compile_schedule(nodes).unwrap();
    let pos = |id: NodeId| nodes.iter().position(|n| n.id == id).unwrap();
    let mut out = vec![vec![0.0f32; samples]; nodes.len()];
    let mut sums = vec![vec![0.0f32; samples]; nodes.len()];
    for t in 0..samples {
        for op in &schedule.ops {
            let i = op.spec_index;
            let mut s = 0.0f32;
            for input in &op.inputs {
                let (src, d) = match *input {
                    InputSource::Forward {
                        from, compensation, ..
                    } => (pos(from), compensation as usize),
                    InputSource::Feedback { from, delay, .. } => (pos(from), delay as usize),
                };
                if t >= d {
                    s += out[src][t - d];
                }
            }
            sums[i][t] = s;
            out[i][t] = match kinds[i] {
                Kind::Source(seed) => source_sample(seed, t as u64),
                Kind::Gain(g) => s * g,
                Kind::Latent => {
                    let l = nodes[i].latency as usize;
                    if t >= l { sums[i][t - l] } else { 0.0 }
                }
            };
        }
    }
    out
}

fn run_blocks(
    nodes: &[NodeSpec],
    kinds: &[Kind],
    block: usize,
    blocks: usize,
    sink: NodeId,
) -> Vec<f32> {
    let mut g = CompiledGraph::new(nodes, processors(nodes, kinds, block), block).unwrap();
    let mut out = Vec::new();
    for _ in 0..blocks {
        g.process_block();
        out.extend_from_slice(g.output(sink).unwrap());
    }
    out
}

fn fwd(ids: &[u32]) -> Vec<Input> {
    ids.iter().map(|&i| Input::Forward(NodeId(i))).collect()
}

fn node(id: u32, latency: u32, inputs: Vec<Input>) -> NodeSpec {
    NodeSpec {
        id: NodeId(id),
        latency,
        inputs,
    }
}

/// dry/wet: src -> limiter (37 samples lookahead) -> sum, src -> dry -> sum.
fn dry_wet() -> (Vec<NodeSpec>, Vec<Kind>) {
    (
        vec![
            node(0, 0, vec![]),
            node(1, 37, fwd(&[0])),
            node(2, 0, fwd(&[0])),
            node(3, 0, fwd(&[1, 2])),
        ],
        vec![
            Kind::Source(1),
            Kind::Latent,
            Kind::Gain(1.0),
            Kind::Gain(1.0),
        ],
    )
}

#[test]
fn parallel_branches_get_compensation_delays() {
    let (nodes, _) = dry_wet();
    let s = compile_schedule(&nodes).unwrap();
    let sum = s.op(NodeId(3)).unwrap();
    let comps: Vec<u32> = sum
        .inputs
        .iter()
        .map(|i| match *i {
            InputSource::Forward { compensation, .. } => compensation,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(comps, vec![0, 37]);
    assert_eq!(sum.output_latency, 37);
    assert_eq!(s.lines.len(), 1);
}

#[test]
fn compensated_impulse_arrives_aligned() {
    struct Impulse(bool);
    impl DspProcessor for Impulse {
        fn process(&mut self, _: &Inputs<'_>, out: &mut [f32]) {
            out.fill(0.0);
            if !self.0 {
                out[0] = 1.0;
                self.0 = true;
            }
        }
    }
    let (nodes, _) = dry_wet();
    let block = 16;
    let procs: Vec<Box<dyn DspProcessor>> = vec![
        Box::new(Impulse(false)),
        Box::new(LatentMix::new(37, block).unwrap()),
        Box::new(MixGain(1.0)),
        Box::new(MixGain(1.0)),
    ];
    let mut g = CompiledGraph::new(&nodes, procs, block).unwrap();
    let mut out = Vec::new();
    for _ in 0..5 {
        g.process_block();
        out.extend_from_slice(g.output(NodeId(3)).unwrap());
    }
    for (t, &y) in out.iter().enumerate() {
        assert_eq!(y, if t == 37 { 2.0 } else { 0.0 }, "t={t}");
    }
}

#[test]
fn block_executor_matches_per_sample_reference() {
    let (nodes, kinds) = dry_wet();
    let r = reference(&nodes, &kinds, 640);
    for block in [1, 7, 16, 64] {
        let got = run_blocks(&nodes, &kinds, block, 640 / block, NodeId(3));
        assert_eq!(got, r[3][..got.len()], "block {block}");
    }
}

/// src -> mix -> damp(0.5) --(feedback 64)--> mix; damp is the sink.
fn feedback_graph(delay: u32) -> (Vec<NodeSpec>, Vec<Kind>) {
    (
        vec![
            node(0, 0, vec![]),
            node(
                1,
                5,
                vec![
                    Input::Forward(NodeId(0)),
                    Input::Feedback {
                        from: NodeId(2),
                        delay,
                    },
                ],
            ),
            node(2, 0, fwd(&[1])),
        ],
        vec![Kind::Source(3), Kind::Latent, Kind::Gain(0.5)],
    )
}

#[test]
fn delayed_feedback_matches_reference() {
    let (nodes, kinds) = feedback_graph(64);
    let r = reference(&nodes, &kinds, 1024);
    assert!(r[2].iter().skip(70).any(|&y| y != 0.0));
    for block in [1, 8, 32, 64] {
        let got = run_blocks(&nodes, &kinds, block, 1024 / block, NodeId(2));
        assert_eq!(got, r[2][..got.len()], "block {block}");
    }
}

#[test]
fn cycles_need_explicit_delay() {
    // a -> b -> a with forward edges only.
    let nodes = vec![
        node(0, 0, fwd(&[1])),
        node(1, 0, fwd(&[0])),
        node(2, 0, vec![]),
    ];
    assert_eq!(
        compile_schedule(&nodes),
        Err(PdcError::CycleWithoutDelay {
            nodes: vec![NodeId(0), NodeId(1)]
        })
    );
    let (nodes, _) = feedback_graph(0);
    assert_eq!(
        compile_schedule(&nodes),
        Err(PdcError::ZeroDelayFeedback {
            from: NodeId(2),
            to: NodeId(1)
        })
    );
    let (nodes, kinds) = feedback_graph(16);
    assert!(compile_schedule(&nodes).is_ok());
    assert!(matches!(
        CompiledGraph::new(&nodes, processors(&nodes, &kinds, 32), 32),
        Err(PdcError::FeedbackShorterThanBlock {
            delay: 16,
            block: 32,
            ..
        })
    ));
}

#[test]
fn malformed_graphs_are_rejected() {
    assert_eq!(
        compile_schedule(&[node(0, 0, vec![]), node(0, 0, vec![])]),
        Err(PdcError::DuplicateNode(NodeId(0)))
    );
    assert_eq!(
        compile_schedule(&[node(0, 0, fwd(&[9]))]),
        Err(PdcError::UnknownNode(NodeId(9)))
    );
    let nodes = vec![node(0, u32::MAX, vec![])];
    assert!(compile_schedule(&nodes).is_ok());
    assert_eq!(
        LatentMix::new(u32::MAX, 16).err(),
        Some(PdcError::DelayTooLong(u32::MAX))
    );
    let long = vec![
        node(0, 0, vec![]),
        node(1, u32::MAX, fwd(&[0])),
        node(2, 0, fwd(&[0, 1])),
    ];
    let procs: Vec<Box<dyn DspProcessor>> = vec![
        Box::new(MixGain(1.0)),
        Box::new(MixGain(1.0)),
        Box::new(MixGain(1.0)),
    ];
    assert!(matches!(
        CompiledGraph::new(&long, procs, 16),
        Err(PdcError::DelayTooLong(u32::MAX))
    ));
    let nodes = vec![node(0, 0, vec![])];
    assert!(matches!(
        CompiledGraph::new(&nodes, vec![], 16),
        Err(PdcError::ProcessorCount { .. })
    ));
    assert!(matches!(
        CompiledGraph::new(&nodes, vec![Box::new(MixGain(1.0))], 0),
        Err(PdcError::BadBlockLength(0))
    ));
}

#[test]
fn compensation_over_u32_is_a_typed_error() {
    // Two chains of u32::MAX each, merged with a direct edge: compensation
    // 2 * u32::MAX does not fit a u32 delay line.
    let nodes = vec![
        node(0, 0, vec![]),
        node(1, u32::MAX, fwd(&[0])),
        node(2, u32::MAX, fwd(&[1])),
        node(3, 0, fwd(&[0, 2])),
    ];
    assert_eq!(
        compile_schedule(&nodes),
        Err(PdcError::LatencyOverflow(NodeId(3)))
    );
}

#[test]
fn buffer_liveness_reuses_buffers() {
    let mut nodes = vec![node(0, 0, vec![])];
    for i in 1..10 {
        nodes.push(node(i, 0, fwd(&[i - 1])));
    }
    let s = compile_schedule(&nodes).unwrap();
    assert_eq!(s.buffer_count, 2);
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random_dag(rng: &mut Rng, n: u32) -> (Vec<NodeSpec>, Vec<Kind>) {
    let mut nodes = Vec::new();
    let mut kinds = Vec::new();
    for i in 0..n {
        if i < 2 {
            nodes.push(node(i, 0, vec![]));
            kinds.push(Kind::Source(i));
            continue;
        }
        let mut inputs = Vec::new();
        for _ in 0..=rng.below(3) {
            inputs.push(Input::Forward(NodeId(rng.below(u64::from(i)) as u32)));
        }
        if rng.below(4) == 0 {
            inputs.push(Input::Feedback {
                from: NodeId(rng.below(u64::from(n)) as u32),
                delay: 32 + rng.below(40) as u32,
            });
        }
        nodes.push(node(i, rng.below(50) as u32, inputs));
        kinds.push(if rng.below(2) == 0 {
            Kind::Latent
        } else {
            Kind::Gain(0.25)
        });
    }
    // Shuffle spec order so the scheduler has to sort.
    for i in (1..nodes.len()).rev() {
        let j = rng.below(i as u64 + 1) as usize;
        nodes.swap(i, j);
        kinds.swap(i, j);
    }
    // Gain nodes must have zero latency in this model.
    for (n, k) in nodes.iter_mut().zip(&kinds) {
        if !matches!(k, Kind::Latent) {
            n.latency = 0;
        }
    }
    (nodes, kinds)
}

/// Every forward path from s to t carries latency(t) - latency(s) after
/// compensation: sum over edges of (compensation + intrinsic of the head).
#[test]
fn random_dags_have_equal_path_latency_and_match_reference() {
    let mut rng = Rng(0x0005_eed8);
    for case in 0..40 {
        let (nodes, kinds) = random_dag(&mut rng, 3 + (case % 9));
        let s = compile_schedule(&nodes).unwrap();
        let lat = |id: NodeId| s.op(id).unwrap().output_latency;
        // DFS all paths from each node, accumulating compensated latency.
        for start in &nodes {
            let mut stack = vec![(start.id, 0u64)];
            while let Some((u, acc)) = stack.pop() {
                assert_eq!(lat(start.id) + acc, lat(u), "case {case}");
                for op in &s.ops {
                    let spec = &nodes[op.spec_index];
                    for input in &op.inputs {
                        if let InputSource::Forward {
                            from, compensation, ..
                        } = *input
                            && from == u
                        {
                            stack.push((
                                op.node,
                                acc + u64::from(compensation) + u64::from(spec.latency),
                            ));
                        }
                    }
                }
            }
        }
        // Executor vs per-sample reference for every sink.
        let r = reference(&nodes, &kinds, 256);
        let sinks: Vec<usize> = (0..nodes.len())
            .filter(|&i| {
                !nodes
                    .iter()
                    .any(|n| n.inputs.contains(&Input::Forward(nodes[i].id)))
            })
            .collect();
        for block in [1, 32] {
            let mut g =
                CompiledGraph::new(&nodes, processors(&nodes, &kinds, block), block).unwrap();
            let mut got = vec![Vec::new(); nodes.len()];
            for _ in 0..256 / block {
                g.process_block();
                for &i in &sinks {
                    got[i].extend_from_slice(g.output(nodes[i].id).unwrap());
                }
            }
            for &i in &sinks {
                for (t, (a, b)) in got[i].iter().zip(&r[i]).enumerate() {
                    assert!(
                        (a - b).abs() <= 1e-5 * (1.0 + b.abs()),
                        "case {case} block {block} node {i} t {t}: {a} vs {b}"
                    );
                }
            }
        }
    }
}
