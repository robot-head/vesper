//! Tensor graph fusion: the fused term is compared with the unfused
//! reference on random inputs. Every rule except `rmsnorm-prologue` must be
//! bit-identical; `rmsnorm-prologue` must stay within `rmsnorm_tolerance`.
mod common;

use common::Rng;
use vesper_fusion::Error;
use vesper_fusion::tensor::{Activation, DType, TensorEnv, TensorGraph, Value, rmsnorm_tolerance};
use vesper_tensor::quant::{Matrix, WeightFormat};

fn bits(v: &Value) -> Vec<u32> {
    match v {
        Value::F32(v) => v.iter().map(|x| x.to_bits()).collect(),
        Value::I32(v) => v.iter().map(|x| *x as u32).collect(),
    }
}

fn dense(rng: &mut Rng, rows: usize, cols: usize) -> Matrix {
    Matrix::f32(rows, cols, rng.vec(rows * cols, 0.5)).unwrap()
}

/// relu(W x + b) with W in `format`.
fn mlp(format: WeightFormat, act: Activation) -> (TensorGraph, vesper_fusion::tensor::Tensor) {
    let mut g = TensorGraph::new();
    let x = g.input("x", DType::F32, 64).unwrap();
    let b = g.input("b", DType::F32, 24).unwrap();
    let w = g.matrix("w", 24, 64, format).unwrap();
    let y = g.matvec(w, x).unwrap();
    let y = g.add(y, b).unwrap();
    let y = g.activation(act, y).unwrap();
    (g, y)
}

#[test]
fn matmul_bias_activation_is_one_kernel_and_bit_identical() {
    for act in [Activation::Relu, Activation::Silu] {
        let (g, root) = mlp(WeightFormat::F32, act);
        let fused = g.fuse(root).unwrap();
        let report = fused.report();
        assert_eq!(report.kernels_before, 3);
        assert_eq!(report.kernels_after, 1, "{}", fused.term());
        assert!(report.cost_after < report.cost_before);
        assert_eq!(fused.count("Fuse"), 1);
        assert_eq!(fused.count("Dot"), 1);
        assert_eq!(fused.count("MatVec"), 0);
        let mut rng = Rng(0x5eed_1001);
        for _ in 0..40 {
            let mut env = TensorEnv::new();
            env.vector_f32("x", rng.vec(64, 2.0))
                .vector_f32("b", rng.vec(24, 1.0))
                .matrix("w", dense(&mut rng, 24, 64));
            let reference = g.evaluate(root, &env).unwrap();
            let out = fused.evaluate(&env).unwrap();
            assert_eq!(bits(&reference), bits(&out), "{act:?}");
        }
    }
}

#[test]
fn dequantize_into_matmul_is_exact_for_q8_and_q4() {
    for format in [WeightFormat::Q8_0, WeightFormat::Q4_0] {
        let (g, root) = mlp(format, Activation::Silu);
        let fused = g.fuse(root).unwrap();
        let report = fused.report();
        assert_eq!(report.materialized_dequant, 0, "{}", fused.term());
        assert_eq!(fused.count("QView"), 1);
        assert_eq!(report.kernels_after, 1);
        let mut rng = Rng(0x5eed_1002);
        for _ in 0..30 {
            let w = dense(&mut rng, 24, 64).quantize(format).unwrap();
            let mut env = TensorEnv::new();
            env.vector_f32("x", rng.vec(64, 3.0))
                .vector_f32("b", rng.vec(24, 1.0))
                .matrix("w", w);
            let reference = g.evaluate(root, &env).unwrap();
            assert_eq!(
                bits(&reference),
                bits(&fused.evaluate(&env).unwrap()),
                "{format:?}"
            );
        }
    }
    // A bare quantized matvec also streams its dequantization.
    let mut g = TensorGraph::new();
    let x = g.input("x", DType::F32, 32).unwrap();
    let w = g.matrix("w", 8, 32, WeightFormat::Q4_0).unwrap();
    let y = g.matvec(w, x).unwrap();
    let fused = g.fuse(y).unwrap();
    assert_eq!(
        (fused.count("Deq"), fused.count("QView")),
        (0, 1),
        "{}",
        fused.term()
    );
    let mut rng = Rng(9);
    let mut env = TensorEnv::new();
    env.vector_f32("x", rng.vec(32, 1.0)).matrix(
        "w",
        dense(&mut rng, 8, 32).quantize(WeightFormat::Q4_0).unwrap(),
    );
    assert_eq!(
        bits(&g.evaluate(y, &env).unwrap()),
        bits(&fused.evaluate(&env).unwrap())
    );
}

#[test]
fn elementwise_chain_collapses_to_one_pass_and_is_bit_identical() {
    let mut g = TensorGraph::new();
    let x = g.input("x", DType::F32, 257).unwrap();
    let y = g.input("y", DType::F32, 257).unwrap();
    let z = g.input("z", DType::F32, 257).unwrap();
    let a = g.scale_f32(0.75, x).unwrap();
    let b = g.add(a, y).unwrap();
    let c = g.mul(b, z).unwrap();
    let d = g.activation(Activation::Silu, c).unwrap();
    let e = g.add(d, a).unwrap(); // `a` is shared: a diamond
    let root = g.activation(Activation::Relu, e).unwrap();
    let fused = g.fuse(root).unwrap();
    assert_eq!(fused.report().kernels_before, 6);
    assert_eq!(fused.report().kernels_after, 1, "{}", fused.term());
    let mut rng = Rng(0x5eed_1003);
    for _ in 0..40 {
        let mut env = TensorEnv::new();
        env.vector_f32("x", rng.vec(257, 4.0))
            .vector_f32("y", rng.vec(257, 4.0))
            .vector_f32("z", rng.vec(257, 4.0));
        assert_eq!(
            bits(&g.evaluate(root, &env).unwrap()),
            bits(&fused.evaluate(&env).unwrap())
        );
    }
}

#[test]
fn swiglu_feed_forward_fuses_both_projections() {
    // silu(W1 x) * (W3 x), then W2 of that, plus a residual.
    let mut g = TensorGraph::new();
    let x = g.input("x", DType::F32, 32).unwrap();
    let w1 = g.matrix("w1", 64, 32, WeightFormat::Q8_0).unwrap();
    let w3 = g.matrix("w3", 64, 32, WeightFormat::Q8_0).unwrap();
    let w2 = g.matrix("w2", 32, 64, WeightFormat::Q8_0).unwrap();
    let h1 = g.matvec(w1, x).unwrap();
    let h1 = g.activation(Activation::Silu, h1).unwrap();
    let h3 = g.matvec(w3, x).unwrap();
    let h = g.mul(h1, h3).unwrap();
    let o = g.matvec(w2, h).unwrap();
    let root = g.add(o, x).unwrap();
    let fused = g.fuse(root).unwrap();
    // Two passes: the gated hidden vector, then W2 with the residual epilogue.
    assert_eq!(fused.report().kernels_before, 6);
    assert_eq!(fused.report().kernels_after, 2, "{}", fused.term());
    assert_eq!(fused.count("Dot"), 3);
    assert_eq!(fused.report().materialized_dequant, 0);
    let mut rng = Rng(0x5eed_1004);
    for _ in 0..20 {
        let mut env = TensorEnv::new();
        env.vector_f32("x", rng.vec(32, 2.0))
            .matrix(
                "w1",
                dense(&mut rng, 64, 32)
                    .quantize(WeightFormat::Q8_0)
                    .unwrap(),
            )
            .matrix(
                "w3",
                dense(&mut rng, 64, 32)
                    .quantize(WeightFormat::Q8_0)
                    .unwrap(),
            )
            .matrix(
                "w2",
                dense(&mut rng, 32, 64)
                    .quantize(WeightFormat::Q8_0)
                    .unwrap(),
            );
        assert_eq!(
            bits(&g.evaluate(root, &env).unwrap()),
            bits(&fused.evaluate(&env).unwrap())
        );
    }
}

#[test]
fn rmsnorm_prologue_fuses_within_the_stated_tolerance() {
    for format in [WeightFormat::F32, WeightFormat::Q4_0] {
        let (n, rows) = (96, 40);
        let mut g = TensorGraph::new();
        let x = g.input("x", DType::F32, n).unwrap();
        let nw = g.input("nw", DType::F32, n).unwrap();
        let w = g.matrix("w", rows, n, format).unwrap();
        let normed = g.rms_norm(x, nw, 1e-5).unwrap();
        let root = g.matvec(w, normed).unwrap();
        let fused = g.fuse(root).unwrap();
        assert_eq!(fused.count("RmsMatVec"), 1, "{}", fused.term());
        assert_eq!(fused.report().kernels_after, 1);
        let mut rng = Rng(0x5eed_1005);
        let mut worst = 0.0f32;
        for _ in 0..30 {
            let xs = rng.vec(n, 3.0);
            let nws = rng.vec(n, 1.5);
            let m = dense(&mut rng, rows, n);
            let m = if format == WeightFormat::F32 {
                m
            } else {
                m.quantize(format).unwrap()
            };
            let mut env = TensorEnv::new();
            env.vector_f32("x", xs.clone())
                .vector_f32("nw", nws.clone())
                .matrix("w", m.clone());
            let Value::F32(reference) = g.evaluate(root, &env).unwrap() else {
                panic!()
            };
            let Value::F32(out) = fused.evaluate(&env).unwrap() else {
                panic!()
            };
            let ss = xs.iter().map(|v| v * v).sum::<f32>() / n as f32 + 1e-5;
            let inv = 1.0 / ss.sqrt();
            let mut row = vec![0.0; n];
            for r in 0..rows {
                m.dequantize_row(r, &mut row).unwrap();
                let abs: f32 = (0..n).map(|c| (row[c] * nws[c] * xs[c]).abs()).sum();
                let tol = rmsnorm_tolerance(abs, inv, n);
                let diff = (reference[r] - out[r]).abs();
                assert!(
                    diff <= tol,
                    "{format:?} row {r}: |{} - {}| = {diff} > {tol}",
                    reference[r],
                    out[r]
                );
                worst = worst.max(diff / tol.max(f32::MIN_POSITIVE));
            }
        }
        eprintln!("{format:?}: worst error / tolerance = {worst}");
    }
}

#[test]
fn integer_chains_agree_exactly_and_overflow_identically() {
    let mut g = TensorGraph::new();
    let a = g.input("a", DType::I32, 100).unwrap();
    let b = g.input("b", DType::I32, 100).unwrap();
    let s = g.scale_i32(-3, a).unwrap();
    let t = g.add(s, b).unwrap();
    let u = g.mul(t, b).unwrap();
    let root = g.activation(Activation::Relu, u).unwrap();
    let fused = g.fuse(root).unwrap();
    assert_eq!(fused.report().kernels_after, 1, "{}", fused.term());
    let mut rng = Rng(0x5eed_1006);
    for _ in 0..50 {
        let mut env = TensorEnv::new();
        env.vector_i32("a", (0..100).map(|_| rng.i32(1000)).collect())
            .vector_i32("b", (0..100).map(|_| rng.i32(1000)).collect());
        let reference = g.evaluate(root, &env).unwrap();
        assert_eq!(reference, fused.evaluate(&env).unwrap());
    }
    let mut env = TensorEnv::new();
    env.vector_i32("a", vec![i32::MAX / 2; 100])
        .vector_i32("b", vec![1; 100]);
    assert_eq!(g.evaluate(root, &env), Err(Error::Overflow));
    assert_eq!(fused.evaluate(&env), Err(Error::Overflow));
}

#[test]
fn malformed_graphs_and_environments_are_typed_errors() {
    let mut g = TensorGraph::new();
    let x = g.input("x", DType::F32, 8).unwrap();
    let i = g.input("i", DType::I32, 8).unwrap();
    assert!(matches!(
        g.input("x", DType::F32, 8),
        Err(Error::InvalidName(_))
    ));
    assert!(matches!(
        g.input("bad name", DType::F32, 8),
        Err(Error::InvalidName(_))
    ));
    assert!(matches!(
        g.input("q\"", DType::F32, 8),
        Err(Error::InvalidName(_))
    ));
    assert_eq!(
        g.input("z", DType::F32, 0),
        Err(Error::Limit("vector length"))
    );
    assert_eq!(g.add(x, i), Err(Error::Shape("elementwise operand types")));
    assert_eq!(
        g.activation(Activation::Silu, i),
        Err(Error::Unsupported("silu over i32"))
    );
    assert_eq!(g.scale_f32(f32::NAN, x), Err(Error::NonFinite));
    assert_eq!(
        g.rms_norm(x, x, 0.0),
        Err(Error::Parameter("rms_norm epsilon"))
    );
    assert!(matches!(
        g.matrix("q", 4, 20, WeightFormat::Q8_0),
        Err(Error::Shape(_))
    ));
    let w = g.matrix("w", 4, 16, WeightFormat::F32).unwrap();
    assert_eq!(g.matvec(w, x), Err(Error::Shape("matvec inner dimension")));
    // A handle from another graph is rejected.
    let mut other = TensorGraph::new();
    let foreign = other.input("x", DType::F32, 8).unwrap();
    assert_eq!(g.add(x, foreign), Err(Error::InvalidNode));
    assert!(matches!(g.fuse(foreign), Err(Error::InvalidNode)));

    let y = g.add(x, x).unwrap();
    let fused = g.fuse(y).unwrap();
    let mut env = TensorEnv::new();
    assert_eq!(g.evaluate(y, &env), Err(Error::Missing("x".into())));
    assert_eq!(fused.evaluate(&env), Err(Error::Missing("x".into())));
    env.vector_f32("x", vec![1.0; 7]);
    assert!(matches!(fused.evaluate(&env), Err(Error::Shape(_))));
    env.vector_f32("x", vec![f32::INFINITY; 8]);
    assert_eq!(fused.evaluate(&env), Err(Error::NonFinite));
    env.vector_f32("x", vec![f32::MAX; 8]);
    assert_eq!(g.evaluate(y, &env), Err(Error::NonFinite));
    assert_eq!(fused.evaluate(&env), Err(Error::NonFinite));
    env.vector_i32("x", vec![1; 8]);
    assert!(matches!(fused.evaluate(&env), Err(Error::Shape(_))));

    // A matrix whose format differs from its declaration is rejected.
    let mut g = TensorGraph::new();
    let x = g.input("x", DType::F32, 32).unwrap();
    let w = g.matrix("w", 4, 32, WeightFormat::Q8_0).unwrap();
    let y = g.matvec(w, x).unwrap();
    let fused = g.fuse(y).unwrap();
    let mut env = TensorEnv::new();
    env.vector_f32("x", vec![0.5; 32])
        .matrix("w", Matrix::f32(4, 32, vec![0.1; 128]).unwrap());
    assert!(matches!(fused.evaluate(&env), Err(Error::Shape(_))));
}

#[test]
fn long_chains_stay_within_budget() {
    let mut g = TensorGraph::new();
    let x = g.input("x", DType::F32, 16).unwrap();
    let mut cur = x;
    for i in 0..300 {
        cur = if i % 3 == 0 {
            g.scale_f32(0.999, cur).unwrap()
        } else if i % 3 == 1 {
            g.add(cur, x).unwrap()
        } else {
            g.activation(Activation::Relu, cur).unwrap()
        };
    }
    let start = std::time::Instant::now();
    let fused = g.fuse(cur).unwrap();
    eprintln!("300-node chain fused in {:?}", start.elapsed());
    assert_eq!(fused.report().kernels_after, 1);
    let mut env = TensorEnv::new();
    env.vector_f32("x", Rng(3).vec(16, 1.0));
    assert_eq!(
        bits(&g.evaluate(cur, &env).unwrap()),
        bits(&fused.evaluate(&env).unwrap())
    );
}

#[test]
fn cloned_graphs_do_not_share_handles() {
    let mut a = TensorGraph::new();
    let x = a.input("x", DType::F32, 4).unwrap();
    let mut b = a.clone();
    let y = a.scale_f32(2.0, x).unwrap();
    assert_eq!(b.scale_f32(3.0, x), Err(Error::InvalidNode));
    let mut env = TensorEnv::new();
    env.vector_f32("x", vec![1.0; 4]);
    assert_eq!(b.evaluate(y, &env), Err(Error::InvalidNode));
    assert!(matches!(b.fuse(y), Err(Error::InvalidNode)));
}
