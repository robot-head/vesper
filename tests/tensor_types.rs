//! Source tensor types (§28.2): accepted programs run on the evaluator and
//! match direct `vesper-tensor` host-kernel calls on the same inputs.
use vesper::tensor_types::{
    self,
    eval::{self, HostData, HostTensor, HostValue, Options},
};
use vesper_tensor::attention::{AttentionMode, AttentionShape, paged_gqa_attention};
use vesper_tensor::kv::{Bf16Step, KvConfig, KvFormat, KvPool};
use vesper_tensor::ops;
use vesper_tensor::quant::{self, Matrix, WeightFormat, decode_bf16, encode_bf16};

fn analyze(src: &str) -> tensor_types::Analysis {
    let parsed = vesper::parser::parse(src);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let analysis = tensor_types::analyze(&parsed.module);
    assert!(
        analysis.checked.diagnostics.is_empty(),
        "{:#?}",
        analysis.checked.diagnostics
    );
    analysis
}
fn t(dims: &[usize], data: Vec<f32>) -> HostValue {
    HostValue::Tensor(HostTensor::f32(dims.to_vec(), data))
}
fn values(n: usize, seed: u32) -> Vec<f32> {
    // Small deterministic values with mixed signs.
    (0..n)
        .map(|i| {
            let x = (i as u32)
                .wrapping_mul(2_654_435_761)
                .wrapping_add(seed.wrapping_mul(40_503));
            ((x >> 16) % 2001) as f32 / 1000.0 - 1.0
        })
        .collect()
}
fn tensor(v: &HostValue) -> &HostTensor {
    match v {
        HostValue::Tensor(t) => t,
        other => panic!("expected a tensor, found {other:?}"),
    }
}
fn f32s(v: &HostValue) -> &[f32] {
    tensor(v).as_f32().expect("an F32 tensor")
}
fn ok(v: &HostValue) -> &HostValue {
    match v {
        HostValue::Data {
            ty,
            constructor,
            fields,
        } if ty == "Result" && constructor == "Ok" => &fields["0"],
        other => panic!("expected Ok(..), found {other:?}"),
    }
}
fn bits(a: &[f32]) -> Vec<u32> {
    a.iter().map(|v| v.to_bits()).collect()
}
/// Reference `[M, K] x [K, N]` through the same host kernel (`Matrix::matvec`).
fn matmul(a: &[f32], b: &[f32], m: usize, k: usize, n: usize) -> Vec<f32> {
    let mut bt = vec![0.0; k * n];
    for r in 0..k {
        for c in 0..n {
            bt[c * k + r] = b[r * n + c];
        }
    }
    let w = Matrix::f32(n, k, bt).unwrap();
    let mut out = vec![0.0; m * n];
    for row in 0..m {
        w.matvec(&a[row * k..(row + 1) * k], &mut out[row * n..(row + 1) * n])
            .unwrap();
    }
    out
}
/// Naive reference, accumulated in ascending `k` like the kernel.
fn naive_matmul(a: &[f32], b: &[f32], m: usize, k: usize, n: usize) -> Vec<f32> {
    let mut out = vec![0.0; m * n];
    for i in 0..m {
        for j in 0..n {
            let mut s = 0.0f32;
            for x in 0..k {
                s += a[i * k + x] * b[x * n + j];
            }
            out[i * n + j] = s;
        }
    }
    out
}

#[test]
fn projection_matches_host_matmul_and_broadcast() {
    let a = analyze(include_str!("fixtures/tensor_types/projection.ves"));
    let (x, w, b) = (values(5 * 3, 1), values(3 * 2, 2), values(2, 3));
    let out = eval::run(
        &a,
        "main",
        vec![
            t(&[5, 3], x.clone()),
            t(&[3, 2], w.clone()),
            t(&[2], b.clone()),
        ],
        Options::default(),
    )
    .unwrap();
    let mut want = naive_matmul(&x, &w, 5, 3, 2);
    for (i, v) in want.iter_mut().enumerate() {
        *v += b[i % 2];
    }
    assert_eq!(tensor(&out.value).dims, vec![5, 2]);
    assert_eq!(bits(f32s(&out.value)), bits(&want));
    assert_eq!(out.stats.dims["B"], 5);
    // The same program at another batch size, with no recompilation.
    let out = eval::run(
        &a,
        "main",
        vec![t(&[1, 3], x[..3].to_vec()), t(&[3, 2], w), t(&[2], b)],
        Options::default(),
    )
    .unwrap();
    assert_eq!(tensor(&out.value).dims, vec![1, 2]);
}

#[test]
fn decoder_pieces_match_vesper_tensor_ops() {
    let a = analyze(include_str!("fixtures/tensor_types/layer.ves"));
    let (h, d, f) = (2usize, 4usize, 6usize);
    let n = h * d;
    let x = values(n, 10);
    let w = values(n, 11);
    let w1 = values(f * n, 12);
    let w3 = values(f * n, 13);
    let w2 = values(n * f, 14);
    let out = eval::run(
        &a,
        "layer",
        vec![
            t(&[h, d], x.clone()),
            t(&[n], w.clone()),
            t(&[f, n], w1.clone()),
            t(&[f, n], w3.clone()),
            t(&[n, f], w2.clone()),
            HostValue::U64(3),
        ],
        Options::default(),
    )
    .unwrap();
    // Reference: the same kernels called directly.
    let mut normed = vec![0.0; n];
    ops::rms_norm(&mut normed, &x, &w, 0.00001).unwrap();
    ops::rope(&mut normed, d, 3, eval::ROPE_BASE).unwrap();
    let (mut gate, mut up, mut hidden) = (vec![0.0; f], vec![0.0; f], vec![0.0; n]);
    Matrix::f32(f, n, w1)
        .unwrap()
        .matvec(&normed, &mut gate)
        .unwrap();
    Matrix::f32(f, n, w3)
        .unwrap()
        .matvec(&normed, &mut up)
        .unwrap();
    ops::swiglu(&mut gate, &up).unwrap();
    Matrix::f32(n, f, w2)
        .unwrap()
        .matvec(&gate, &mut hidden)
        .unwrap();
    let mut probs = hidden.clone();
    ops::softmax(&mut probs).unwrap();
    let HostValue::Tuple(parts) = &out.value else {
        panic!("{:?}", out.value)
    };
    assert_eq!(bits(f32s(&parts[0])), bits(&hidden));
    assert_eq!(
        parts[1],
        HostValue::U64(ops::argmax(&probs).unwrap() as u64)
    );
    // RoPE's even-width rule became a precondition of the generic function.
    let pre = &a.signatures["norm_rope"].obligations;
    assert_eq!(pre.len(), 1, "{pre:?}");
    assert!(
        pre.iter()
            .next()
            .unwrap()
            .to_string()
            .contains("D must be even")
    );
}

#[test]
fn elementwise_broadcast_and_conversions_match() {
    let a = analyze(include_str!("fixtures/tensor_types/layer.ves"));
    let (r, c) = (3usize, 4usize);
    let (m, row, col) = (values(r * c, 20), values(c, 21), values(r, 22));
    let out = eval::run(
        &a,
        "elementwise",
        vec![
            t(&[r, c], m.clone()),
            t(&[1, c], row.clone()),
            t(&[r, 1], col.clone()),
        ],
        Options::default(),
    )
    .unwrap();
    let mut want = vec![0.0; r * c];
    for i in 0..r {
        for j in 0..c {
            let s = (m[i * c + j] + row[j]) * col[i];
            let g = decode_bf16(encode_bf16(ops::silu(s)));
            want[j * r + i] = g + 0.5;
        }
    }
    assert_eq!(tensor(&out.value).dims, vec![c, r]);
    assert_eq!(bits(f32s(&out.value)), bits(&want));
}

#[test]
fn gqa_decode_matches_paged_attention() {
    let a = analyze(include_str!("fixtures/tensor_types/attention.ves"));
    let (qh, kv, hd) = (4usize, 2usize, 8usize);
    let q = values(qh * hd, 30);
    let ks = [values(kv * hd, 31), values(kv * hd, 33)];
    let vs = [values(kv * hd, 32), values(kv * hd, 34)];
    let out = eval::run(
        &a,
        "generate",
        vec![
            t(&[qh, hd], q.clone()),
            t(&[kv, hd], ks[0].clone()),
            t(&[kv, hd], vs[0].clone()),
            t(&[kv, hd], ks[1].clone()),
            t(&[kv, hd], vs[1].clone()),
        ],
        Options::default(),
    )
    .unwrap();
    // Reference: the pool and attention kernel called directly.
    let mut pool = KvPool::with_format(
        KvConfig {
            heads: kv,
            head_dim: hd,
            pages: 64,
            max_sequences: 16,
            max_tokens_per_sequence: 64 * 16,
            max_bytes: 64 * 1024 * 1024,
        },
        KvFormat::Bf16,
    )
    .unwrap();
    let mut seq = pool.begin().unwrap();
    let mut want = vec![];
    for step in 0..2 {
        let enc = |v: &[f32]| -> Vec<u8> {
            v.iter()
                .flat_map(|x| encode_bf16(*x).to_le_bytes())
                .collect()
        };
        let (kb, vb) = (enc(&ks[step]), enc(&vs[step]));
        let s = |bytes| Bf16Step {
            heads: kv,
            head_dim: hd,
            bytes,
        };
        pool.append(&mut seq, step, s(&kb), s(&vb)).unwrap();
        let mut rq = q.clone();
        ops::rope(&mut rq, hd, step + 1, eval::ROPE_BASE).unwrap();
        let mut scores = vec![0.0; qh * (step + 1)];
        want = vec![0.0; qh * hd];
        let shape = AttentionShape {
            query_heads: qh,
            kv_heads: kv,
            head_dim: hd,
        };
        paged_gqa_attention(
            &pool,
            &seq,
            shape,
            &rq,
            &mut scores,
            &mut want,
            AttentionMode::TwoPass,
        )
        .unwrap();
    }
    pool.release(seq).unwrap();
    assert_eq!(bits(f32s(ok(&out.value))), bits(&want));
    assert_eq!(out.stats.sequences_begun, 1);
    assert_eq!(out.stats.sequences_released, 1);
    assert_eq!(out.stats.allocated_pages_at_exit, 0);
    // The GQA rule became a precondition of the generic decode step.
    let pre: Vec<String> = a.signatures["decode_step"]
        .obligations
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        pre.iter()
            .any(|p| p.contains("QH must be a multiple of KV")),
        "{pre:?}"
    );
}

fn q_bytes(format: WeightFormat, rows: usize, cols: usize, seed: u32) -> (Vec<f32>, Vec<u8>) {
    let data = values(rows * cols, seed);
    let bytes = match format {
        WeightFormat::Q4_0 => quant::quantize_q4_0(&data, cols).unwrap(),
        _ => quant::quantize_q8_0(&data, cols).unwrap(),
    };
    (data, bytes)
}

#[test]
fn quantized_wire_blocks_match_host_matvec() {
    let a = analyze(include_str!("fixtures/tensor_types/quantized.ves"));
    assert_eq!(a.wires["Q4Block32Wire"].layout.size(), quant::Q4_0_BYTES);
    assert_eq!(a.wires["Q8Block32Wire"].layout.size(), quant::Q8_0_BYTES);
    let (rows, cols) = (3usize, 64usize);
    let (_, b4) = q_bytes(WeightFormat::Q4_0, rows, cols, 40);
    let (_, b8) = q_bytes(WeightFormat::Q8_0, rows, cols, 41);
    let x = values(cols, 42);
    let out = eval::run(
        &a,
        "project",
        vec![
            HostValue::Tensor(HostTensor::quantized(
                "Q4Block32Wire",
                rows,
                cols,
                b4.clone(),
            )),
            HostValue::Tensor(HostTensor::quantized(
                "Q8Block32Wire",
                rows,
                cols,
                b8.clone(),
            )),
            t(&[cols], x.clone()),
        ],
        Options::default(),
    )
    .unwrap();
    let mut y4 = vec![0.0; rows];
    let mut y8 = vec![0.0; rows];
    Matrix::quantized(WeightFormat::Q4_0, rows, cols, b4.clone())
        .unwrap()
        .matvec(&x, &mut y4)
        .unwrap();
    Matrix::quantized(WeightFormat::Q8_0, rows, cols, b8)
        .unwrap()
        .matvec(&x, &mut y8)
        .unwrap();
    let HostValue::Tuple(parts) = &out.value else {
        panic!("{:?}", out.value)
    };
    assert_eq!(bits(f32s(&parts[0])), bits(&y4));
    assert_eq!(bits(f32s(&parts[1])), bits(&y8));
    // Block (row 1, block 1) is the fourth 18-byte block; its scale is the
    // first two bytes, read through the wire layout.
    let off = 3 * quant::Q4_0_BYTES;
    let scale = vesper_tensor::host::decode_fp16(u16::from_le_bytes([b4[off], b4[off + 1]]));
    assert_eq!(parts[2], HostValue::F64(f64::from(scale)));
}

#[test]
fn page_leases_fill_to_sixteen_tokens_and_are_returned() {
    let a = analyze(include_str!("fixtures/tensor_types/pages.ves"));
    for (writes, want) in [(3u64, 3u64), (16, 16), (40, 16)] {
        let out = eval::run(
            &a,
            "pages",
            vec![t(&[2, 4], values(8, 50)), HostValue::U64(writes)],
            Options::default(),
        )
        .unwrap();
        assert_eq!(out.value, HostValue::U64(want));
        assert_eq!(out.stats.pages_leased, 1);
        assert_eq!(out.stats.pages_released, 1);
        assert_eq!(out.stats.allocated_pages_at_exit, 0);
    }
    // An exhausted pool takes the error path, which also returns no lease.
    let out = eval::run(
        &a,
        "pages",
        vec![t(&[2, 4], values(8, 50)), HostValue::U64(1)],
        Options {
            max_sequences: 1,
            pages: 1,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(out.value, HostValue::U64(1));
}

#[test]
fn records_branches_and_product_dimensions_infer() {
    let a = analyze(include_str!("fixtures/tensor_types/generic.ves"));
    let bsz = 3usize;
    let (x, w1, b1, w2, b2) = (
        values(bsz * 4, 60),
        values(4 * 6, 61),
        values(6, 62),
        values(6 * 2, 63),
        values(2, 64),
    );
    let out = eval::run(
        &a,
        "chain",
        vec![
            t(&[bsz, 4], x.clone()),
            t(&[4, 6], w1.clone()),
            t(&[6], b1.clone()),
            t(&[6, 2], w2.clone()),
            t(&[2], b2.clone()),
        ],
        Options::default(),
    )
    .unwrap();
    let mut h = matmul(&x, &w1, bsz, 4, 6);
    for (i, v) in h.iter_mut().enumerate() {
        *v += b1[i % 6];
    }
    let mut y = matmul(&h, &w2, bsz, 6, 2);
    for (i, v) in y.iter_mut().enumerate() {
        *v += b2[i % 2];
    }
    ops::rope(&mut y, 2 * bsz, 3, eval::ROPE_BASE).unwrap();
    assert_eq!(tensor(&out.value).dims, vec![1, 2 * bsz]);
    assert_eq!(bits(f32s(&out.value)), bits(&y));
}

#[test]
fn entry_arguments_are_checked_against_the_signature() {
    let a = analyze(include_str!("fixtures/tensor_types/projection.ves"));
    let run = |args| eval::run(&a, "main", args, Options::default());
    // K is 3 in the signature.
    let e = run(vec![
        t(&[2, 4], values(8, 1)),
        t(&[3, 2], values(6, 2)),
        t(&[2], values(2, 3)),
    ])
    .unwrap_err();
    assert!(matches!(e, eval::EvalError::Entry(_)), "{e:?}");
    // Element count disagrees with the dimensions.
    let e = run(vec![
        t(&[2, 3], values(5, 1)),
        t(&[3, 2], values(6, 2)),
        t(&[2], values(2, 3)),
    ])
    .unwrap_err();
    assert!(matches!(e, eval::EvalError::Entry(_)), "{e:?}");
    // Wrong element type.
    let bf = HostValue::Tensor(HostTensor::bf16_from_f32(vec![2, 3], &values(6, 1)));
    let e = run(vec![bf, t(&[3, 2], values(6, 2)), t(&[2], values(2, 3))]).unwrap_err();
    assert!(matches!(e, eval::EvalError::Entry(_)), "{e:?}");
    // An entry precondition is checked with the concrete dimensions.
    let l = analyze(include_str!("fixtures/tensor_types/layer.ves"));
    let e = eval::run(
        &l,
        "norm_rope",
        vec![
            t(&[2, 3], values(6, 1)),
            t(&[6], values(6, 2)),
            HostValue::U64(0),
        ],
        Options::default(),
    )
    .unwrap_err();
    assert!(format!("{e:?}").contains("must be even"), "{e:?}");
    // Malformed quantized bytes are a typed kernel error, not a panic.
    let q = analyze(include_str!("fixtures/tensor_types/quantized.ves"));
    let e = eval::run(
        &q,
        "project",
        vec![
            HostValue::Tensor(HostTensor::quantized("Q4Block32Wire", 1, 32, vec![0; 5])),
            HostValue::Tensor(HostTensor::quantized("Q8Block32Wire", 1, 32, vec![0; 34])),
            t(&[32], values(32, 1)),
        ],
        Options::default(),
    )
    .unwrap_err();
    assert!(matches!(e, eval::EvalError::Kernel(_)), "{e:?}");
    let _ = HostData::F32(vec![]);
}

#[test]
fn modules_without_tensor_syntax_are_untouched() {
    let parsed = vesper::parser::parse("fn main() -> u64 { 1 }");
    assert!(tensor_types::check(&parsed.module).is_none());
    // A module that declares its own `Tensor` keeps it.
    let parsed = vesper::parser::parse(
        "copy struct Tensor { n: u64 } fn main() -> u64 { let t = Tensor { n: 1 }; t.n }",
    );
    assert!(!tensor_types::active(&parsed.module));
    assert!(tensor_types::check(&parsed.module).is_none());
}

#[test]
fn function_values_dtype_parameters_and_product_entries() {
    let a = analyze(include_str!("fixtures/tensor_types/values.ves"));
    let x = values(2, 70);
    let out = eval::run(&a, "through", vec![t(&[2], x.clone())], Options::default()).unwrap();
    let y: Vec<f32> = x.iter().map(|v| ops::silu(*v)).collect();
    assert_eq!(bits(f32s(&out.value)), bits(&y));
    // A dtype parameter takes the element type of the argument.
    let out = eval::run(&a, "same", vec![t(&[2], x.clone())], Options::default()).unwrap();
    assert_eq!(bits(f32s(&out.value)), bits(&x));
    let bf = HostValue::Tensor(HostTensor::bf16_from_f32(vec![2], &x));
    let out = eval::run(&a, "same", vec![bf.clone()], Options::default()).unwrap();
    assert_eq!(tensor(&out.value), tensor(&bf));
    // An output over the element limit is refused before it is allocated.
    let n = 1 << 16;
    let e = eval::run(
        &a,
        "mm",
        vec![t(&[n, 1], vec![1.0; n]), t(&[1, n], vec![1.0; n])],
        Options::default(),
    )
    .unwrap_err();
    assert!(matches!(e, eval::EvalError::Limit(_)), "{e:?}");
    // `D` occurs only in the product `D * 2` and is solved from the shape.
    let g = analyze(include_str!("fixtures/tensor_types/generic.ves"));
    let x = values(8, 71);
    let out = eval::run(
        &g,
        "rotate",
        vec![t(&[1, 8], x.clone())],
        Options::default(),
    )
    .unwrap();
    let mut y = x;
    ops::rope(&mut y, 8, 3, eval::ROPE_BASE).unwrap();
    assert_eq!(bits(f32s(&out.value)), bits(&y));
    // An odd width has no solution.
    let e = eval::run(
        &g,
        "rotate",
        vec![t(&[1, 7], values(7, 1))],
        Options::default(),
    )
    .unwrap_err();
    assert!(matches!(e, eval::EvalError::Entry(_)), "{e:?}");
}
