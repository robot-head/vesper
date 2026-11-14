//! Resonance chain fusion: fused terms against the unfused reference on
//! random blocks. Cascades of biquads and gain/pan stages that are not
//! composed are bit-identical; composed gains, merged sends and gains folded
//! into biquad coefficients stay within the stated tolerances.
mod common;

use common::Rng;
use vesper_fusion::Error;
use vesper_fusion::dsp::{
    Biquad, DspEnv, DspGraph, Stereo, filter_tolerance, gain_tolerance, pan_gains,
};

fn block(rng: &mut Rng, n: usize) -> Stereo {
    Stereo {
        left: rng.vec(n, 1.0),
        right: rng.vec(n, 1.0),
    }
}

fn bits(s: &Stereo) -> Vec<u32> {
    s.left.iter().chain(&s.right).map(|v| v.to_bits()).collect()
}

/// A random stable section with pole radius at most 0.99.
fn section(rng: &mut Rng) -> Biquad {
    let radius = 0.3 + 0.69 * (rng.f32(1.0) * 0.5 + 0.5);
    let theta = rng.f32(std::f32::consts::PI);
    let (a1, a2) = (-2.0 * radius * theta.cos(), radius * radius);
    Biquad::new([rng.f32(1.0), rng.f32(1.0), rng.f32(1.0)], [a1, a2]).unwrap()
}

fn max_diff(a: &Stereo, b: &Stereo) -> f32 {
    a.left
        .iter()
        .chain(&a.right)
        .zip(b.left.iter().chain(&b.right))
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

fn peak(a: &Stereo) -> f32 {
    a.left
        .iter()
        .chain(&a.right)
        .map(|v| v.abs())
        .fold(0.0, f32::max)
}

#[test]
fn biquad_cascade_is_one_pass_and_bit_identical() {
    let mut rng = Rng(0x5eed_2001);
    for _ in 0..20 {
        let mut g = DspGraph::new();
        let src = g.source("in").unwrap();
        let mut cur = src;
        for _ in 0..4 {
            cur = g.biquad(section(&mut rng), cur).unwrap();
        }
        let fused = g.fuse(cur).unwrap();
        let report = fused.report().unwrap();
        assert_eq!(
            (report.passes_before, report.passes_after),
            (4, 1),
            "{}",
            fused.term()
        );
        assert!(report.cost_after < report.cost_before);
        assert_eq!((fused.count("Cascade"), fused.count("SCons")), (1, 4));
        let mut env = DspEnv::new();
        env.source("in", block(&mut rng, 1024));
        assert_eq!(
            bits(&g.evaluate(cur, &env).unwrap()),
            bits(&fused.evaluate(&env).unwrap())
        );
    }
}

#[test]
fn gain_and_pan_stages_compose_within_tolerance() {
    let mut rng = Rng(0x5eed_2002);
    for _ in 0..30 {
        let mut g = DspGraph::new();
        let src = g.source("in").unwrap();
        let (g1, p, g2) = (rng.f32(2.0), rng.f32(1.0), rng.f32(2.0));
        let a = g.gain(g1, src).unwrap();
        let b = g.pan(p, a).unwrap();
        let root = g.gain(g2, b).unwrap();
        let fused = g.fuse(root).unwrap();
        let report = fused.report().unwrap();
        assert_eq!(
            (report.passes_before, report.passes_after),
            (3, 1),
            "{}",
            fused.term()
        );
        assert_eq!(fused.count("Stereo"), 1);
        let mut env = DspEnv::new();
        let input = block(&mut rng, 512);
        env.source("in", input.clone());
        let reference = g.evaluate(root, &env).unwrap();
        let out = fused.evaluate(&env).unwrap();
        let (pl, pr) = pan_gains(p);
        for t in 0..512 {
            for (x, gain, r, o) in [
                (input.left[t], pl, reference.left[t], out.left[t]),
                (input.right[t], pr, reference.right[t], out.right[t]),
            ] {
                let magnitude = (g1 * gain * g2 * x).abs() * 2.0;
                assert!((r - o).abs() <= gain_tolerance(4, magnitude), "{r} vs {o}");
            }
        }
    }
}

#[test]
fn uniform_gain_folds_into_the_first_biquad_section() {
    let mut rng = Rng(0x5eed_2003);
    for before in [false, true] {
        for _ in 0..20 {
            let mut g = DspGraph::new();
            let src = g.source("in").unwrap();
            let k = rng.f32(3.0);
            let root = if before {
                let a = g.gain(k, src).unwrap();
                let b = g.biquad(section(&mut rng), a).unwrap();
                g.biquad(section(&mut rng), b).unwrap()
            } else {
                let a = g.biquad(section(&mut rng), src).unwrap();
                let b = g.biquad(section(&mut rng), a).unwrap();
                g.gain(k, b).unwrap()
            };
            let fused = g.fuse(root).unwrap();
            let report = fused.report().unwrap();
            assert_eq!(
                (report.passes_before, report.passes_after),
                (3, 1),
                "{}",
                fused.term()
            );
            assert_eq!(fused.count("CScale"), 1, "{}", fused.term());
            assert_eq!(fused.count("Stereo"), 0);
            let mut env = DspEnv::new();
            env.source("in", block(&mut rng, 4096));
            let reference = g.evaluate(root, &env).unwrap();
            let out = fused.evaluate(&env).unwrap();
            let diff = max_diff(&reference, &out);
            assert!(
                diff <= filter_tolerance(peak(&reference)),
                "diff {diff} peak {}",
                peak(&reference)
            );
        }
    }
}

#[test]
fn parallel_sends_of_one_source_merge() {
    let mut rng = Rng(0x5eed_2004);
    for _ in 0..30 {
        let mut g = DspGraph::new();
        let src = g.source("in").unwrap();
        let (ga, gb) = (rng.f32(1.0), rng.f32(1.0));
        let a = g.gain(ga, src).unwrap();
        let b = g.gain(gb, src).unwrap();
        let root = g.mix(a, b).unwrap();
        let fused = g.fuse(root).unwrap();
        let report = fused.report().unwrap();
        assert_eq!(
            (report.passes_before, report.passes_after),
            (3, 1),
            "{}",
            fused.term()
        );
        assert_eq!(fused.count("GAdd"), 1); // one hash-consed node shared by both channels
        let mut env = DspEnv::new();
        let input = block(&mut rng, 256);
        env.source("in", input.clone());
        let reference = g.evaluate(root, &env).unwrap();
        let out = fused.evaluate(&env).unwrap();
        for t in 0..256 {
            for (x, r, o) in [
                (input.left[t], reference.left[t], out.left[t]),
                (input.right[t], reference.right[t], out.right[t]),
            ] {
                let magnitude = (ga.abs() + gb.abs()) * x.abs() * 2.0;
                assert!((r - o).abs() <= gain_tolerance(3, magnitude), "{r} vs {o}");
            }
        }
    }
}

#[test]
fn channel_strips_mix_in_fewer_resonance_passes() {
    // Two strips: EQ (two sections), trim, pan; then a bus gain.
    let mut rng = Rng(0x5eed_2005);
    let mut g = DspGraph::new();
    let mut strips = Vec::new();
    for name in ["kick", "snare"] {
        let s = g.source(name).unwrap();
        let s = g.biquad(section(&mut rng), s).unwrap();
        let s = g.biquad(section(&mut rng), s).unwrap();
        let s = g.gain(0.8, s).unwrap();
        strips.push(g.pan(rng.f32(1.0), s).unwrap());
    }
    let bus = g.mix(strips[0], strips[1]).unwrap();
    let root = g.gain(0.5, bus).unwrap();
    let fused = g.fuse(root).unwrap();
    let report = fused.report().unwrap();
    assert_eq!(report.passes_before, 10);
    // One loop per strip, the mix, and the bus gain.
    assert_eq!(report.passes_after, 4, "{}", fused.term());
    let before = g.schedule(root).unwrap();
    let after = fused.schedule().unwrap();
    eprintln!(
        "Resonance buffers: unfused {}, fused {}",
        before.buffer_count, after.buffer_count
    );
    // Linear-scan buffer reuse already keeps the unfused chain at a few
    // buffers; fusion removes scheduled passes, and never adds buffers.
    assert!(after.buffer_count <= before.buffer_count);
    assert_eq!((before.ops.len(), after.ops.len()), (12, 6));
    let mut env = DspEnv::new();
    env.source("kick", block(&mut rng, 2048))
        .source("snare", block(&mut rng, 2048));
    let reference = g.evaluate(root, &env).unwrap();
    let out = fused.evaluate(&env).unwrap();
    assert!(max_diff(&reference, &out) <= filter_tolerance(peak(&reference)));
}

#[test]
fn malformed_dsp_inputs_are_typed_errors() {
    assert_eq!(
        Biquad::new([1.0, 0.0, 0.0], [0.0, 1.0]),
        Err(Error::Parameter("biquad poles outside the unit circle"))
    );
    assert_eq!(
        Biquad::new([1.0, 0.0, 0.0], [-2.1, 0.9]),
        Err(Error::Parameter("biquad poles outside the unit circle"))
    );
    assert_eq!(
        Biquad::new([f32::NAN, 0.0, 0.0], [0.0, 0.0]),
        Err(Error::NonFinite)
    );
    let mut g = DspGraph::new();
    let s = g.source("in").unwrap();
    let t = g.source("other").unwrap();
    assert!(matches!(g.source("in"), Err(Error::InvalidName(_))));
    assert!(matches!(g.pan(1.5, s), Err(Error::Parameter(_))));
    assert_eq!(g.gain(f32::INFINITY, s), Err(Error::NonFinite));
    // Bypassing `Biquad::new` through the public fields is still checked.
    let unstable = Biquad {
        b: [1.0, 0.0, 0.0],
        a: [0.0, 1.5],
    };
    assert!(matches!(g.biquad(unstable, s), Err(Error::Parameter(_))));
    let mut other = DspGraph::new();
    let foreign = other.source("x").unwrap();
    assert_eq!(g.gain(1.0, foreign), Err(Error::InvalidNode));

    let root = g.mix(s, t).unwrap();
    let fused = g.fuse(root).unwrap();
    let mut env = DspEnv::new();
    assert!(matches!(fused.evaluate(&env), Err(Error::Missing(_))));
    env.source(
        "in",
        Stereo {
            left: vec![0.0; 4],
            right: vec![0.0; 4],
        },
    );
    env.source(
        "other",
        Stereo {
            left: vec![0.0; 5],
            right: vec![0.0; 5],
        },
    );
    assert_eq!(
        g.evaluate(root, &env),
        Err(Error::Shape("mix block lengths"))
    );
    assert_eq!(fused.evaluate(&env), Err(Error::Shape("mix block lengths")));
    env.source(
        "other",
        Stereo {
            left: vec![0.0; 4],
            right: vec![0.0; 3],
        },
    );
    assert!(matches!(fused.evaluate(&env), Err(Error::Shape(_))));
    env.source(
        "other",
        Stereo {
            left: vec![f32::NAN; 4],
            right: vec![0.0; 4],
        },
    );
    assert_eq!(fused.evaluate(&env), Err(Error::NonFinite));
}

#[test]
fn gain_composition_never_leaves_the_fp32_range() {
    // Overflowing product: 2^64 * 2^64 is infinite, but the staged result
    // 2^-120 * 2^64 * 2^64 = 2^8 is finite. Underflowing product:
    // 1e-30 * 1e-30 flushes to zero, but 1e30 * 1e-30 * 1e-30 is 1e-30.
    for (g, x) in [(2f32.powi(64), 2f32.powi(-120)), (1e-30, 1e30)] {
        for with_biquad in [false, true] {
            let mut g_ = DspGraph::new();
            let src = g_.source("in").unwrap();
            let mut cur = src;
            if with_biquad {
                cur = g_
                    .biquad(Biquad::new([1.0, 0.5, 0.25], [-0.5, 0.25]).unwrap(), cur)
                    .unwrap();
            }
            let a = g_.gain(g, cur).unwrap();
            let root = g_.gain(g, a).unwrap();
            let fused = g_.fuse(root).unwrap();
            assert_eq!(fused.report().unwrap().passes_after, 1, "{}", fused.term());
            let mut env = DspEnv::new();
            env.source(
                "in",
                Stereo {
                    left: vec![x, -x, 0.0],
                    right: vec![x; 3],
                },
            );
            let reference = g_.evaluate(root, &env).unwrap();
            let out = fused.evaluate(&env).unwrap();
            assert!(peak(&reference) > 0.0);
            let diff = max_diff(&reference, &out);
            assert!(
                diff <= 1e-5 * peak(&reference),
                "{g} {x} {with_biquad}: {reference:?} vs {out:?}"
            );
        }
    }
}

#[test]
fn cloned_graphs_do_not_share_handles() {
    let mut a = DspGraph::new();
    let s = a.source("in").unwrap();
    let mut b = a.clone();
    let x = a.gain(2.0, s).unwrap();
    b.gain(3.0, s).unwrap_err();
    assert!(matches!(b.fuse(x), Err(Error::InvalidNode)));
    let mut env = DspEnv::new();
    env.source(
        "in",
        Stereo {
            left: vec![1.0],
            right: vec![1.0],
        },
    );
    assert_eq!(b.evaluate(x, &env), Err(Error::InvalidNode));
}
