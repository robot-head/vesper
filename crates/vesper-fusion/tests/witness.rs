//! Rewrite witnesses: every fusion carries a checked chain of named rewrites
//! from the input graph to the fused term, every shipped rule shows up in
//! some witness, and tampered or foreign witnesses and unsound rewrites are
//! refused.
use std::collections::BTreeSet;
use vesper_fusion::dsp::{self, Biquad, DspGraph};
use vesper_fusion::engine;
use vesper_fusion::tensor::{self, Activation, DType, TensorGraph};
use vesper_fusion::witness::{self, Direction, RuleSet, Summary, Witness};
use vesper_fusion::{Error, RULE_LEMMAS};
use vesper_tensor::quant::WeightFormat;

fn section() -> Biquad {
    Biquad::new([0.2, 0.1, 0.05], [-0.3, 0.1]).unwrap()
}

/// Tensor graphs that between them use every tensor rule.
fn tensor_cases() -> Vec<vesper_fusion::tensor::FusedTensor> {
    let mut out = Vec::new();
    for format in [WeightFormat::F32, WeightFormat::Q8_0] {
        let mut g = TensorGraph::new();
        let x = g.input("x", DType::F32, 64).unwrap();
        let b = g.input("b", DType::F32, 24).unwrap();
        let w = g.matrix("w", 24, 64, format).unwrap();
        let y = g.matvec(w, x).unwrap();
        let y = g.add(y, b).unwrap();
        let y = g.mul(y, b).unwrap();
        let y = g.scale_f32(0.5, y).unwrap();
        let y = g.activation(Activation::Silu, y).unwrap();
        out.push(g.fuse(y).unwrap());
    }
    let mut g = TensorGraph::new();
    let x = g.input("x", DType::F32, 64).unwrap();
    let n = g.input("n", DType::F32, 64).unwrap();
    let w = g.matrix("w", 16, 64, WeightFormat::F32).unwrap();
    let h = g.rms_norm(x, n, 1e-5).unwrap();
    let y = g.matvec(w, h).unwrap();
    let y = g.activation(Activation::Relu, y).unwrap();
    out.push(g.fuse(y).unwrap());
    out
}

/// DSP chains that between them use every DSP rule.
fn dsp_cases() -> Vec<vesper_fusion::dsp::FusedDsp> {
    let mut out = Vec::new();
    // gain, pan, compose, then a three-section cascade with a gain before it.
    let mut g = DspGraph::new();
    let s = g.source("in").unwrap();
    let a = g.gain(0.5, s).unwrap();
    let a = g.pan(0.25, a).unwrap();
    let a = g.gain(0.75, a).unwrap();
    let a = g.mix(a, s).unwrap();
    let a = g.gain(0.9, a).unwrap();
    let a = g.biquad(section(), a).unwrap();
    let a = g.biquad(section(), a).unwrap();
    let a = g.biquad(section(), a).unwrap();
    out.push(g.fuse(a).unwrap());
    // a gain after a cascade, and two sends of one source.
    let mut g = DspGraph::new();
    let s = g.source("in").unwrap();
    let a = g.biquad(section(), s).unwrap();
    let a = g.biquad(section(), a).unwrap();
    let a = g.gain(0.5, a).unwrap();
    out.push(g.fuse(a).unwrap());
    let mut g = DspGraph::new();
    let s = g.source("in").unwrap();
    let a = g.gain(0.3, s).unwrap();
    let b = g.gain(0.6, s).unwrap();
    let m = g.mix(a, b).unwrap();
    out.push(g.fuse(m).unwrap());
    out
}

fn used(summaries: &[&Summary]) -> BTreeSet<&'static str> {
    summaries
        .iter()
        .flat_map(|s| s.rules.iter().map(|(name, _, _)| *name))
        .collect()
}

fn refused<T: std::fmt::Debug>(r: Result<T, Error>) {
    assert!(matches!(r, Err(Error::Witness(_))), "{r:?}");
}

#[test]
fn every_shipped_rule_appears_in_a_checked_witness() {
    let tensors = tensor_cases();
    let dsps = dsp_cases();
    let mut summaries: Vec<&Summary> = tensors.iter().map(|f| f.witness_summary()).collect();
    summaries.extend(dsps.iter().map(|f| f.witness_summary()));
    for s in &summaries {
        assert!(s.steps > 0);
        assert_eq!(s.steps, s.forward + s.backward);
        assert_eq!(s.steps, s.rules.iter().map(|(_, _, n)| n).sum::<usize>());
        for (name, lemma, _) in &s.rules {
            assert!(RULE_LEMMAS.contains(&(*name, *lemma)), "{name} {lemma}");
        }
    }
    let all: BTreeSet<&str> = RULE_LEMMAS.iter().map(|(n, _)| *n).collect();
    assert_eq!(used(&summaries), all);
    // Re-checking the stored witness gives the stored summary.
    for f in &tensors {
        assert_eq!(&f.check_witness(f.witness()).unwrap(), f.witness_summary());
    }
    for f in &dsps {
        assert_eq!(&f.check_witness(f.witness()).unwrap(), f.witness_summary());
    }
}

#[test]
fn rule_sets_name_every_rewrite_with_a_lemma() {
    let t = tensor::rule_set().unwrap();
    let d = dsp::rule_set().unwrap();
    assert_eq!(t.rules().len(), tensor::REWRITE_NAMES.len());
    assert_eq!(d.rules().len(), dsp::REWRITE_NAMES.len());
    assert_eq!(t.rules().len() + d.rules().len(), RULE_LEMMAS.len());
    // One rewrite too many, a name without a lemma, an unbound right-side
    // variable: all refused.
    let extra = format!("{}(rewrite (Act a x) x)\n", tensor::RULES);
    refused(RuleSet::parse(&extra, tensor::REWRITE_NAMES));
    let mut names = tensor::REWRITE_NAMES.to_vec();
    names[0] = "no-such-rule";
    refused(RuleSet::parse(tensor::RULES, &names));
    refused(RuleSet::parse("(rewrite (Act a x) (Act a y))", &["ew-act"]));
    refused(RuleSet::parse(
        "(rewrite (Act a x) x :when ((= a 0)))",
        &["ew-act"],
    ));
}

fn tampered(f: &vesper_fusion::tensor::FusedTensor, edit: impl Fn(&mut Witness)) {
    let mut w = f.witness().clone();
    edit(&mut w);
    refused(f.check_witness(&w));
}

#[test]
fn tampered_witnesses_are_refused() {
    let cases = tensor_cases();
    let f = &cases[1];
    let n = f.witness().steps.len();
    assert!(n >= 4);
    tampered(f, |w| w.steps.clear());
    tampered(f, |w| {
        w.steps.pop();
    });
    tampered(f, |w| {
        w.steps.remove(0);
    });
    tampered(f, |w| w.steps.swap(0, n - 1));
    tampered(f, |w| {
        let extra = w.steps[0].clone();
        w.steps.push(extra);
    });
    tampered(f, |w| w.steps[0].rule = (w.steps[0].rule + 1) % 8);
    tampered(f, |w| w.steps[0].rule = 99);
    tampered(f, |w| {
        w.steps[0].direction = match w.steps[0].direction {
            Direction::Forward => Direction::Backward,
            Direction::Backward => Direction::Forward,
        }
    });
    tampered(f, |w| w.steps[n - 1].path.push(0));
    tampered(f, |w| w.steps[n - 1].path.push(7));
    tampered(f, |w| w.steps[0].substitution.clear());
    tampered(f, |w| {
        let dup = w.steps[0].substitution[0].clone();
        w.steps[0].substitution.push(dup);
    });
    tampered(f, |w| {
        w.steps[0].substitution.push(("unused".into(), 0));
    });
    tampered(f, |w| {
        let len = w.terms.len();
        w.steps[0].substitution[0].1 = len;
    });
    tampered(f, |w| {
        // Point a variable at some other term of the arena.
        let (_, t) = w.steps[0].substitution[0].clone();
        w.steps[0].substitution[0].1 = (t + 1) % w.terms.len();
    });
}

#[test]
fn a_witness_for_another_graph_is_refused() {
    let tensors = tensor_cases();
    // Same shape, different matrix format: the dense witness does not reach
    // the dequantized term.
    refused(tensors[1].check_witness(tensors[0].witness()));
    refused(tensors[0].check_witness(tensors[2].witness()));
    let dsps = dsp_cases();
    refused(dsps[0].check_witness(dsps[2].witness()));
}

#[test]
fn checking_against_the_wrong_source_or_target_is_refused() {
    let set = tensor::rule_set().unwrap();
    let (src, root) =
        witness::source_term("(let t0 (In \"x\"))\n(let t1 (Act 0 t0))\n", "t1").unwrap();
    let (tgt, troot) =
        witness::source_term("(let t0 (Fuse (EAct 0 (Lane (In \"x\")))))\n", "t0").unwrap();
    let empty = Witness {
        terms: Vec::new(),
        steps: Vec::new(),
    };
    // No steps: only a term equal to the source is reached.
    assert_eq!(
        witness::check(set, (&src, root), (&src, root), &empty)
            .unwrap()
            .steps,
        0
    );
    refused(witness::check(set, (&src, root), (&tgt, troot), &empty));
    // One honest ew-act step reaches the target, and nothing else.
    let mut terms = src.clone();
    terms.push(vesper_fusion::engine::Node::Int(0));
    let a = terms.len() - 1;
    let x = src
        .iter()
        .position(|n| matches!(n, vesper_fusion::engine::Node::App(h, _) if h == "In"))
        .unwrap();
    let step = witness::Step {
        rule: 0,
        direction: Direction::Forward,
        path: Vec::new(),
        substitution: vec![("a".into(), a), ("x".into(), x)],
    };
    let w = Witness {
        terms,
        steps: vec![step],
    };
    let s = witness::check(set, (&src, root), (&tgt, troot), &w).unwrap();
    assert_eq!(s.rules, vec![("ew-act", "ew_act_refines", 1)]);
    refused(witness::check(set, (&tgt, troot), (&tgt, troot), &w));
    refused(witness::check(set, (&src, root), (&src, root), &w));
}

#[test]
fn an_unsound_rewrite_egglog_applies_is_refused() {
    // egglog runs a rule set with an extra rewrite that drops activations
    // (cheaper, so extraction uses it), but the checker only knows the
    // named, lemma-backed rules.
    let bogus = format!("{}(rewrite (Act a x) x)\n", tensor::RULES);
    let set = tensor::rule_set().unwrap();
    let bindings = "(let t0 (In \"x\"))\n(let t1 (Act 0 t0))\n(let t2 (Add t1 t0))\n";
    let honest = engine::fuse(tensor::RULES, set, bindings, "t2").unwrap();
    assert!(honest.summary.steps > 0);
    match engine::fuse(&bogus, set, bindings, "t2") {
        Err(Error::Witness(m)) => assert!(m.contains("unknown rule"), "{m}"),
        other => panic!("{other:?}"),
    }
    // A rule set that does not match the text egglog runs is refused too.
    assert!(engine::fuse(dsp::RULES, set, bindings, "t2").is_err());
}

#[test]
fn resource_names_may_contain_the_query_prefix() {
    let mut g = TensorGraph::new();
    let x = g.input("fusionwitness0", DType::F32, 8).unwrap();
    let y = g.activation(Activation::Relu, x).unwrap();
    let y = g.add(y, x).unwrap();
    assert!(g.fuse(y).unwrap().witness_summary().steps > 0);
    // A binding identifier that would shadow the query is still refused.
    let set = tensor::rule_set().unwrap();
    refused(engine::fuse(
        tensor::RULES,
        set,
        "(let fusionwitness0 (In \"x\"))\n",
        "fusionwitness0",
    ));
}
