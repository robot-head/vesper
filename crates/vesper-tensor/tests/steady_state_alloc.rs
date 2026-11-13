//! Counting global allocator: decode steps after `KvCache::begin` allocate
//! nothing. One test per binary so no other test allocates concurrently.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use vesper_tensor::attention::AttentionMode;
use vesper_tensor::kv::KvFormat;
use vesper_tensor::llama::{KvCache, Model, BOS};
use vesper_tensor::ops::argmax;
use vesper_tensor::quant::WeightFormat;

struct Counting;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn decode_steps_perform_no_heap_allocation() {
    let fp32 = Model::from_llama2c(include_bytes!("../models/stories260K.bin")).unwrap();
    let (q8, _) = fp32.quantized(WeightFormat::Q8_0).unwrap();
    let (q4, _) = fp32.quantized(WeightFormat::Q4_0).unwrap();
    for model in [&fp32, &q8, &q4] {
        for format in [KvFormat::F32, KvFormat::Bf16] {
            for mode in [AttentionMode::TwoPass, AttentionMode::Online] {
                let mut cache = KvCache::new(model.config(), format, 1).unwrap();
                let mut session = cache.begin().unwrap();
                let before = ALLOCATIONS.load(Ordering::Relaxed);
                // Crosses page boundaries at 16, 32, ... (new page leases).
                let mut token = BOS;
                for _ in 0..100 {
                    let logits = model
                        .forward(&mut cache, &mut session, token, mode)
                        .unwrap();
                    token = argmax(logits).unwrap() as u32;
                    if token == BOS {
                        token = 2;
                    }
                }
                let after = ALLOCATIONS.load(Ordering::Relaxed);
                assert_eq!(after - before, 0, "{format:?} {mode:?}");
                cache.release(session).map_err(|(e, _)| e).unwrap();
            }
        }
    }
}
