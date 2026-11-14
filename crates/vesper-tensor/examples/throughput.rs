//! Measures decode, prefill and multi-sequence throughput:
//! `cargo run --release -p vesper-tensor --example throughput`.
//!
//! Models: the bundled stories260K checkpoint and a synthetic random-weight
//! Llama-architecture model (dim 512, hidden 1536, 8 layers, 8 query / 4 KV
//! heads, vocabulary 4096, context 256) in several weight formats. Each line
//! is the best of three runs of wall-clock time on this host; numbers are
//! host-specific measurements, not a benchmark suite.
use std::time::{Duration, Instant};

use vesper_tensor::attention::AttentionMode;
use vesper_tensor::kv::KvFormat;
use vesper_tensor::llama::{BatchItem, KvCache, Model, RunOptions, BOS};
use vesper_tensor::ops::argmax;
use vesper_tensor::quant::WeightFormat;
use vesper_tensor::simd::{simd_isa, Kernel};

fn synthetic() -> Model {
    let shape = [512usize, 1536, 8, 8, 4, 4096, 256];
    let [dim, hidden, layers, heads, kv_heads, vocab, seq_len] = shape;
    let kv_dim = kv_heads * dim / heads;
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut out = Vec::new();
    for v in shape {
        out.extend_from_slice(&(v as i32).to_le_bytes());
    }
    let mut put = |n: usize, scale: f32, offset: f32| {
        for _ in 0..n {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let u = ((state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0;
            out.extend_from_slice(&(offset + u * scale).to_le_bytes());
        }
    };
    let s = |fan_in: usize| 1.7 / (fan_in as f32).sqrt();
    put(vocab * dim, 1.0, 0.0);
    put(layers * dim, 0.1, 1.0);
    put(layers * dim * dim, s(dim), 0.0);
    put(layers * kv_dim * dim, s(dim), 0.0);
    put(layers * kv_dim * dim, s(dim), 0.0);
    put(layers * dim * dim, s(dim), 0.0);
    put(layers * dim, 0.1, 1.0);
    put(layers * hidden * dim, s(dim), 0.0);
    put(layers * dim * hidden, s(hidden), 0.0);
    put(layers * hidden * dim, s(dim), 0.0);
    put(dim, 0.1, 1.0);
    put(seq_len * (dim / heads), 0.0, 0.0);
    Model::from_llama2c(&out).expect("synthetic checkpoint")
}

fn best<F: FnMut() -> usize>(mut f: F) -> (usize, Duration) {
    let mut best = Duration::MAX;
    let mut tokens = 0;
    for _ in 0..3 {
        let start = Instant::now();
        tokens = f();
        best = best.min(start.elapsed());
    }
    (tokens, best)
}

fn rate(tokens: usize, d: Duration) -> f64 {
    tokens as f64 / d.as_secs_f64()
}

/// Greedy decode of `steps` tokens from BOS, one token per call.
fn decode(model: &Model, steps: usize, opts: RunOptions) -> usize {
    let mut cache = KvCache::new(model.config(), KvFormat::F32, 1).unwrap();
    let mut s = cache.begin().unwrap();
    let mut token = BOS;
    for _ in 0..steps {
        token =
            argmax(model.forward_with(&mut cache, &mut s, token, opts).unwrap()).unwrap() as u32;
    }
    cache.release(s).map_err(|(e, _)| e).unwrap();
    steps
}

/// `prompt` tokens prefilled one per call (`batched = false`) or in one call.
fn prefill(model: &Model, prompt: usize, opts: RunOptions, batched: bool) -> usize {
    let mut cache = KvCache::new(model.config(), KvFormat::F32, 1).unwrap();
    let mut s = cache.begin().unwrap();
    let tokens: Vec<u32> = (0..prompt as u32).map(|i| 1 + i % 200).collect();
    if batched {
        let mut scratch = model.batch_scratch(prompt, 1).unwrap();
        model
            .forward_batch(
                &mut cache,
                &mut [BatchItem {
                    session: &mut s,
                    tokens: &tokens,
                }],
                &mut scratch,
                opts,
            )
            .unwrap();
    } else {
        for &t in &tokens {
            model.forward_with(&mut cache, &mut s, t, opts).unwrap();
        }
    }
    cache.release(s).map_err(|(e, _)| e).unwrap();
    prompt
}

/// `n` sequences decoding `steps` tokens each, all in one call per step.
fn multi(model: &Model, n: usize, steps: usize, opts: RunOptions) -> usize {
    let mut cache = KvCache::new(model.config(), KvFormat::F32, n).unwrap();
    let mut sessions: Vec<_> = (0..n).map(|_| cache.begin().unwrap()).collect();
    let mut scratch = model.batch_scratch(n, n).unwrap();
    let mut next: Vec<[u32; 1]> = (0..n as u32).map(|i| [1 + i]).collect();
    for _ in 0..steps {
        let mut items: Vec<BatchItem<'_>> = sessions
            .iter_mut()
            .zip(&next)
            .map(|(session, t)| BatchItem {
                session,
                tokens: t.as_slice(),
            })
            .collect();
        model
            .forward_batch(&mut cache, &mut items, &mut scratch, opts)
            .unwrap();
        for (t, s) in next.iter_mut().zip(&sessions) {
            t[0] = argmax(s.logits()).unwrap() as u32;
        }
    }
    for s in sessions {
        cache.release(s).map_err(|(e, _)| e).unwrap();
    }
    n * steps
}

fn main() {
    let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
    println!(
        "host: {} {}, {threads} hardware threads, SIMD ISA {:?}, single-threaded runs",
        std::env::consts::OS,
        std::env::consts::ARCH,
        simd_isa()
    );
    let stories = Model::from_llama2c(include_bytes!("../models/stories260K.bin")).unwrap();
    let synthetic = synthetic();
    let opts = |kernel| RunOptions {
        attention: AttentionMode::Online,
        kernel,
    };
    let mut models = vec![("stories260K F32".to_string(), stories, 200usize)];
    for format in [
        WeightFormat::F32,
        WeightFormat::Q8_0,
        WeightFormat::Q4K,
        WeightFormat::Mxfp4,
        WeightFormat::F8E4M3,
    ] {
        let (m, _) = synthetic.quantized(format).unwrap();
        models.push((format!("synthetic-512 {format:?}"), m, 64));
    }
    for (name, model, steps) in &models {
        for kernel in [Kernel::Scalar, Kernel::Simd] {
            let (n, d) = best(|| decode(model, *steps, opts(kernel)));
            let (p1, d1) = best(|| prefill(model, 128, opts(kernel), false));
            let (p2, d2) = best(|| prefill(model, 128, opts(kernel), true));
            let (m8, dm) = best(|| multi(model, 8, steps / 2, opts(kernel)));
            println!(
                "{name:<24} {kernel:?}: decode {:>9.1} tok/s | prefill 128: one-by-one {:>9.1} tok/s, batched {:>9.1} tok/s | 8 sequences batched {:>9.1} tok/s",
                rate(n, d),
                rate(p1, d1),
                rate(p2, d2),
                rate(m8, dm)
            );
        }
    }
}
