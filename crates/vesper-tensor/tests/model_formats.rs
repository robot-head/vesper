//! End-to-end models from GGUF and safetensors, memory-mapped, and in the
//! quantized formats. The FP32 reference text is llama2.c's recorded output
//! (models/README.md); stories260K-q8_0.gguf was written by gguf-py 0.19.0
//! (fixtures/make_interop.py).
use std::path::PathBuf;

use vesper_tensor::attention::AttentionMode;
use vesper_tensor::gguf::Gguf;
use vesper_tensor::kv::KvFormat;
use vesper_tensor::llama::{generate_greedy, HfConfig, KvCache, Model, RopeStyle, Tokenizer, BOS};
use vesper_tensor::quant::WeightFormat;
use vesper_tensor::safetensors::SafeTensors;
use vesper_tensor::Error;

const MODEL: &[u8] = include_bytes!("../models/stories260K.bin");
const TOKENIZER: &[u8] = include_bytes!("../models/tok512.bin");
const EXPECTED: &[u8] = include_bytes!("../models/stories260K-greedy-200.txt");

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}
fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vesper-tensor-models-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}
/// Debug builds (as `cargo test` runs in CI) decode fewer steps for the
/// quantized-format sweep; release builds decode 200.
const SWEEP_STEPS: usize = if cfg!(debug_assertions) { 64 } else { 200 };

fn greedy(model: &Model) -> Vec<u32> {
    greedy_steps(model, 200)
}
fn greedy_steps(model: &Model, steps: usize) -> Vec<u32> {
    let mut cache = KvCache::new(model.config(), KvFormat::F32, 1).unwrap();
    let mut session = cache.begin().unwrap();
    let out = generate_greedy(
        model,
        &mut cache,
        &mut session,
        &[BOS],
        steps,
        AttentionMode::TwoPass,
    )
    .unwrap();
    cache.release(session).map_err(|(e, _)| e).unwrap();
    out
}
fn agreeing_prefix(a: &[u32], b: &[u32]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

#[test]
fn gguf_py_q8_0_model_maps_zero_copy_and_matches_our_q8_0_bit_for_bit() {
    let g = Gguf::open(&fixture("stories260K-q8_0.gguf")).unwrap();
    assert!(g.mapping().is_mapped());
    let model = Model::from_gguf(&g).unwrap();
    let reference = Model::from_llama2c(MODEL).unwrap();
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
    assert_eq!(c.rope, RopeStyle::Interleaved);
    // gguf-py's Q8_0 quantizer and ours agree on every block, so the model
    // equals our own Q8_0 conversion of the llama2.c checkpoint exactly.
    let (ours, kept) = reference.quantized(WeightFormat::Q8_0).unwrap();
    assert_eq!(kept, 5); // w2 (172 columns) in each layer stays FP32
    assert_eq!(model.layers(), ours.layers());
    assert_eq!(model.embedding(), ours.embedding());
    assert_eq!(greedy(&model), greedy(&ours));
    // Every matrix is borrowed from the mapping.
    for l in model.layers() {
        for m in [&l.wq, &l.wk, &l.wv, &l.wo, &l.w1, &l.w2, &l.w3] {
            assert!(m.is_file_mapped());
        }
    }
    assert!(model.embedding().is_file_mapped());
    assert_eq!(
        model.mapped_weight_bytes(),
        model.weight_bytes() - (5 * 2 + 1) * 64 * 4
    );
    // Its vocabulary is tok512's.
    let tokenizer = Tokenizer::from_gguf(&g).unwrap();
    assert_eq!(tokenizer.vocab(), 512);
    let tok512 = Tokenizer::from_llama2c(TOKENIZER, 512).unwrap();
    for id in 0..512 {
        assert_eq!(tokenizer.piece(id), tok512.piece(id), "piece {id}");
        assert_eq!(tokenizer.score(id), tok512.score(id), "score {id}");
    }
}

#[test]
fn fp32_gguf_and_safetensors_round_trips_reproduce_the_reference_text() {
    let reference = Model::from_llama2c(MODEL).unwrap();
    let tokenizer = Tokenizer::from_llama2c(TOKENIZER, 512).unwrap();
    let expected = greedy(&reference);
    assert_eq!(tokenizer.decode_all(BOS, &expected).unwrap(), EXPECTED);

    let path = temp("stories260K-f32.gguf");
    std::fs::write(&path, reference.to_gguf(Some(&tokenizer)).unwrap()).unwrap();
    let g = Gguf::open(&path).unwrap();
    let from_gguf = Model::from_gguf(&g).unwrap();
    assert_eq!(from_gguf, reference);
    assert_eq!(greedy(&from_gguf), expected);
    assert_eq!(Tokenizer::from_gguf(&g).unwrap(), tokenizer);

    // Hugging Face layout: Q/K rows permuted, rotate_half RoPE.
    let path = temp("stories260K-f32.safetensors");
    std::fs::write(&path, reference.to_safetensors().unwrap()).unwrap();
    let st = SafeTensors::open(&path).unwrap();
    let hf = HfConfig::from_metadata(&st).unwrap();
    assert_eq!(
        hf,
        HfConfig {
            heads: 8,
            kv_heads: 4,
            seq_len: 512,
            rope_base: 10_000.0,
            rms_epsilon: 1e-5
        }
    );
    let from_st = Model::from_safetensors(&st, hf).unwrap();
    // FP8 embedding and an FP8 lm_head, each with its `_scale` companion,
    // are accepted (not reported as unused tensors).
    {
        use vesper_tensor::formats::{quantize_fp8, Fp8};
        use vesper_tensor::safetensors::{self, Dtype, TensorData};
        let mut tensors: Vec<TensorData> = st
            .tensors()
            .iter()
            .map(|t| TensorData {
                name: t.name.clone(),
                dtype: t.dtype,
                shape: t.shape.clone(),
                bytes: st.bytes(t).to_vec(),
            })
            .collect();
        let embed = tensors
            .iter()
            .position(|t| t.name == "model.embed_tokens.weight")
            .unwrap();
        let values: Vec<f32> = tensors[embed]
            .bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let (codes, scale) = quantize_fp8(&values, Fp8::E4M3).unwrap();
        let shape = tensors[embed].shape.clone();
        tensors.remove(embed);
        for name in ["model.embed_tokens.weight", "lm_head.weight"] {
            tensors.push(TensorData {
                name: name.into(),
                dtype: Dtype::F8E4M3,
                shape: shape.clone(),
                bytes: codes.clone(),
            });
            tensors.push(TensorData {
                name: format!("{name}_scale"),
                dtype: Dtype::F32,
                shape: vec![],
                bytes: scale.to_le_bytes().to_vec(),
            });
        }
        let meta: Vec<(String, String)> = st.metadata().to_vec();
        let fp8 = SafeTensors::from_vec(safetensors::write(&tensors, &meta).unwrap()).unwrap();
        let m = Model::from_safetensors(&fp8, hf).unwrap();
        assert_eq!(m.embedding().format(), WeightFormat::F8E4M3);
        assert_eq!(m.classifier().format(), WeightFormat::F8E4M3);
        assert!(!m.config().shared_classifier);
    }
    assert_eq!(from_st.config().rope, RopeStyle::Half);
    assert!(from_st.mapped_weight_bytes() > 0);
    // Head elements are summed in a different order after the permutation,
    // so logits differ in the last bits; the greedy text is unchanged.
    assert_eq!(greedy(&from_st), expected);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn quantized_formats_through_their_containers() {
    let reference = Model::from_llama2c(MODEL).unwrap();
    let expected = greedy_steps(&reference, SWEEP_STEPS);
    let mut report = Vec::new();
    for format in [
        WeightFormat::F16,
        WeightFormat::Bf16,
        WeightFormat::Q8_0,
        WeightFormat::Q4_0,
        WeightFormat::Q4K,
        WeightFormat::Mxfp4,
        WeightFormat::F8E4M3,
        WeightFormat::F8E5M2,
    ] {
        let (model, kept) = reference.quantized(format).unwrap();
        // Every format goes through a container and back.
        let loaded = match format {
            WeightFormat::F8E4M3 | WeightFormat::F8E5M2 => {
                assert!(matches!(model.to_gguf(None), Err(Error::Unsupported(_))));
                let st = SafeTensors::from_vec(model.to_safetensors().unwrap()).unwrap();
                Model::from_safetensors(&st, HfConfig::from_metadata(&st).unwrap()).unwrap()
            }
            _ => {
                let g = Gguf::from_vec(model.to_gguf(None).unwrap()).unwrap();
                let m = Model::from_gguf(&g).unwrap();
                assert_eq!(m, model);
                m
            }
        };
        let tokens = greedy_steps(&loaded, SWEEP_STEPS);
        let agree = agreeing_prefix(&tokens, &expected);
        report.push(format!(
            "{format:?}: {agree}/{SWEEP_STEPS} leading tokens agree ({kept} kept)"
        ));
        let floor = match format {
            WeightFormat::F16 | WeightFormat::Bf16 => 32,
            WeightFormat::Q8_0 | WeightFormat::F8E4M3 => 16,
            _ => 1,
        };
        assert!(agree >= floor, "{format:?}: {agree}");
        assert!(!tokens.is_empty(), "{format:?}");
    }
    eprintln!("{}", report.join("\n"));
}

#[test]
fn malformed_model_containers_are_typed_errors() {
    let reference = Model::from_llama2c(MODEL).unwrap();
    let bytes = reference.to_gguf(None).unwrap();
    let g = Gguf::from_vec(bytes.clone()).unwrap();
    assert!(Model::from_gguf(&g).is_ok());
    // Not llama.
    let interop = Gguf::open(&fixture("interop.gguf")).unwrap();
    assert_eq!(
        Model::from_gguf(&interop).unwrap_err(),
        Error::Unsupported("GGUF architecture interop".into())
    );
    assert!(matches!(
        Tokenizer::from_gguf(&interop),
        Err(Error::MissingTensor(_))
    ));
    // Rewrite one metadata value through the writer.
    use vesper_tensor::gguf::{self, TensorData, Value};
    type Edit<'a> = dyn Fn(&mut Vec<(String, Value)>, &mut Vec<TensorData>) + 'a;
    let rebuild = |edit: &Edit| {
        let mut meta = g.metadata().to_vec();
        let mut tensors: Vec<TensorData> = g
            .tensors()
            .iter()
            .map(|t| TensorData {
                name: t.name.clone(),
                dims: t.dims.clone(),
                ty: t.ty,
                bytes: g.bytes(t).to_vec(),
            })
            .collect();
        edit(&mut meta, &mut tensors);
        Model::from_gguf(&Gguf::from_vec(gguf::write(&meta, &tensors).unwrap()).unwrap())
    };
    let set = |key: &'static str, v: Value| {
        move |m: &mut Vec<(String, Value)>, _: &mut Vec<TensorData>| {
            m.iter_mut().find(|(k, _)| k == key).unwrap().1 = v.clone();
        }
    };
    assert_eq!(
        rebuild(&set("llama.attention.head_count_kv", Value::U32(3))).unwrap_err(),
        Error::Shape("invalid GQA ratio")
    );
    assert_eq!(
        rebuild(&set("llama.rope.dimension_count", Value::U32(4))).unwrap_err(),
        Error::Unsupported("partial rotary embedding".into())
    );
    assert_eq!(
        rebuild(&set("llama.embedding_length", Value::U32(0))).unwrap_err(),
        Error::Malformed("dim")
    );
    assert_eq!(
        rebuild(&set("llama.block_count", Value::U32(1_000_000))).unwrap_err(),
        Error::Limit("layers")
    );
    assert_eq!(
        rebuild(&set("llama.block_count", Value::U32(6))).unwrap_err(),
        Error::MissingTensor("blk.5.attn_norm.weight".into())
    );
    assert_eq!(
        rebuild(&set(
            "llama.attention.layer_norm_rms_epsilon",
            Value::F32(-1.0)
        ))
        .unwrap_err(),
        Error::InvalidEpsilon
    );
    assert_eq!(
        rebuild(&set("llama.embedding_length", Value::String("64".into()))).unwrap_err(),
        Error::Malformed("GGUF integer metadata")
    );
    assert_eq!(
        rebuild(&|_, t| t.push(TensorData {
            name: "rope_freqs.weight".into(),
            dims: vec![4],
            ty: vesper_tensor::gguf::GgmlType::F32,
            bytes: vec![0; 16],
        }))
        .unwrap_err(),
        Error::Unsupported("unused GGUF tensor rope_freqs.weight".into())
    );
    assert_eq!(
        rebuild(&|_, t| {
            let q = t
                .iter_mut()
                .find(|t| t.name == "blk.0.attn_k.weight")
                .unwrap();
            q.dims = vec![64, 16];
            q.bytes.truncate(64 * 16 * 4);
        })
        .unwrap_err(),
        Error::Shape("attn_k")
    );
    assert_eq!(
        rebuild(&|_, t| {
            let n = t
                .iter_mut()
                .find(|t| t.name == "blk.0.ffn_norm.weight")
                .unwrap();
            n.bytes[0..4].copy_from_slice(&f32::NAN.to_le_bytes());
        })
        .unwrap_err(),
        Error::NonFinite
    );
    // Safetensors: missing hyperparameters, missing projections, extras.
    let st = SafeTensors::from_vec(reference.to_safetensors().unwrap()).unwrap();
    let hf = HfConfig::from_metadata(&st).unwrap();
    assert_eq!(
        Model::from_safetensors(&st, HfConfig { heads: 3, ..hf }).unwrap_err(),
        Error::Shape("dim must be a multiple of heads")
    );
    assert_eq!(
        Model::from_safetensors(&st, HfConfig { seq_len: 0, ..hf }).unwrap_err(),
        Error::Malformed("safetensors hyperparameters")
    );
    let plain = SafeTensors::from_vec(
        vesper_tensor::safetensors::write(
            &[vesper_tensor::safetensors::TensorData {
                name: "model.embed_tokens.weight".into(),
                dtype: vesper_tensor::safetensors::Dtype::F32,
                shape: vec![2, 2],
                bytes: vec![0; 16],
            }],
            &[],
        )
        .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        HfConfig::from_metadata(&plain),
        Err(Error::MissingTensor(_))
    ));
    assert_eq!(
        Model::from_safetensors(&plain, hf).unwrap_err(),
        Error::MissingTensor("model.layers.0.mlp.gate_proj.weight".into())
    );
}

mod common;

#[test]
fn block_formats_track_fp32_logits_on_a_model_wide_enough_for_q4_k() {
    let reference = common::synthetic();
    let logits = |m: &Model| -> Vec<f32> {
        let mut cache = KvCache::new(m.config(), KvFormat::F32, 1).unwrap();
        let mut s = cache.begin().unwrap();
        let mut last = Vec::new();
        for t in [1u32, 17, 42, 5, 77, 3, 60, 9] {
            last = m
                .forward(&mut cache, &mut s, t, AttentionMode::TwoPass)
                .unwrap()
                .to_vec();
        }
        cache.release(s).map_err(|(e, _)| e).unwrap();
        last
    };
    let base = logits(&reference);
    let cosine = |a: &[f32], b: &[f32]| {
        let dot: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| f64::from(*x) * f64::from(*y))
            .sum();
        let na: f64 = a.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
        let nb: f64 = b.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
        dot / (na * nb)
    };
    let mut report = Vec::new();
    for (format, floor) in [
        (WeightFormat::F16, 0.99999),
        (WeightFormat::Bf16, 0.9999),
        (WeightFormat::Q8_0, 0.9999),
        (WeightFormat::F8E4M3, 0.999),
        (WeightFormat::F8E5M2, 0.995),
        (WeightFormat::Q4K, 0.99),
        (WeightFormat::Q4_0, 0.99),
        (WeightFormat::Mxfp4, 0.98),
    ] {
        let (m, kept) = reference.quantized(format).unwrap();
        assert_eq!(kept, 0);
        let c = cosine(&base, &logits(&m));
        report.push(format!("{format:?}: logits cosine {c:.6}"));
        assert!(c >= floor, "{format:?}: {c}");
    }
    eprintln!("{}", report.join("\n"));
}
