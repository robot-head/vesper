//! Edge cases of the Vesper saturation engine (review of #1436): names that
//! spell the engine's keywords, extraction of deep binding chains on an
//! ordinary test-thread stack, and shared unchanged subgraphs that must not
//! be walked as trees when explaining an extraction.
use std::fmt::Write as _;
use std::time::{Duration, Instant};
use vesper_fusion::dsp::DspGraph;
use vesper_fusion::engine::{self, Node};
use vesper_fusion::tensor::{self, Activation, DType, TensorGraph};

#[test]
fn names_that_spell_keywords_fuse() {
    for name in ["sort", "constructor", "rewrite", "let", "i64", "String"] {
        let mut g = TensorGraph::new();
        let x = g.input(name, DType::F32, 8).unwrap();
        let y = g.activation(Activation::Relu, x).unwrap();
        let y = g.add(y, x).unwrap();
        let fused = g.fuse(y).unwrap();
        assert!(fused.witness_summary().steps > 0, "{name}");
        let mut d = DspGraph::new();
        let s = d.source(name).unwrap();
        let s = d.gain(0.5, s).unwrap();
        d.fuse(s).unwrap();
        let t = engine::run(
            tensor::RULES,
            &format!("(let t0 (In \"{name}\"))\n"),
            "t0",
            false,
        )
        .unwrap();
        assert!(t.nodes.contains(&Node::Str(name.into())), "{name}");
    }
}

#[test]
fn deep_binding_chains_extract_without_native_recursion() {
    let depth = 50_000;
    let mut b = String::from("(let t0 (In \"x\"))\n");
    for i in 1..=depth {
        let _ = writeln!(b, "(let t{i} (Act {} t{}))", i % 4, i - 1);
    }
    let t = engine::run(tensor::RULES, &b, &format!("t{depth}"), false).unwrap();
    assert_eq!(t.count("Act"), depth);
}

#[test]
fn shared_unchanged_subgraphs_are_not_walked_as_trees() {
    let mut g = DspGraph::new();
    let mut s = g.source("in").unwrap();
    for _ in 0..40 {
        s = g.mix(s, s).unwrap();
    }
    let start = Instant::now();
    let fused = g.fuse(s).unwrap();
    assert_eq!(fused.witness_summary().steps, 0);
    assert!(start.elapsed() < Duration::from_secs(30));
}
