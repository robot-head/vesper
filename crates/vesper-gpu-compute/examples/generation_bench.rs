//! One timed run of the decoding workload, for the ABBA comparison that
//! `port/bench-abba.sh generation` drives against the Rust tensor driver at
//! its last commit. Prints a label, the elapsed nanoseconds and a digest of
//! the tokens, so both arms are seen to compute the same thing.
//!
//! The workload: open a device, upload the bundled stories260K model, then
//! greedily decode 200 tokens `ROUNDS` times, one submission per token.
use std::time::Instant;

use vesper_gpu_compute::Context;
use vesper_gpu_compute::tensor::{GpuKvCache, GpuModel};
use vesper_tensor::llama::{BOS, Model};

const MODEL: &[u8] = include_bytes!("../../vesper-tensor/models/stories260K.bin");
const ROUNDS: u32 = 3;

fn main() {
    let start = Instant::now();
    let ctx = Context::new().expect("a Vulkan device");
    let model = Model::from_llama2c(MODEL).unwrap();
    let gpu = GpuModel::upload(&ctx, &model).unwrap();
    let mut cache = GpuKvCache::new(&gpu, 64).unwrap();
    let mut digest = 0u64;
    for _ in 0..ROUNDS {
        let mut session = cache.begin(&gpu).unwrap();
        let tokens = gpu
            .generate_greedy(&mut cache, &mut session, &[BOS], 200)
            .unwrap();
        cache.release(session).unwrap();
        digest = tokens
            .iter()
            .fold(digest, |h, &t| h.rotate_left(5) ^ u64::from(t));
    }
    println!("vesper {} {digest:016x}", start.elapsed().as_nanos());
}
