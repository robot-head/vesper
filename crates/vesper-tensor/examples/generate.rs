//! Greedy generation with the bundled stories260K checkpoint:
//! `cargo run -p vesper-tensor --example generate -- [steps] [f32|q8_0|q4_0]`.
//! Exits nonzero if the FP32 200-step output differs from llama2.c's recorded
//! reference.
use std::time::Instant;

use vesper_tensor::attention::AttentionMode;
use vesper_tensor::kv::KvFormat;
use vesper_tensor::llama::{generate_greedy, KvCache, Model, Tokenizer, BOS};
use vesper_tensor::quant::WeightFormat;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let steps: usize = args.next().map_or(Ok(200), |s| s.parse())?;
    let format = match args.next().as_deref() {
        None | Some("f32") => WeightFormat::F32,
        Some("q8_0") => WeightFormat::Q8_0,
        Some("q4_0") => WeightFormat::Q4_0,
        Some(other) => return Err(format!("unknown weight format {other}").into()),
    };
    let fp32 = Model::from_llama2c(include_bytes!("../models/stories260K.bin"))?;
    let tokenizer =
        Tokenizer::from_llama2c(include_bytes!("../models/tok512.bin"), fp32.config().vocab)?;
    let (model, kept) = fp32.quantized(format)?;
    let mut cache = KvCache::new(model.config(), KvFormat::F32, 1)?;
    let mut session = cache.begin()?;
    let start = Instant::now();
    let tokens = generate_greedy(
        &model,
        &mut cache,
        &mut session,
        &[BOS],
        steps,
        AttentionMode::Online,
    )?;
    let elapsed = start.elapsed();
    cache.release(session).map_err(|(e, _)| e)?;
    let text = tokenizer.decode_all(BOS, &tokens)?;
    println!("{}", String::from_utf8_lossy(&text));
    eprintln!(
        "{format:?} weights ({} bytes, {kept} matrices kept FP32): {} tokens in {elapsed:?}",
        model.weight_bytes(),
        tokens.len()
    );
    if format == WeightFormat::F32
        && steps == 200
        && text != include_bytes!("../models/stories260K-greedy-200.txt")
    {
        return Err("output differs from the llama2.c reference".into());
    }
    Ok(())
}
