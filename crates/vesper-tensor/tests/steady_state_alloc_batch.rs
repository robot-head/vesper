//! Counting global allocator over warmed-up steady states: SIMD decode steps,
//! batched multi-sequence decode and prefill, and top-p sampling perform no
//! heap allocation; loading a memory-mapped GGUF allocates only a small
//! fraction of the weight bytes it maps. One test per binary so nothing else
//! allocates concurrently.
mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use vesper_tensor::attention::AttentionMode;
use vesper_tensor::gguf::Gguf;
use vesper_tensor::kv::KvFormat;
use vesper_tensor::llama::{BatchItem, KvCache, Model, RunOptions, BOS};
use vesper_tensor::quant::WeightFormat;
use vesper_tensor::sampling::{Sampler, SamplerConfig};
use vesper_tensor::simd::Kernel;

struct Counting;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations() -> usize {
    ALLOCATIONS.load(Ordering::Relaxed)
}

#[test]
fn simd_batched_and_sampled_steady_states_allocate_nothing() {
    let stories = common::stories();
    let synthetic_q4k = common::synthetic().quantized(WeightFormat::Q4K).unwrap().0;
    for (model, mode) in [
        (&stories, AttentionMode::TwoPass),
        (&stories, AttentionMode::Online),
        (&synthetic_q4k, AttentionMode::Online),
    ] {
        let opts = RunOptions {
            attention: mode,
            kernel: Kernel::Simd,
        };
        let vocab = model.config().vocab;
        // Single-sequence SIMD decode with top-p sampling.
        let mut cache = KvCache::new(model.config(), KvFormat::Bf16, 3).unwrap();
        let mut s = cache.begin().unwrap();
        let mut sampler = Sampler::new(
            vocab,
            SamplerConfig {
                temperature: 0.9,
                top_p: 0.9,
                seed: 5,
            },
        )
        .unwrap();
        let mut token = BOS;
        let before = allocations();
        for _ in 0..20 {
            let logits = model.forward_with(&mut cache, &mut s, token, opts).unwrap();
            token = sampler.sample(logits).unwrap();
        }
        assert_eq!(allocations() - before, 0, "SIMD decode + top-p");
        // Batched: prefill two sequences, then decode three per call.
        let mut b = cache.begin().unwrap();
        let mut c = cache.begin().unwrap();
        let mut scratch = model.batch_scratch(48, 3).unwrap();
        let prompt_b = [BOS; 20];
        let prompt_c = [BOS; 17];
        let before = allocations();
        model
            .forward_batch(
                &mut cache,
                &mut [
                    BatchItem {
                        session: &mut b,
                        tokens: &prompt_b,
                    },
                    BatchItem {
                        session: &mut c,
                        tokens: &prompt_c,
                    },
                ],
                &mut scratch,
                opts,
            )
            .unwrap();
        for _ in 0..20 {
            let next = [
                [sampler.sample(s.logits()).unwrap()],
                [sampler.sample(b.logits()).unwrap()],
                [sampler.sample(c.logits()).unwrap()],
            ];
            model
                .forward_batch(
                    &mut cache,
                    &mut [
                        BatchItem {
                            session: &mut s,
                            tokens: &next[0],
                        },
                        BatchItem {
                            session: &mut b,
                            tokens: &next[1],
                        },
                        BatchItem {
                            session: &mut c,
                            tokens: &next[2],
                        },
                    ],
                    &mut scratch,
                    opts,
                )
                .unwrap();
        }
        assert_eq!(allocations() - before, 0, "batched prefill + decode");
        for session in [s, b, c] {
            cache.release(session).map_err(|(e, _)| e).unwrap();
        }
    }

    // Zero-copy load: mapping and parsing allocate metadata, the vocabulary
    // and the copied norm vectors, not the matrices.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/stories260K-q8_0.gguf");
    let before = BYTES.load(Ordering::Relaxed);
    let g = Gguf::open(&path).unwrap();
    let model = Model::from_gguf(&g).unwrap();
    let allocated = BYTES.load(Ordering::Relaxed) - before;
    let mapped = model.mapped_weight_bytes();
    eprintln!("GGUF load: {allocated} bytes allocated, {mapped} weight bytes mapped");
    assert!(mapped > 400_000);
    assert!(
        allocated * 8 < mapped,
        "{allocated} allocated vs {mapped} mapped"
    );
}
