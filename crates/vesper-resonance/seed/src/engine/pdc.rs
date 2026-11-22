//! The frozen Rust built-in graph runner (`pdc::render_builtin_graph`),
//! which the Vesper port (vesper/src/graph.ves) replaced in the package
//! vesper-resonance (SH6 lane 9, #1404).

use crate::pdc::{
    BuiltinKind, CompiledGraph, DspProcessor, Inputs, LatentMix, MixGain, NodeId, NodeSpec,
    PdcError,
};

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
