//! End-to-end greedy generation with the public stories260K checkpoint
//! (see models/README.md for provenance and licence).
use vesper_tensor::attention::AttentionMode;
use vesper_tensor::kv::KvFormat;
use vesper_tensor::llama::{generate_greedy, leaked_sessions, KvCache, Model, Tokenizer, BOS};
use vesper_tensor::quant::WeightFormat;
use vesper_tensor::Error;

const MODEL: &[u8] = include_bytes!("../models/stories260K.bin");
const TOKENIZER: &[u8] = include_bytes!("../models/tok512.bin");
/// llama2.c `test_all.py` `expected_stdout`: `run.c -t 0.0 -n 200` output.
const EXPECTED: &[u8] = include_bytes!("../models/stories260K-greedy-200.txt");

fn load() -> (Model, Tokenizer) {
    let model = Model::from_llama2c(MODEL).unwrap();
    let tokenizer = Tokenizer::from_llama2c(TOKENIZER, model.config().vocab).unwrap();
    (model, tokenizer)
}

fn generate(model: &Model, format: KvFormat, mode: AttentionMode) -> Vec<u32> {
    let mut cache = KvCache::new(model.config(), format, 1).unwrap();
    let mut session = cache.begin().unwrap();
    let tokens = generate_greedy(model, &mut cache, &mut session, &[BOS], 200, mode).unwrap();
    cache.release(session).map_err(|(e, _)| e).unwrap();
    for pool in cache.pools() {
        assert_eq!(pool.stats().allocated_pages, 0);
    }
    tokens
}

#[test]
fn config_matches_published_header() {
    let (model, tokenizer) = load();
    let c = model.config();
    assert_eq!(
        (
            c.dim,
            c.hidden_dim,
            c.layers,
            c.heads,
            c.kv_heads,
            c.vocab,
            c.seq_len
        ),
        (64, 172, 5, 8, 4, 512, 512)
    );
    assert!(c.shared_classifier);
    assert_eq!(tokenizer.vocab(), 512);
}

#[test]
fn fp32_two_pass_matches_llama2c_reference_text() {
    let (model, tokenizer) = load();
    let tokens = generate(&model, KvFormat::F32, AttentionMode::TwoPass);
    let text = tokenizer.decode_all(BOS, &tokens).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&text),
        String::from_utf8_lossy(EXPECTED)
    );
    assert_eq!(tokens.len(), 200);
}

#[test]
fn online_paged_attention_matches_reference_text() {
    let (model, tokenizer) = load();
    let tokens = generate(&model, KvFormat::F32, AttentionMode::Online);
    let text = tokenizer.decode_all(BOS, &tokens).unwrap();
    assert_eq!(text, EXPECTED);
}

#[test]
fn bf16_kv_cache_keeps_the_reference_prefix() {
    let (model, tokenizer) = load();
    let reference = generate(&model, KvFormat::F32, AttentionMode::TwoPass);
    // Measured on x86_64 Linux: all 200 tokens agree. Assert a prefix that
    // tolerates platform `exp`/`sin`/`cos` differences.
    for mode in [AttentionMode::TwoPass, AttentionMode::Online] {
        let tokens = generate(&model, KvFormat::Bf16, mode);
        assert_eq!(tokens[..64], reference[..64]);
        let text = tokenizer.decode_all(BOS, &tokens[..20]).unwrap();
        assert!(EXPECTED.starts_with(&text));
    }
}

fn agreement(a: &[u32], b: &[u32]) -> usize {
    a.iter().zip(b).take_while(|(a, b)| a == b).count()
}

#[test]
fn quantized_weights_track_the_fp32_generation() {
    let (model, _) = load();
    let reference = generate(&model, KvFormat::F32, AttentionMode::TwoPass);
    // w2 has 172 input columns (not a multiple of 32) and stays FP32 in each
    // of the 5 layers. Measured agreement on x86_64 Linux: Q8_0 114 tokens,
    // Q4_0 27 tokens; the asserted floors leave slack for libm differences.
    for (format, floor) in [(WeightFormat::Q8_0, 48), (WeightFormat::Q4_0, 12)] {
        let (quantized, kept) = model.quantized(format).unwrap();
        assert_eq!(kept, 5);
        assert!(quantized.weight_bytes() < model.weight_bytes());
        let tokens = generate(&quantized, KvFormat::F32, AttentionMode::Online);
        let agree = agreement(&tokens, &reference);
        assert!(agree >= floor, "{format:?} agreed on only {agree} tokens");
    }
}

/// Straightforward f64 forward pass with contiguous K/V, independent of the
/// crate's kernels (matrices read through `dequantize_row`).
struct Reference {
    keys: Vec<Vec<Vec<f64>>>,
    values: Vec<Vec<Vec<f64>>>,
}
fn rows(m: &vesper_tensor::quant::Matrix, x: &[f64]) -> Vec<f64> {
    let mut row = vec![0.0f32; m.cols()];
    (0..m.rows())
        .map(|r| {
            m.dequantize_row(r, &mut row).unwrap();
            row.iter().zip(x).map(|(w, x)| f64::from(*w) * x).sum()
        })
        .collect()
}
fn norm(x: &[f64], w: &[f32]) -> Vec<f64> {
    let ms = x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64;
    let inv = 1.0 / (ms + 1e-5).sqrt();
    x.iter()
        .zip(w)
        .map(|(x, w)| x * inv * f64::from(*w))
        .collect()
}
fn rotate(x: &mut [f64], hd: usize, pos: usize) {
    for head in x.chunks_mut(hd) {
        for i in (0..hd).step_by(2) {
            let (s, c) = (pos as f64 * 10_000f64.powf(-(i as f64) / hd as f64)).sin_cos();
            let (a, b) = (head[i], head[i + 1]);
            head[i] = a * c - b * s;
            head[i + 1] = a * s + b * c;
        }
    }
}
impl Reference {
    fn step(&mut self, model: &Model, token: usize, pos: usize) -> Vec<f64> {
        let c = model.config();
        let hd = c.head_dim();
        let group = c.heads / c.kv_heads;
        let mut row = vec![0.0f32; c.dim];
        model.embedding().dequantize_row(token, &mut row).unwrap();
        let mut x: Vec<f64> = row.iter().map(|v| f64::from(*v)).collect();
        for (l, layer) in model.layers().iter().enumerate() {
            let xb = norm(&x, &layer.rms_attention);
            let mut q = rows(&layer.wq, &xb);
            let mut k = rows(&layer.wk, &xb);
            let v = rows(&layer.wv, &xb);
            rotate(&mut q, hd, pos);
            rotate(&mut k, hd, pos);
            self.keys[l].push(k);
            self.values[l].push(v);
            let mut att = vec![0.0; c.dim];
            for h in 0..c.heads {
                let kvh = h / group;
                let scores: Vec<f64> = self.keys[l]
                    .iter()
                    .map(|k| {
                        (0..hd)
                            .map(|i| q[h * hd + i] * k[kvh * hd + i])
                            .sum::<f64>()
                            / (hd as f64).sqrt()
                    })
                    .collect();
                let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let e: Vec<f64> = scores.iter().map(|s| (s - max).exp()).collect();
                let total: f64 = e.iter().sum();
                for (w, v) in e.iter().zip(&self.values[l]) {
                    for i in 0..hd {
                        att[h * hd + i] += w / total * v[kvh * hd + i];
                    }
                }
            }
            for (x, o) in x.iter_mut().zip(rows(&layer.wo, &att)) {
                *x += o;
            }
            let xb = norm(&x, &layer.rms_ffn);
            let gate = rows(&layer.w1, &xb);
            let up = rows(&layer.w3, &xb);
            let h: Vec<f64> = gate
                .iter()
                .zip(&up)
                .map(|(g, u)| g / (1.0 + (-g).exp()) * u)
                .collect();
            for (x, o) in x.iter_mut().zip(rows(&layer.w2, &h)) {
                *x += o;
            }
        }
        rows(model.classifier(), &norm(&x, model.rms_final()))
    }
}

#[test]
fn forward_logits_match_an_independent_f64_reference() {
    let (model, _) = load();
    let (q8, _) = model.quantized(WeightFormat::Q8_0).unwrap();
    for model in [&model, &q8] {
        let layers = model.config().layers;
        let mut reference = Reference {
            keys: vec![Vec::new(); layers],
            values: vec![Vec::new(); layers],
        };
        let mut cache = KvCache::new(model.config(), KvFormat::F32, 1).unwrap();
        let mut session = cache.begin().unwrap();
        let mut token = BOS as usize;
        let mut worst = 0.0f64;
        for pos in 0..40 {
            let expected = reference.step(model, token, pos);
            let logits = model
                .forward(
                    &mut cache,
                    &mut session,
                    token as u32,
                    AttentionMode::Online,
                )
                .unwrap();
            for (a, e) in logits.iter().zip(&expected) {
                worst = worst.max((f64::from(*a) - e).abs() / (1.0 + e.abs()));
            }
            token = vesper_tensor::ops::argmax(logits).unwrap();
        }
        assert!(worst < 1e-4, "worst relative logit error {worst}");
        cache.release(session).map_err(|(e, _)| e).unwrap();
    }
}

#[test]
fn sessions_are_explicitly_released_and_leaks_are_counted() {
    let (model, _) = load();
    let mut cache = KvCache::new(model.config(), KvFormat::F32, 2).unwrap();
    let mut other = KvCache::new(model.config(), KvFormat::F32, 1).unwrap();
    let mut session = cache.begin().unwrap();
    for _ in 0..17 {
        model
            .forward(&mut cache, &mut session, BOS, AttentionMode::Online)
            .unwrap();
    }
    assert_eq!(session.position(), 17);
    assert!(cache.pools().iter().all(|p| p.stats().allocated_pages == 2));
    // A foreign cache refuses both decoding and release, returning the owner.
    assert_eq!(
        model
            .forward(&mut other, &mut session, BOS, AttentionMode::Online)
            .unwrap_err(),
        Error::InvalidId
    );
    let (error, session) = other.release(session).unwrap_err();
    assert_eq!(error, Error::InvalidId);
    assert_eq!(
        generate_greedy(
            &model,
            &mut cache,
            &mut { session },
            &[BOS],
            1,
            AttentionMode::Online
        )
        .unwrap_err(),
        Error::Shape("generation needs a fresh session")
    );
    // (the block above moved and dropped the session: one counted leak)
    assert!(leaked_sessions() >= 1);
    assert!(cache.pools().iter().all(|p| p.stats().allocated_pages == 2));

    let mut fresh = cache.begin().unwrap();
    assert!(generate_greedy(
        &model,
        &mut cache,
        &mut fresh,
        &[2],
        4,
        AttentionMode::Online
    )
    .is_err());
    assert!(generate_greedy(
        &model,
        &mut cache,
        &mut fresh,
        &[BOS],
        513,
        AttentionMode::Online
    )
    .is_err());
    assert_eq!(
        model
            .forward(&mut cache, &mut fresh, 512, AttentionMode::Online)
            .unwrap_err(),
        Error::Shape("token outside the vocabulary")
    );
    cache.release(fresh).map_err(|(e, _)| e).unwrap();
    // Both slots are now in use (one leaked) so a third sequence is refused.
    let _held = cache.begin().unwrap();
    assert_eq!(
        cache.begin().unwrap_err(),
        Error::Kv(vesper_tensor::kv::KvError::SequenceLimit)
    );
    std::mem::forget(_held);
}

#[test]
fn cache_rejects_invalid_public_configs() {
    let (model, _) = load();
    let good = model.config();
    let cases = [
        vesper_tensor::llama::Config { heads: 0, ..good },
        vesper_tensor::llama::Config {
            kv_heads: 3,
            ..good
        },
        vesper_tensor::llama::Config { dim: 60, ..good },
        vesper_tensor::llama::Config { dim: 24, ..good },
        vesper_tensor::llama::Config { seq_len: 0, ..good },
        vesper_tensor::llama::Config {
            layers: 1000,
            ..good
        },
    ];
    for config in cases {
        assert!(
            KvCache::new(config, KvFormat::F32, 1).is_err(),
            "{config:?}"
        );
    }
    assert!(KvCache::new(good, KvFormat::F32, 0).is_err());
}

#[test]
fn sequence_length_limit_is_a_typed_error() {
    let (model, _) = load();
    let mut cache = KvCache::new(model.config(), KvFormat::F32, 1).unwrap();
    let mut session = cache.begin().unwrap();
    // Generation from BOS emits BOS (stops) after 345 tokens; feed tokens
    // directly to fill all 512 positions (32 pages per layer).
    for _ in 0..512 {
        model
            .forward(&mut cache, &mut session, 400, AttentionMode::Online)
            .unwrap();
    }
    assert!(cache
        .pools()
        .iter()
        .all(|p| p.stats().allocated_pages == 32));
    assert_eq!(
        model
            .forward(&mut cache, &mut session, BOS, AttentionMode::Online)
            .unwrap_err(),
        Error::Limit("sequence length")
    );
    assert_eq!(session.position(), 512);
    cache.release(session).map_err(|(e, _)| e).unwrap();
}

fn header(fields: [i32; 7]) -> Vec<u8> {
    fields.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[test]
fn malformed_checkpoints_are_typed_errors() {
    assert_eq!(
        Model::from_llama2c(&MODEL[..MODEL.len() - 1]).unwrap_err(),
        Error::Malformed("truncated input")
    );
    let mut extra = MODEL.to_vec();
    extra.push(0);
    assert_eq!(
        Model::from_llama2c(&extra).unwrap_err(),
        Error::Malformed("trailing bytes after checkpoint")
    );
    let mut nan = MODEL.to_vec();
    nan[28..32].copy_from_slice(&f32::NAN.to_le_bytes());
    assert_eq!(Model::from_llama2c(&nan).unwrap_err(), Error::NonFinite);
    assert_eq!(
        Model::from_llama2c(&[]).unwrap_err(),
        Error::Malformed("truncated input")
    );
    assert_eq!(
        Model::from_llama2c(&header([0, 1, 1, 1, 1, 1, 1])).unwrap_err(),
        Error::Malformed("dim")
    );
    assert_eq!(
        Model::from_llama2c(&header([64, 1, 1, 8, 3, 1, 1])).unwrap_err(),
        Error::Shape("invalid GQA ratio")
    );
    assert_eq!(
        Model::from_llama2c(&header([63, 1, 1, 8, 8, 1, 1])).unwrap_err(),
        Error::Shape("dim must be a multiple of heads")
    );
    assert_eq!(
        Model::from_llama2c(&header([24, 1, 1, 8, 8, 1, 1])).unwrap_err(),
        Error::Shape("RoPE needs an even head width")
    );
    assert_eq!(
        Model::from_llama2c(&header([64, 1, 1, 8, 8, i32::MIN, 1])).unwrap_err(),
        Error::Malformed("vocab")
    );
    assert_eq!(
        Model::from_llama2c(&header([1 << 20, 1, 1, 1, 1, 1, 1])).unwrap_err(),
        Error::Limit("dim")
    );
    // Huge but individually bounded dimensions overflow or truncate, never panic.
    assert!(Model::from_llama2c(&header([8192, 32768, 256, 8192, 8192, 1 << 20, 65536])).is_err());
}

#[test]
fn malformed_tokenizers_are_typed_errors() {
    assert!(Tokenizer::from_llama2c(&TOKENIZER[..TOKENIZER.len() - 1], 512).is_err());
    assert_eq!(
        Tokenizer::from_llama2c(TOKENIZER, 511).unwrap_err(),
        Error::Malformed("trailing bytes after tokenizer")
    );
    let mut long = TOKENIZER.to_vec();
    long[8..12].copy_from_slice(&1000i32.to_le_bytes()); // first piece longer than max
    assert_eq!(
        Tokenizer::from_llama2c(&long, 512).unwrap_err(),
        Error::Limit("token length")
    );
    let tokenizer = Tokenizer::from_llama2c(TOKENIZER, 512).unwrap();
    assert!(tokenizer.decode(BOS, 512).is_err());
}
