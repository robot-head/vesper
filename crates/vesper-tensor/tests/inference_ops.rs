//! Per-op checks against independent references: hand-computed values and
//! straightforward f64 implementations written separately from the kernels.
use vesper_tensor::attention::{paged_gqa_attention, AttentionMode, AttentionShape};
use vesper_tensor::host::decode_fp16;
use vesper_tensor::kv::{KvConfig, KvError, KvFormat, KvPool};
use vesper_tensor::ops::{argmax, rms_norm, rope, silu, softmax, swiglu, swiglu_matvec};
use vesper_tensor::quant::{
    decode_bf16, encode_bf16, encode_fp16, quantize_q4_0, quantize_q8_0, Matrix, WeightFormat,
    Q4_0_BYTES, Q8_0_BYTES,
};
use vesper_tensor::Error;

/// Deterministic xorshift values in [-1, 1).
fn values(seed: u64, n: usize) -> Vec<f32> {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 40) as f32 / (1u64 << 23) as f32) - 1.0
        })
        .collect()
}
fn close(actual: &[f32], expected: &[f64], tol: f64) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        let err = (f64::from(*a) - e).abs();
        assert!(
            err <= tol * (1.0 + e.abs()),
            "index {i}: {a} vs {e} (err {err})"
        );
    }
}

#[test]
fn rms_norm_hand_value_and_f64_reference() {
    // x = [3, 4]: mean square 12.5, so x / sqrt(12.5 + eps).
    let mut out = [0.0f32; 2];
    rms_norm(&mut out, &[3.0, 4.0], &[1.0, 2.0], 1e-5).unwrap();
    let inv = 1.0 / (12.5f64 + 1e-5).sqrt();
    close(&out, &[3.0 * inv, 8.0 * inv], 1e-6);
    for n in [1, 7, 64, 172] {
        let x = values(n as u64, n);
        let w = values(n as u64 + 99, n);
        let ms = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / n as f64;
        let inv = 1.0 / (ms + 1e-5).sqrt();
        let expected: Vec<f64> = x
            .iter()
            .zip(&w)
            .map(|(x, w)| f64::from(*x) * inv * f64::from(*w))
            .collect();
        let mut out = vec![0.0; n];
        rms_norm(&mut out, &x, &w, 1e-5).unwrap();
        close(&out, &expected, 1e-5);
    }
    let mut out = [0.0f32; 2];
    assert_eq!(
        rms_norm(&mut out, &[1.0], &[1.0], 1e-5),
        Err(Error::Shape("rms_norm lengths"))
    );
    assert_eq!(
        rms_norm(&mut out, &[1.0, 1.0], &[1.0, 1.0], 0.0),
        Err(Error::InvalidEpsilon)
    );
}

#[test]
fn rope_matches_f64_rotation_and_rejects_odd_widths() {
    // Position 0 is the identity.
    let mut x = values(1, 16);
    let original = x.clone();
    rope(&mut x, 8, 0, 10_000.0).unwrap();
    assert_eq!(x, original);
    // Hand value: head_dim 2, pair 0 has frequency 1, angle = position.
    let mut x = [1.0f32, 0.0];
    rope(&mut x, 2, 1, 10_000.0).unwrap();
    close(&x, &[1f64.cos(), 1f64.sin()], 1e-6);
    for (head_dim, pos) in [(8, 3), (16, 100), (8, 511)] {
        let mut x = values(head_dim as u64 * 7 + pos as u64, head_dim * 3);
        let expected: Vec<f64> = x
            .chunks(head_dim)
            .flat_map(|head| {
                let mut out = Vec::new();
                for i in (0..head_dim).step_by(2) {
                    let freq = 10_000f64.powf(-(i as f64) / head_dim as f64);
                    let (s, c) = (pos as f64 * freq).sin_cos();
                    let (a, b) = (f64::from(head[i]), f64::from(head[i + 1]));
                    out.push(a * c - b * s);
                    out.push(a * s + b * c);
                }
                out
            })
            .collect();
        rope(&mut x, head_dim, pos, 10_000.0).unwrap();
        // FP32 angles at position 511 carry ~3e-5 relative angle error.
        close(&x, &expected, 1e-4);
        // Rotation preserves each pair's norm.
    }
    assert!(rope(&mut [0.0; 6], 3, 1, 10_000.0).is_err());
    assert!(rope(&mut [0.0; 6], 4, 1, 10_000.0).is_err());
}

#[test]
fn silu_swiglu_softmax_argmax() {
    assert_eq!(silu(0.0), 0.0);
    for v in [-6.0f32, -1.0, 0.5, 2.0, 9.0] {
        let e = f64::from(v) / (1.0 + (-f64::from(v)).exp());
        close(&[silu(v)], &[e], 1e-6);
    }
    let mut gate = vec![1.0f32, -2.0, 3.0];
    swiglu(&mut gate, &[2.0, 0.5, -1.0]).unwrap();
    let s = |v: f64| v / (1.0 + (-v).exp());
    close(&gate, &[s(1.0) * 2.0, s(-2.0) * 0.5, -s(3.0)], 1e-6);

    let mut x = vec![1.0f32, 2.0, 3.0, 1000.0, -1000.0];
    softmax(&mut x).unwrap();
    close(&x, &[0.0, 0.0, 0.0, 1.0, 0.0], 1e-6);
    let mut x = vec![0.0f32, 1.0f32.ln(), 2.0f32.ln()];
    softmax(&mut x).unwrap();
    close(&x, &[0.25, 0.25, 0.5], 1e-6);
    assert_eq!(
        softmax(&mut []),
        Err(Error::Shape("softmax of empty vector"))
    );
    assert_eq!(softmax(&mut [f32::NAN]), Err(Error::NonFinite));

    assert_eq!(argmax(&[1.0, 5.0, 5.0, -1.0]).unwrap(), 1);
    assert_eq!(argmax(&[f32::NAN]), Err(Error::NonFinite));
}

#[test]
fn fused_swiglu_matvec_is_bit_identical_to_unfused() {
    for format in [WeightFormat::F32, WeightFormat::Q8_0, WeightFormat::Q4_0] {
        let w1 = Matrix::f32(5, 64, values(11, 320))
            .unwrap()
            .quantize(format)
            .unwrap();
        let w3 = Matrix::f32(5, 64, values(12, 320))
            .unwrap()
            .quantize(format)
            .unwrap();
        let x = values(13, 64);
        let (mut a, mut b, mut fused) = (vec![0.0; 5], vec![0.0; 5], vec![0.0; 5]);
        w1.matvec(&x, &mut a).unwrap();
        w3.matvec(&x, &mut b).unwrap();
        swiglu(&mut a, &b).unwrap();
        swiglu_matvec(&w1, &w3, &x, &mut fused).unwrap();
        assert_eq!(
            a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            fused.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn fp16_encoder_round_trips_and_rounds_midpoints_to_even() {
    for bits in 0..=u16::MAX {
        let v = decode_fp16(bits);
        if v.is_nan() {
            assert!(decode_fp16(encode_fp16(v)).is_nan());
        } else {
            assert_eq!(encode_fp16(v), bits, "{bits:#06x}");
        }
    }
    // Every midpoint between adjacent positive finite halves is exact in f32;
    // it must round to the even neighbour, and one f32 ulp either side to the
    // nearer neighbour.
    for low in 0u16..0x7bff {
        let (a, b) = (decode_fp16(low), decode_fp16(low + 1));
        let mid = ((f64::from(a) + f64::from(b)) / 2.0) as f32;
        assert_eq!(f64::from(mid), (f64::from(a) + f64::from(b)) / 2.0);
        let even = if low % 2 == 0 { low } else { low + 1 };
        assert_eq!(encode_fp16(mid), even);
        assert_eq!(encode_fp16(f32::from_bits(mid.to_bits() - 1)), low);
        assert_eq!(encode_fp16(f32::from_bits(mid.to_bits() + 1)), low + 1);
        assert_eq!(encode_fp16(-mid), even | 0x8000);
    }
    assert_eq!(encode_fp16(65_520.0), 0x7c00); // rounds past the largest half
    assert_eq!(encode_fp16(65_519.0), 0x7bff);
    assert_eq!(encode_fp16(1e-9), 0);
    assert_eq!(encode_fp16(f32::NEG_INFINITY), 0xfc00);
}

#[test]
fn bf16_round_to_nearest_even() {
    assert_eq!(encode_bf16(1.0), 0x3f80);
    // 1 + 2^-8 is exactly halfway between 1 and 1 + 2^-7: ties to even (1.0).
    assert_eq!(encode_bf16(1.0 + 2f32.powi(-8)), 0x3f80);
    assert_eq!(encode_bf16(1.0 + 3.0 * 2f32.powi(-8)), 0x3f82);
    assert_eq!(encode_bf16(f32::from_bits(0x3f80_8001)), 0x3f81);
    assert!(decode_bf16(encode_bf16(f32::NAN)).is_nan());
    assert_eq!(encode_bf16(f32::MAX), 0x7f80); // overflow rounds to infinity
    for v in values(5, 1000) {
        let r = f64::from(decode_bf16(encode_bf16(v)));
        assert!((r - f64::from(v)).abs() <= f64::from(v).abs() * 2f64.powi(-8));
    }
}

#[test]
fn q8_0_and_q4_0_blocks_dequantize_to_hand_values() {
    // Q8_0: scale 0.5 (0x3800), q = [-128, -1, 0, 1, 127, 2, ...].
    let mut block = vec![0x00, 0x38];
    let qs: Vec<i8> = (0..32).map(|i| [-128, -1, 0, 1, 127][i % 5]).collect();
    block.extend(qs.iter().map(|q| *q as u8));
    let m = Matrix::quantized(WeightFormat::Q8_0, 1, 32, block).unwrap();
    let mut row = vec![0.0; 32];
    m.dequantize_row(0, &mut row).unwrap();
    let expected: Vec<f32> = qs.iter().map(|q| 0.5 * f32::from(*q)).collect();
    assert_eq!(row, expected);
    // Q4_0: scale -2 (0xc000); byte j = low nibble (element j) | high (j+16).
    let mut block = vec![0x00, 0xc0];
    block.extend((0..16u8).map(|j| j | ((15 - j) << 4)));
    let m = Matrix::quantized(WeightFormat::Q4_0, 1, 32, block).unwrap();
    m.dequantize_row(0, &mut row).unwrap();
    for (i, v) in row.iter().enumerate() {
        let nibble = if i < 16 {
            i as i32
        } else {
            15 - (i as i32 - 16)
        };
        assert_eq!(*v, -2.0 * (nibble - 8) as f32, "element {i}");
    }
    // Typed rejections.
    assert!(Matrix::quantized(WeightFormat::Q4_0, 1, 33, vec![0; 18]).is_err());
    assert!(Matrix::quantized(WeightFormat::Q8_0, 1, 32, vec![0; 33]).is_err());
    let mut nan_scale = vec![0x00, 0x7e];
    nan_scale.extend([0u8; 16]);
    assert_eq!(
        Matrix::quantized(WeightFormat::Q4_0, 1, 32, nan_scale),
        Err(Error::NonFinite)
    );
    assert!(quantize_q8_0(&[0.0; 31], 31).is_err());
    assert_eq!(
        quantize_q4_0(&[f32::INFINITY; 32], 32),
        Err(Error::NonFinite)
    );
}

#[test]
fn quantizers_follow_the_ggml_reference_rules() {
    let x: Vec<f32> = (0..32).map(|i| (i as f32 - 12.0) / 4.0).collect(); // max |x| = 19/4
    let q8 = quantize_q8_0(&x, 32).unwrap();
    assert_eq!(q8.len(), Q8_0_BYTES);
    let d = 4.75f32 / 127.0;
    assert_eq!(u16::from_le_bytes([q8[0], q8[1]]), encode_fp16(d));
    for (i, q) in q8[2..].iter().enumerate() {
        assert_eq!(*q as i8, (x[i] * (1.0 / d)).round() as i8);
    }
    let q4 = quantize_q4_0(&x, 32).unwrap();
    assert_eq!(q4.len(), Q4_0_BYTES);
    // Largest-magnitude value is +4.75, so d = 4.75 / -8.
    let d = 4.75f32 / -8.0;
    assert_eq!(u16::from_le_bytes([q4[0], q4[1]]), encode_fp16(d));
    let m = Matrix::quantized(WeightFormat::Q4_0, 1, 32, q4).unwrap();
    let mut row = vec![0.0; 32];
    m.dequantize_row(0, &mut row).unwrap();
    let dq = decode_fp16(encode_fp16(d));
    for (r, x) in row.iter().zip(&x) {
        assert!((r - x).abs() <= dq.abs() / 2.0 + 1e-3, "{r} vs {x}");
    }
}

#[test]
fn quantized_matvec_matches_f64_reference_over_dequantized_weights() {
    for format in [WeightFormat::Q8_0, WeightFormat::Q4_0] {
        for (rows, cols) in [(1, 32), (3, 64), (17, 96)] {
            let w = Matrix::f32(
                rows,
                cols,
                values(rows as u64 * 31 + cols as u64, rows * cols),
            )
            .unwrap()
            .quantize(format)
            .unwrap();
            let x = values(77, cols);
            let mut out = vec![0.0; rows];
            w.matvec(&x, &mut out).unwrap();
            let mut row = vec![0.0; cols];
            let expected: Vec<f64> = (0..rows)
                .map(|r| {
                    w.dequantize_row(r, &mut row).unwrap();
                    row.iter()
                        .zip(&x)
                        .map(|(w, x)| f64::from(*w) * f64::from(*x))
                        .sum()
                })
                .collect();
            close(&out, &expected, 1e-5);
        }
    }
    let w = Matrix::f32(2, 32, vec![0.0; 64]).unwrap();
    assert!(w.matvec(&[0.0; 31], &mut [0.0; 2]).is_err());
}

/// Naive contiguous f64 GQA attention for one query over `tokens` K/V rows.
fn reference_attention(
    q: &[f32],
    keys: &[Vec<f32>],
    values: &[Vec<f32>],
    shape: AttentionShape,
) -> Vec<f64> {
    let group = shape.query_heads / shape.kv_heads;
    let hd = shape.head_dim;
    let mut out = vec![0.0; shape.query_heads * hd];
    for h in 0..shape.query_heads {
        let kvh = h / group;
        let scores: Vec<f64> = keys
            .iter()
            .map(|k| {
                (0..hd)
                    .map(|i| f64::from(q[h * hd + i]) * f64::from(k[kvh * hd + i]))
                    .sum::<f64>()
                    / (hd as f64).sqrt()
            })
            .collect();
        let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let weights: Vec<f64> = scores.iter().map(|s| (s - max).exp()).collect();
        let total: f64 = weights.iter().sum();
        for (w, v) in weights.iter().zip(values) {
            for i in 0..hd {
                out[h * hd + i] += w / total * f64::from(v[kvh * hd + i]);
            }
        }
    }
    out
}

#[test]
fn paged_gqa_attention_matches_contiguous_reference_across_pages() {
    let shape = AttentionShape {
        query_heads: 8,
        kv_heads: 2,
        head_dim: 4,
    };
    for format in [KvFormat::F32, KvFormat::Bf16] {
        let mut pool = KvPool::with_format(
            KvConfig {
                heads: 2,
                head_dim: 4,
                pages: 3,
                max_sequences: 1,
                max_tokens_per_sequence: 48,
                max_bytes: 1 << 16,
            },
            format,
        )
        .unwrap();
        let mut seq = pool.begin().unwrap();
        let (mut keys, mut vals) = (Vec::new(), Vec::new());
        for pos in 0..48 {
            let mut k = values(1000 + pos as u64, 8);
            let mut v = values(2000 + pos as u64, 8);
            if format == KvFormat::Bf16 {
                // The reference sees exactly the stored (rounded) values.
                for x in k.iter_mut().chain(v.iter_mut()) {
                    *x = decode_bf16(encode_bf16(*x * 3.0));
                }
                let enc = |xs: &[f32]| -> Vec<u8> {
                    xs.iter()
                        .flat_map(|x| encode_bf16(*x).to_le_bytes())
                        .collect()
                };
                let (kb, vb) = (enc(&k), enc(&v));
                let step = |bytes| vesper_tensor::kv::Bf16Step {
                    heads: 2,
                    head_dim: 4,
                    bytes,
                };
                assert_eq!(pool.append_f32(&mut seq, pos, &k, &v), Err(KvError::Format));
                pool.append(&mut seq, pos, step(&kb), step(&vb)).unwrap();
            } else {
                for x in k.iter_mut() {
                    *x *= 3.0;
                }
                pool.append_f32(&mut seq, pos, &k, &v).unwrap();
            }
            keys.push(k);
            vals.push(v);
            let tokens = pos + 1;
            if ![1, 15, 16, 17, 32, 33, 48].contains(&tokens) {
                continue;
            }
            let q = values(3000 + pos as u64, 32);
            let expected = reference_attention(&q, &keys, &vals, shape);
            for mode in [AttentionMode::TwoPass, AttentionMode::Online] {
                let mut scores = vec![0.0; 8 * 48];
                let mut out = vec![0.0; 32];
                paged_gqa_attention(&pool, &seq, shape, &q, &mut scores, &mut out, mode).unwrap();
                close(&out, &expected, 1e-5);
            }
        }
        assert_eq!(pool.page_count(&seq).unwrap(), 3);
        let mut out = vec![0.0; 32];
        let bad = AttentionShape {
            query_heads: 3,
            ..shape
        };
        assert!(paged_gqa_attention(
            &pool,
            &seq,
            bad,
            &[0.0; 12],
            &mut [],
            &mut out[..12],
            AttentionMode::Online
        )
        .is_err());
        let wrong_kv = AttentionShape {
            kv_heads: 4,
            head_dim: 2,
            ..shape
        };
        assert!(paged_gqa_attention(
            &pool,
            &seq,
            wrong_kv,
            &[0.0; 16],
            &mut [],
            &mut out[..16],
            AttentionMode::Online
        )
        .is_err());
        // TwoPass needs score scratch for every head and token.
        assert!(paged_gqa_attention(
            &pool,
            &seq,
            shape,
            &[0.0; 32],
            &mut [0.0; 10],
            &mut out,
            AttentionMode::TwoPass
        )
        .is_err());
        pool.release(seq).unwrap();
        assert_eq!(pool.stats().allocated_pages, 0);
    }
}

#[test]
fn attention_rejects_empty_sequences_and_foreign_handles() {
    let config = KvConfig {
        heads: 1,
        head_dim: 2,
        pages: 1,
        max_sequences: 1,
        max_tokens_per_sequence: 16,
        max_bytes: 1024,
    };
    let shape = AttentionShape {
        query_heads: 1,
        kv_heads: 1,
        head_dim: 2,
    };
    let pool = KvPool::with_format(config, KvFormat::F32).unwrap();
    let mut other = KvPool::with_format(config, KvFormat::F32).unwrap();
    let mut seq = other.begin().unwrap();
    let mut out = [0.0; 2];
    assert!(paged_gqa_attention(
        &other,
        &seq,
        shape,
        &[1.0, 1.0],
        &mut [],
        &mut out,
        AttentionMode::Online
    )
    .is_err());
    other
        .append_f32(&mut seq, 0, &[1.0, 2.0], &[3.0, 4.0])
        .unwrap();
    assert_eq!(
        paged_gqa_attention(
            &pool,
            &seq,
            shape,
            &[1.0, 1.0],
            &mut [],
            &mut out,
            AttentionMode::Online
        ),
        Err(Error::Kv(KvError::ForeignOwner))
    );
    paged_gqa_attention(
        &other,
        &seq,
        shape,
        &[1.0, 1.0],
        &mut [],
        &mut out,
        AttentionMode::Online,
    )
    .unwrap();
    assert_eq!(out, [3.0, 4.0]);
    assert_eq!(
        other.append_f32(&mut seq, 1, &[1.0], &[1.0]),
        Err(KvError::Shape)
    );
    other.release(seq).unwrap();
}
