//! `std::arch` kernels against their scalar references: the SIMD dot product
//! equals the portable eight-lane emulation bit for bit, element-wise kernels
//! equal the scalar loops bit for bit, and every matrix format's SIMD
//! matvec/matmul equals the emulation over its dequantized rows.
mod common;

use vesper_tensor::attention::{paged_gqa_attention_with, AttentionMode, AttentionShape};
use vesper_tensor::kv::{KvConfig, KvFormat, KvPool};
use vesper_tensor::quant::{Matrix, WeightFormat};
use vesper_tensor::simd::{self, ByteElements, DotAcc, Kernel};

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

#[test]
fn simd_dot_equals_the_lane_emulation_and_scalar_is_sequential() {
    eprintln!("SIMD ISA on this host: {:?}", simd::simd_isa());
    let mut rng = common::Rng(11);
    for len in 0..=300 {
        let a = rng.vec(len, 3.0);
        let b = rng.vec(len, 3.0);
        let simd_dot = simd::dot(Kernel::Simd, &a, &b);
        assert_eq!(
            simd_dot.to_bits(),
            simd::lanes_reference_dot(&a, &b).to_bits(),
            "len {len}"
        );
        let mut seq = 0.0f32;
        for (x, y) in a.iter().zip(&b) {
            seq += x * y;
        }
        assert_eq!(simd::dot(Kernel::Scalar, &a, &b).to_bits(), seq.to_bits());
        let exact: f64 = a
            .iter()
            .zip(&b)
            .map(|(x, y)| f64::from(*x) * f64::from(*y))
            .sum();
        let mag: f64 = a
            .iter()
            .zip(&b)
            .map(|(x, y)| f64::from((x * y).abs()))
            .sum();
        assert!((f64::from(simd_dot) - exact).abs() <= 1e-5 * mag + 1e-30);
        // Feeding the same accumulator in pieces of multiples of eight is
        // identical to one call.
        if len >= 16 {
            let mut acc = DotAcc::ZERO;
            simd::dot_acc(Kernel::Simd, &mut acc, &a[..8], &b[..8]);
            simd::dot_acc(Kernel::Simd, &mut acc, &a[8..16], &b[8..16]);
            simd::dot_acc(Kernel::Simd, &mut acc, &a[16..], &b[16..]);
            assert_eq!(acc.finish(Kernel::Simd).to_bits(), simd_dot.to_bits());
        }
        // Byte operands: little-endian FP32 and BF16.
        let f32_bytes: Vec<u8> = b.iter().flat_map(|v| v.to_le_bytes()).collect();
        let bf16: Vec<u16> = b.iter().map(|v| (v.to_bits() >> 16) as u16).collect();
        let bf16_bytes: Vec<u8> = bf16.iter().flat_map(|v| v.to_le_bytes()).collect();
        let bf16_vals: Vec<f32> = bf16
            .iter()
            .map(|v| f32::from_bits(u32::from(*v) << 16))
            .collect();
        for kernel in [Kernel::Scalar, Kernel::Simd] {
            let mut acc = DotAcc::ZERO;
            simd::dot_acc_bytes(kernel, &mut acc, &a, &f32_bytes, ByteElements::F32Le);
            assert_eq!(
                acc.finish(kernel).to_bits(),
                simd::dot(kernel, &a, &b).to_bits()
            );
            let mut acc = DotAcc::ZERO;
            simd::dot_acc_bytes(kernel, &mut acc, &a, &bf16_bytes, ByteElements::Bf16Le);
            assert_eq!(
                acc.finish(kernel).to_bits(),
                simd::dot(kernel, &a, &bf16_vals).to_bits()
            );
        }
        // AXPY and scale: one rounding per element, so identical.
        for (bytes, vals, e) in [
            (&f32_bytes, &b, ByteElements::F32Le),
            (&bf16_bytes, &bf16_vals, ByteElements::Bf16Le),
        ] {
            let mut x = a.clone();
            let mut y = a.clone();
            let mut z = a.clone();
            simd::axpy(Kernel::Simd, &mut x, 0.37, vals);
            simd::axpy_bytes(Kernel::Simd, &mut y, 0.37, bytes, e);
            for (z, v) in z.iter_mut().zip(vals.iter()) {
                *z += 0.37 * v;
            }
            assert_eq!(bits(&x), bits(&z));
            assert_eq!(bits(&y), bits(&z));
        }
    }
}

#[test]
fn every_format_simd_matvec_and_matmul_follow_their_references() {
    let mut rng = common::Rng(5);
    let rows = 9;
    for (format, cols) in [
        (WeightFormat::F32, 300),
        (WeightFormat::F16, 77),
        (WeightFormat::Bf16, 64),
        (WeightFormat::Q8_0, 96),
        (WeightFormat::Q4_0, 64),
        (WeightFormat::Q4K, 512),
        (WeightFormat::Mxfp4, 128),
        (WeightFormat::F8E4M3, 270),
        (WeightFormat::F8E5M2, 33),
    ] {
        let m = Matrix::f32(rows, cols, rng.vec(rows * cols, 1.0))
            .unwrap()
            .quantize(format)
            .unwrap();
        let n = 5;
        let xs = rng.vec(n * cols, 2.0);
        let dense = m.to_f32().unwrap();
        for kernel in [Kernel::Scalar, Kernel::Simd] {
            let mut accs = vec![DotAcc::ZERO; n];
            let mut batched = vec![0.0; n * rows];
            m.matmul_with(kernel, &xs, &mut batched, &mut accs).unwrap();
            for t in 0..n {
                let x = &xs[t * cols..(t + 1) * cols];
                let mut out = vec![0.0; rows];
                m.matvec_with(kernel, x, &mut out).unwrap();
                assert_eq!(
                    bits(&out),
                    bits(&batched[t * rows..(t + 1) * rows]),
                    "{format:?}"
                );
                for r in 0..rows {
                    let w = &dense[r * cols..(r + 1) * cols];
                    let expected = match kernel {
                        Kernel::Simd => simd::lanes_reference_dot(w, x),
                        Kernel::Scalar => simd::dot(Kernel::Scalar, w, x),
                    };
                    assert_eq!(
                        out[r].to_bits(),
                        expected.to_bits(),
                        "{format:?} {kernel:?}"
                    );
                }
            }
        }
        let mut accs = vec![DotAcc::ZERO; 2];
        assert!(m
            .matmul_with(Kernel::Simd, &xs, &mut [0.0; 3], &mut accs)
            .is_err());
        assert!(m
            .matmul_with(Kernel::Simd, &xs[..0], &mut [], &mut [])
            .is_err());
    }
}

#[test]
fn simd_paged_attention_is_close_to_scalar_and_causal_limits_are_exact() {
    let mut rng = common::Rng(3);
    let shape = AttentionShape {
        query_heads: 8,
        kv_heads: 2,
        head_dim: 24,
    };
    for format in [KvFormat::F32, KvFormat::Bf16] {
        let config = KvConfig {
            heads: 2,
            head_dim: 24,
            pages: 8,
            max_sequences: 2,
            max_tokens_per_sequence: 64,
            max_bytes: 1 << 20,
        };
        let mut pool = KvPool::with_format(config, format).unwrap();
        let mut short = pool.begin().unwrap();
        let mut long = pool.begin().unwrap();
        let steps: Vec<(Vec<f32>, Vec<f32>)> = (0..40)
            .map(|_| (rng.vec(48, 1.0), rng.vec(48, 1.0)))
            .collect();
        for (i, (k, v)) in steps.iter().enumerate() {
            let encode = |x: &[f32]| -> Vec<u8> {
                x.iter()
                    .flat_map(|v| vesper_tensor::quant::encode_bf16(*v).to_le_bytes())
                    .collect()
            };
            for (seq, limit) in [(&mut short, 21), (&mut long, 40)] {
                if i < limit {
                    match format {
                        KvFormat::F32 => pool.append_f32(seq, i, k, v).unwrap(),
                        KvFormat::Bf16 => {
                            let (k, v) = (encode(k), encode(v));
                            let step = |bytes| vesper_tensor::kv::Bf16Step {
                                heads: 2,
                                head_dim: 24,
                                bytes,
                            };
                            pool.append(seq, i, step(&k), step(&v)).unwrap();
                        }
                    }
                }
            }
        }
        let q = rng.vec(8 * 24, 1.0);
        let mut scores = vec![0.0; 8 * 64];
        for mode in [AttentionMode::TwoPass, AttentionMode::Online] {
            let run = |seq, kernel, visible| {
                let mut out = vec![0.0; 8 * 24];
                let mut scores = scores.clone();
                paged_gqa_attention_with(
                    &pool,
                    seq,
                    shape,
                    &q,
                    &mut scores,
                    &mut out,
                    mode,
                    kernel,
                    visible,
                )
                .unwrap();
                out
            };
            for kernel in [Kernel::Scalar, Kernel::Simd] {
                // Attending the first 21 of 40 tokens equals a 21-token sequence.
                assert_eq!(
                    bits(&run(&long, kernel, Some(21))),
                    bits(&run(&short, kernel, None))
                );
                assert_eq!(
                    bits(&run(&long, kernel, Some(40))),
                    bits(&run(&long, kernel, None))
                );
            }
            let scalar = run(&long, Kernel::Scalar, None);
            let simd = run(&long, Kernel::Simd, None);
            for (a, b) in scalar.iter().zip(&simd) {
                assert!((a - b).abs() <= 1e-5 * (1.0 + a.abs()), "{a} {b}");
            }
            let mut out = vec![0.0; 8 * 24];
            assert!(paged_gqa_attention_with(
                &pool,
                &long,
                shape,
                &q,
                &mut scores,
                &mut out,
                mode,
                Kernel::Simd,
                Some(41)
            )
            .is_err());
            assert!(paged_gqa_attention_with(
                &pool,
                &long,
                shape,
                &q,
                &mut scores,
                &mut out,
                mode,
                Kernel::Simd,
                Some(0)
            )
            .is_err());
        }
        pool.release(short).unwrap();
        pool.release(long).unwrap();
    }
}
