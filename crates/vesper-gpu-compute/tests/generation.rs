//! Greedy decoding of the bundled stories260K model on the device against
//! vesper-tensor's CPU decoder and the llama2.c reference text.
use vesper_gpu_compute::tensor::{GpuKvCache, GpuModel, leaked_sessions};
use vesper_gpu_compute::{GpuError, test_context};
use vesper_tensor::attention::AttentionMode;
use vesper_tensor::kv::KvFormat;
use vesper_tensor::llama::{BOS, KvCache, Model, Tokenizer, generate_greedy};
use vesper_tensor::quant::WeightFormat;

const MODEL: &[u8] = include_bytes!("../../vesper-tensor/models/stories260K.bin");
const TOKENIZER: &[u8] = include_bytes!("../../vesper-tensor/models/tok512.bin");
const REFERENCE: &str = include_str!("../../vesper-tensor/models/stories260K-greedy-200.txt");

fn cpu_tokens(model: &Model, steps: usize) -> Vec<u32> {
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
    cache.release(session).unwrap();
    out
}

#[test]
fn greedy_decoding_matches_the_cpu_tokens_and_the_reference_text() {
    let Some(ctx) = test_context() else { return };
    eprintln!("device: {:?}", ctx.info());
    let model = Model::from_llama2c(MODEL).unwrap();
    let tokenizer = Tokenizer::from_llama2c(TOKENIZER, model.config().vocab).unwrap();
    let gpu = GpuModel::upload(&ctx, &model).unwrap();
    let mut cache = GpuKvCache::new(&gpu, 64).unwrap();
    let mut session = cache.begin(&gpu).unwrap();
    let tokens = gpu
        .generate_greedy(&mut cache, &mut session, &[BOS], 200)
        .unwrap();
    // 200 positions lease 13 pages, from the top of the range down.
    assert_eq!(session.pages().len(), 13);
    assert_eq!(session.pages()[0], 63);
    cache.release(session).unwrap();
    assert_eq!(cache.free_pages(), 64);
    let cpu = cpu_tokens(&model, 200);
    let agree = tokens.iter().zip(&cpu).take_while(|(a, b)| a == b).count();
    eprintln!("GPU and CPU agree on {agree} of {} tokens", cpu.len());
    assert_eq!(tokens, cpu, "GPU greedy tokens differ from the CPU decoder");
    let text = String::from_utf8(tokenizer.decode_all(BOS, &tokens).unwrap()).unwrap();
    assert_eq!(text.trim_end(), REFERENCE.trim_end());
}

#[test]
fn logits_track_the_cpu_decoder_within_tolerance() {
    let Some(ctx) = test_context() else { return };
    let model = Model::from_llama2c(MODEL).unwrap();
    let gpu = GpuModel::upload(&ctx, &model).unwrap();
    let mut gcache = GpuKvCache::new(&gpu, 8).unwrap();
    let mut gsession = gcache.begin(&gpu).unwrap();
    let mut cache = KvCache::new(model.config(), KvFormat::F32, 1).unwrap();
    let mut session = cache.begin().unwrap();
    let mut token = BOS;
    let mut worst = 0.0f64;
    for _ in 0..40 {
        let cpu = model
            .forward(&mut cache, &mut session, token, AttentionMode::TwoPass)
            .unwrap()
            .to_vec();
        let g = gpu.forward(&mut gcache, &mut gsession, token).unwrap();
        for (a, b) in g.iter().zip(&cpu) {
            let err = f64::from((a - b).abs()) / (1.0 + f64::from(b.abs()));
            worst = worst.max(err);
        }
        token = vesper_tensor::ops::argmax(&cpu).unwrap() as u32;
    }
    eprintln!("worst relative logit error over 40 positions: {worst:e}");
    assert!(worst < 1e-4, "logits differ by {worst}");
    gcache.release(gsession).unwrap();
    cache.release(session).unwrap();
}

#[test]
fn quantized_weights_foreign_sessions_and_leaks_are_typed() {
    let Some(ctx) = test_context() else { return };
    let model = Model::from_llama2c(MODEL).unwrap();
    let (q8, _) = model.quantized(WeightFormat::Q8_0).unwrap();
    assert!(matches!(
        GpuModel::upload(&ctx, &q8),
        Err(GpuError::Unsupported(_))
    ));
    let gpu = GpuModel::upload(&ctx, &model).unwrap();
    let mut a = GpuKvCache::new(&gpu, 2).unwrap();
    let mut b = GpuKvCache::new(&gpu, 2).unwrap();
    let session = a.begin(&gpu).unwrap();
    let (err, session) = b.release(session).unwrap_err();
    assert_eq!(err, GpuError::ForeignObject);
    let mut session = *session;
    assert_eq!(
        gpu.forward(&mut b, &mut session, BOS).unwrap_err(),
        GpuError::ForeignObject
    );
    // Two pages hold 32 positions; the 33rd needs a third page.
    for _ in 0..32 {
        gpu.forward(&mut a, &mut session, BOS).unwrap();
    }
    assert_eq!(
        gpu.forward(&mut a, &mut session, BOS).unwrap_err(),
        GpuError::Limit("K/V pages")
    );
    assert_eq!(
        gpu.forward(&mut a, &mut session, 100_000).unwrap_err(),
        GpuError::Shape("token outside the vocabulary")
    );
    a.release(session).unwrap();
    let before = leaked_sessions();
    drop(a.begin(&gpu).unwrap());
    assert_eq!(leaked_sessions(), before + 1);
}
