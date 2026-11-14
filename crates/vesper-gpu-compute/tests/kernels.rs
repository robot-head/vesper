//! Each tensor kernel on the device against vesper-tensor's scalar CPU
//! kernel, within a stated tolerance.
//!
//! Tolerances: matmul, SwiGLU and the K/V write follow the CPU order and are
//! required to agree to 1e-6 relative (the device may fuse multiply-adds);
//! RMSNorm reduces in a tree (1e-5); RoPE and attention use the device's
//! `pow`, `sin`, `cos` and `exp` (2e-5 and 1e-5).
use vesper_gpu_compute::tensor::{MAX_ATTENTION_TOKENS, TensorKernels, rope_frequencies};
use vesper_gpu_compute::{Buffer, Context, GpuError, test_context};
use vesper_tensor::attention::{AttentionMode, AttentionShape, paged_gqa_attention};
use vesper_tensor::kv::{KvConfig, KvFormat, KvPool, TOKENS_PER_PAGE};
use vesper_tensor::ops::{rms_norm, rope, swiglu};
use vesper_tensor::quant::Matrix;

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

fn close(gpu: &[f32], cpu: &[f32], tol: f32, what: &str) -> f32 {
    assert_eq!(gpu.len(), cpu.len());
    let mut worst = 0.0f32;
    for (i, (g, c)) in gpu.iter().zip(cpu).enumerate() {
        let err = (g - c).abs() / (1.0 + c.abs());
        worst = worst.max(err);
        assert!(err <= tol, "{what}[{i}]: GPU {g} vs CPU {c}");
    }
    worst
}

fn read(buffer: &Buffer, n: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; n];
    buffer.read(0, &mut out).unwrap();
    out
}

fn zeros(ctx: &Context, n: usize) -> Buffer {
    ctx.buffer((n * 4) as u64).unwrap()
}

#[test]
fn matmul_matches_cpu_matvec_rows() {
    let Some(ctx) = test_context() else { return };
    let k = TensorKernels::new(&ctx).unwrap();
    for (m, n, kk) in [
        (1, 1, 1),
        (1, 64, 64),
        (3, 65, 17),
        (7, 512, 64),
        (2, 172, 64),
        (1, 64, 172),
        (16, 130, 300),
    ] {
        let a = values(m as u64 * 31 + n as u64, m * kk);
        let w = values(n as u64 * 17 + kk as u64, n * kk);
        let init = values(99, m * n);
        let (ab, wb) = (ctx.buffer_from(&a).unwrap(), ctx.buffer_from(&w).unwrap());
        let out = ctx.buffer_from(&init).unwrap();
        let acc = ctx.buffer_from(&init).unwrap();
        let mut rec = ctx.record().unwrap();
        k.matmul(&mut rec, &ab, &wb, &out, m, n, kk, false).unwrap();
        k.matmul(&mut rec, &ab, &wb, &acc, m, n, kk, true).unwrap();
        rec.submit().unwrap().wait().unwrap();
        let matrix = Matrix::f32(n, kk, w.clone()).unwrap();
        let mut expected = vec![0.0f32; m * n];
        for r in 0..m {
            matrix
                .matvec(&a[r * kk..(r + 1) * kk], &mut expected[r * n..(r + 1) * n])
                .unwrap();
        }
        close(&read(&out, m * n), &expected, 1e-6, "matmul");
        let accumulated: Vec<f32> = expected.iter().zip(&init).map(|(e, i)| i + e).collect();
        close(&read(&acc, m * n), &accumulated, 1e-6, "matmul accumulate");
    }
}

#[test]
fn rms_norm_matches_cpu_per_row() {
    let Some(ctx) = test_context() else { return };
    let k = TensorKernels::new(&ctx).unwrap();
    for (rows, n) in [
        (1, 1),
        (1, 7),
        (1, 64),
        (3, 172),
        (2, 255),
        (2, 256),
        (4, 1000),
    ] {
        let x = values(n as u64, rows * n);
        let w = values(n as u64 + 5, n);
        let (xb, wb, out) = (
            ctx.buffer_from(&x).unwrap(),
            ctx.buffer_from(&w).unwrap(),
            zeros(&ctx, rows * n),
        );
        let mut rec = ctx.record().unwrap();
        k.rms_norm(&mut rec, &xb, &wb, &out, rows, n, 1e-5).unwrap();
        rec.submit().unwrap().wait().unwrap();
        let mut expected = vec![0.0f32; rows * n];
        for r in 0..rows {
            rms_norm(
                &mut expected[r * n..(r + 1) * n],
                &x[r * n..(r + 1) * n],
                &w,
                1e-5,
            )
            .unwrap();
        }
        close(&read(&out, rows * n), &expected, 1e-5, "rms_norm");
    }
}

#[test]
fn rope_matches_cpu_up_to_long_positions() {
    let Some(ctx) = test_context() else { return };
    let k = TensorKernels::new(&ctx).unwrap();
    let mut worst = 0.0f32;
    for head_dim in [2usize, 8, 64, 128] {
        for position in [0usize, 1, 15, 16, 200, 511, 2047] {
            let len = head_dim * 3;
            let x = values((head_dim * 7 + position) as u64, len);
            let xb = ctx.buffer_from(&x).unwrap();
            let fb = ctx
                .buffer_from(&rope_frequencies(head_dim, 10_000.0).unwrap())
                .unwrap();
            let mut rec = ctx.record().unwrap();
            k.rope(&mut rec, &xb, &fb, len, head_dim, position).unwrap();
            rec.submit().unwrap().wait().unwrap();
            let mut expected = x.clone();
            rope(&mut expected, head_dim, position, 10_000.0).unwrap();
            let what = format!("rope head_dim {head_dim} position {position}");
            worst = worst.max(close(&read(&xb, len), &expected, 2e-5, &what));
        }
    }
    eprintln!("worst RoPE relative error: {worst:e}");
}

#[test]
fn swiglu_matches_cpu() {
    let Some(ctx) = test_context() else { return };
    let k = TensorKernels::new(&ctx).unwrap();
    let n = 172 * 3 + 1;
    let gate: Vec<f32> = values(3, n).iter().map(|v| v * 8.0).collect();
    let up = values(4, n);
    let (gb, ub) = (
        ctx.buffer_from(&gate).unwrap(),
        ctx.buffer_from(&up).unwrap(),
    );
    let mut rec = ctx.record().unwrap();
    k.swiglu(&mut rec, &gb, &ub, n).unwrap();
    rec.submit().unwrap().wait().unwrap();
    let mut expected = gate.clone();
    swiglu(&mut expected, &up).unwrap();
    close(&read(&gb, n), &expected, 1e-6, "swiglu");
}

/// Fills a CPU KvPool and the device page pool with the same tokens (the
/// device through `kv_write` and a scrambled page table), then compares
/// paged attention for both CPU schedules.
#[test]
fn paged_attention_matches_cpu_two_pass_and_online() {
    let Some(ctx) = test_context() else { return };
    let k = TensorKernels::new(&ctx).unwrap();
    let mut worst = 0.0f32;
    for (query_heads, kv_heads, head_dim) in
        [(1, 1, 8), (8, 4, 8), (4, 1, 64), (6, 6, 16), (2, 2, 130)]
    {
        let kv_dim = kv_heads * head_dim;
        let max_tokens: usize = 300;
        let pages = max_tokens.div_ceil(TOKENS_PER_PAGE);
        let mut pool = KvPool::with_format(
            KvConfig {
                heads: kv_heads,
                head_dim,
                pages,
                max_sequences: 1,
                max_tokens_per_sequence: max_tokens,
                max_bytes: 64 << 20,
            },
            KvFormat::F32,
        )
        .unwrap();
        let mut seq = pool.begin().unwrap();
        // Physical page for logical page p: a permutation, not the identity.
        let table: Vec<u32> = (0..pages as u32)
            .map(|p| (p * 7 + 3) % pages as u32)
            .collect();
        let mut sorted = table.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), pages, "table is a permutation");
        let tb = ctx.buffer_from(&table).unwrap();
        let keys = zeros(&ctx, pages * TOKENS_PER_PAGE * kv_dim);
        let vals = zeros(&ctx, pages * TOKENS_PER_PAGE * kv_dim);
        let kb = zeros(&ctx, kv_dim);
        let vb = zeros(&ctx, kv_dim);
        let shape = AttentionShape {
            query_heads,
            kv_heads,
            head_dim,
        };
        let mut kb = kb;
        let mut vb = vb;
        for t in 0..max_tokens {
            let key = values(t as u64 * 3 + 1, kv_dim);
            let value = values(t as u64 * 3 + 2, kv_dim);
            pool.append_f32(&mut seq, t, &key, &value).unwrap();
            kb.write(0, &key).unwrap();
            vb.write(0, &value).unwrap();
            let mut rec = ctx.record().unwrap();
            k.kv_write(&mut rec, &kb, &vb, &tb, &keys, &vals, t, kv_dim)
                .unwrap();
            rec.submit().unwrap().wait().unwrap();
            let tokens = t + 1;
            if ![1, 15, 16, 17, 32, 33, 100, 300].contains(&tokens) {
                continue;
            }
            // Large-magnitude queries make the softmax peaked.
            let q: Vec<f32> = values(tokens as u64 + 77, query_heads * head_dim)
                .iter()
                .map(|v| v * 4.0)
                .collect();
            let (qb, out) = (ctx.buffer_from(&q).unwrap(), zeros(&ctx, q.len()));
            let mut rec = ctx.record().unwrap();
            k.attention(&mut rec, &qb, &keys, &vals, &tb, &out, shape, tokens)
                .unwrap();
            rec.submit().unwrap().wait().unwrap();
            let gpu = read(&out, q.len());
            for mode in [AttentionMode::TwoPass, AttentionMode::Online] {
                let mut scores = vec![0.0f32; query_heads * tokens];
                let mut expected = vec![0.0f32; q.len()];
                paged_gqa_attention(&pool, &seq, shape, &q, &mut scores, &mut expected, mode)
                    .unwrap();
                worst = worst.max(close(&gpu, &expected, 1e-5, "attention"));
            }
        }
        pool.release(seq).unwrap();
    }
    eprintln!("worst attention relative error: {worst:e}");
}

#[test]
fn kernel_arguments_are_validated() {
    let Some(ctx) = test_context() else { return };
    let k = TensorKernels::new(&ctx).unwrap();
    let small = zeros(&ctx, 4);
    let big = zeros(&ctx, 64);
    let mut rec = ctx.record().unwrap();
    assert_eq!(
        k.matmul(&mut rec, &small, &big, &big, 1, 4, 8, false),
        Err(GpuError::Shape("matmul a"))
    );
    assert_eq!(
        k.matmul(&mut rec, &big, &big, &big, 0, 4, 8, false),
        Err(GpuError::Shape("matmul a"))
    );
    assert_eq!(
        k.rms_norm(&mut rec, &big, &big, &big, 1, 8, 0.0),
        Err(GpuError::Shape("rms_norm epsilon"))
    );
    assert!(k.rope(&mut rec, &big, &big, 6, 4, 0).is_err());
    assert!(k.rope(&mut rec, &big, &big, 8, 3, 0).is_err());
    assert!(k.rope(&mut rec, &big, &small, 64, 64, 0).is_err());
    assert!(rope_frequencies(4, 1.0).is_err());
    assert!(rope_frequencies(3, 10_000.0).is_err());
    let shape = AttentionShape {
        query_heads: 3,
        kv_heads: 2,
        head_dim: 4,
    };
    assert!(matches!(
        k.attention(&mut rec, &big, &big, &big, &big, &big, shape, 1),
        Err(GpuError::Tensor(_))
    ));
    let shape = AttentionShape {
        query_heads: 2,
        kv_heads: 1,
        head_dim: 1,
    };
    assert_eq!(
        k.attention(&mut rec, &big, &big, &big, &big, &big, shape, 0),
        Err(GpuError::Shape("attention over an empty sequence"))
    );
    assert_eq!(
        k.attention(
            &mut rec,
            &big,
            &big,
            &big,
            &big,
            &big,
            shape,
            MAX_ATTENTION_TOKENS + 1
        ),
        Err(GpuError::Limit("attention tokens"))
    );
    assert_eq!(
        k.attention(&mut rec, &big, &big, &big, &small, &big, shape, 100),
        Err(GpuError::Shape("attention page table too short"))
    );
    assert_eq!(rec.dispatches(), 0);
    rec.submit().unwrap().wait().unwrap();
}
