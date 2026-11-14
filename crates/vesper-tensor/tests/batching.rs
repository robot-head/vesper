//! Batched prefill and multi-sequence batching: one forward call gives every
//! sequence exactly the logits of running its tokens one at a time, for both
//! kernels, both attention schedules, both K/V formats and every weight
//! format. Greedy decoding with the SIMD kernel and with batched prefill is
//! token-for-token equal to the scalar path and to llama2.c's reference text.
mod common;

use vesper_tensor::attention::AttentionMode;
use vesper_tensor::kv::KvFormat;
use vesper_tensor::llama::{
    generate, generate_greedy, greedy_sampler, BatchItem, KvCache, Model, RunOptions, Tokenizer,
    BOS,
};
use vesper_tensor::quant::WeightFormat;
use vesper_tensor::simd::Kernel;
use vesper_tensor::Error;

const TOKENIZER: &[u8] = include_bytes!("../models/tok512.bin");
const EXPECTED: &[u8] = include_bytes!("../models/stories260K-greedy-200.txt");

/// Debug builds (as `cargo test` runs in CI) cover a representative subset;
/// release builds run every combination.
const FULL: bool = !cfg!(debug_assertions);

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}
fn options() -> impl Iterator<Item = RunOptions> {
    [Kernel::Scalar, Kernel::Simd]
        .into_iter()
        .flat_map(|kernel| {
            [AttentionMode::TwoPass, AttentionMode::Online]
                .into_iter()
                .map(move |attention| RunOptions { attention, kernel })
        })
}
fn tokens(seed: u64, n: usize, vocab: usize) -> Vec<u32> {
    let mut rng = common::Rng(seed);
    (0..n)
        .map(|_| (rng.next_u64() % vocab as u64) as u32)
        .collect()
}

/// Logits after each item's tokens, fed one at a time.
fn sequential(model: &Model, format: KvFormat, prompt: &[u32], opts: RunOptions) -> Vec<u32> {
    let mut cache = KvCache::new(model.config(), format, 1).unwrap();
    let mut s = cache.begin().unwrap();
    let mut last = Vec::new();
    for &t in prompt {
        last = bits(model.forward_with(&mut cache, &mut s, t, opts).unwrap());
    }
    cache.release(s).map_err(|(e, _)| e).unwrap();
    last
}

#[test]
fn prefill_logits_are_bit_identical_to_one_token_steps() {
    let stories = common::stories();
    let synthetic = common::synthetic();
    let mut models = vec![("stories f32", stories.clone())];
    models.push((
        "stories q8_0",
        stories.quantized(WeightFormat::Q8_0).unwrap().0,
    ));
    for format in [
        WeightFormat::F32,
        WeightFormat::Q4K,
        WeightFormat::Mxfp4,
        WeightFormat::F8E4M3,
        WeightFormat::Bf16,
        WeightFormat::Q4_0,
    ] {
        let (m, kept) = synthetic.quantized(format).unwrap();
        assert_eq!(kept, 0, "{format:?}");
        models.push(("synthetic", m));
    }
    for (name, model) in &models {
        let vocab = model.config().vocab;
        let lens: &[usize] = if FULL { &[1, 15, 16, 17, 40] } else { &[1, 17] };
        let kv_formats: &[KvFormat] = if FULL || *name == "stories f32" {
            &[KvFormat::F32, KvFormat::Bf16]
        } else {
            &[KvFormat::F32]
        };
        for opts in options() {
            for &format in kv_formats {
                for &len in lens {
                    let prompt = tokens(len as u64 * 31 + 7, len, vocab);
                    let mut cache = KvCache::new(model.config(), format, 1).unwrap();
                    let mut s = cache.begin().unwrap();
                    let mut scratch = model.batch_scratch(64, 1).unwrap();
                    model
                        .forward_batch(
                            &mut cache,
                            &mut [BatchItem {
                                session: &mut s,
                                tokens: &prompt,
                            }],
                            &mut scratch,
                            opts,
                        )
                        .unwrap();
                    assert_eq!(s.position(), len);
                    assert_eq!(
                        bits(s.logits()),
                        sequential(model, format, &prompt, opts),
                        "{name} {opts:?} {format:?} len {len}"
                    );
                    cache.release(s).map_err(|(e, _)| e).unwrap();
                }
            }
        }
    }
}

#[test]
fn several_sequences_in_one_call_match_their_own_runs() {
    let model = common::synthetic().quantized(WeightFormat::Q4K).unwrap().0;
    let vocab = model.config().vocab;
    for opts in options() {
        let prompts = [
            tokens(1, 5, vocab),
            tokens(2, 17, vocab),
            tokens(3, 1, vocab),
        ];
        let mut cache = KvCache::new(model.config(), KvFormat::F32, 3).unwrap();
        let mut sessions: Vec<_> = (0..3).map(|_| cache.begin().unwrap()).collect();
        let mut scratch = model.batch_scratch(32, 3).unwrap();
        {
            let [a, b, c] = sessions.as_mut_slice() else {
                unreachable!()
            };
            let mut items = [
                BatchItem {
                    session: a,
                    tokens: &prompts[0],
                },
                BatchItem {
                    session: b,
                    tokens: &prompts[1],
                },
                BatchItem {
                    session: c,
                    tokens: &prompts[2],
                },
            ];
            model
                .forward_batch(&mut cache, &mut items, &mut scratch, opts)
                .unwrap();
        }
        for (s, p) in sessions.iter().zip(&prompts) {
            assert_eq!(bits(s.logits()), sequential(&model, KvFormat::F32, p, opts));
        }
        // Decode 24 greedy rounds, all three sequences per call, crossing
        // page boundaries at different steps, against separate runs.
        let mut histories: Vec<Vec<u32>> = prompts.to_vec();
        for _ in 0..24 {
            let next: Vec<[u32; 1]> = sessions
                .iter()
                .map(|s| [vesper_tensor::ops::argmax(s.logits()).unwrap() as u32])
                .collect();
            for (h, n) in histories.iter_mut().zip(&next) {
                h.push(n[0]);
            }
            let mut items: Vec<BatchItem<'_>> = sessions
                .iter_mut()
                .zip(&next)
                .map(|(session, n)| BatchItem {
                    session,
                    tokens: n.as_slice(),
                })
                .collect();
            model
                .forward_batch(&mut cache, &mut items, &mut scratch, opts)
                .unwrap();
        }
        for (s, h) in sessions.iter().zip(&histories) {
            assert_eq!(s.position(), h.len());
            assert_eq!(bits(s.logits()), sequential(&model, KvFormat::F32, h, opts));
        }
        // A prefill and a decode step together in one call.
        let extra = tokens(9, 6, vocab);
        let one = [histories[0][0]];
        {
            let [a, b, _] = sessions.as_mut_slice() else {
                unreachable!()
            };
            model
                .forward_batch(
                    &mut cache,
                    &mut [
                        BatchItem {
                            session: a,
                            tokens: &extra,
                        },
                        BatchItem {
                            session: b,
                            tokens: &one,
                        },
                    ],
                    &mut scratch,
                    opts,
                )
                .unwrap();
        }
        histories[0].extend_from_slice(&extra);
        histories[1].push(one[0]);
        for (s, h) in sessions.iter().zip(&histories).take(2) {
            assert_eq!(bits(s.logits()), sequential(&model, KvFormat::F32, h, opts));
        }
        for s in sessions {
            cache.release(s).map_err(|(e, _)| e).unwrap();
        }
        for pool in cache.pools() {
            assert_eq!(pool.stats().allocated_pages, 0);
        }
    }
}

#[test]
fn simd_and_batched_greedy_decoding_reproduce_the_scalar_reference() {
    let model = common::stories();
    let tokenizer = Tokenizer::from_llama2c(TOKENIZER, 512).unwrap();
    let mut cache = KvCache::new(model.config(), KvFormat::F32, 1).unwrap();
    let mut s = cache.begin().unwrap();
    let scalar = generate_greedy(
        &model,
        &mut cache,
        &mut s,
        &[BOS],
        200,
        AttentionMode::TwoPass,
    )
    .unwrap();
    cache.release(s).map_err(|(e, _)| e).unwrap();
    assert_eq!(tokenizer.decode_all(BOS, &scalar).unwrap(), EXPECTED);
    eprintln!("SIMD ISA: {:?}", vesper_tensor::simd::simd_isa());
    for opts in options() {
        for format in [KvFormat::F32, KvFormat::Bf16] {
            let in_subset = (opts.attention == AttentionMode::TwoPass) == (format == KvFormat::F32);
            if !(FULL || opts.kernel == Kernel::Simd && in_subset) {
                continue;
            }
            let mut cache = KvCache::new(model.config(), format, 1).unwrap();
            let mut s = cache.begin().unwrap();
            let mut sampler = greedy_sampler(&model).unwrap();
            let out =
                generate(&model, &mut cache, &mut s, &[BOS], 200, &mut sampler, opts).unwrap();
            cache.release(s).map_err(|(e, _)| e).unwrap();
            assert_eq!(out, scalar, "{opts:?} {format:?}");
        }
    }
    // With a prompt: batched prefill + sampled decode equals token-at-a-time
    // greedy decoding, including prompts longer than `steps`.
    let encoder = tokenizer.encoder();
    let prompt = encoder
        .encode(b"Once upon a time, Lily", true, false)
        .unwrap();
    let steps_list = if FULL {
        vec![3, prompt.len(), prompt.len() + 1, 120]
    } else {
        vec![3, prompt.len() + 1, 48]
    };
    for steps in steps_list {
        for opts in options() {
            if !FULL
                && (opts.kernel == Kernel::Scalar) != (opts.attention == AttentionMode::TwoPass)
            {
                continue;
            }
            let mut cache = KvCache::new(model.config(), KvFormat::F32, 2).unwrap();
            let mut a = cache.begin().unwrap();
            let mut b = cache.begin().unwrap();
            let expected =
                generate_greedy(&model, &mut cache, &mut a, &prompt, steps, opts.attention)
                    .unwrap();
            let mut sampler = greedy_sampler(&model).unwrap();
            let got = generate(
                &model,
                &mut cache,
                &mut b,
                &prompt,
                steps,
                &mut sampler,
                opts,
            )
            .unwrap();
            if opts.kernel == Kernel::Scalar {
                assert_eq!(got, expected, "steps {steps} {opts:?}");
            } else {
                // SIMD reassociates dot products; the text is still identical.
                assert_eq!(
                    tokenizer.decode_all(BOS, &got).unwrap(),
                    tokenizer.decode_all(BOS, &expected).unwrap(),
                    "steps {steps} {opts:?}"
                );
            }
            cache.release(a).map_err(|(e, _)| e).unwrap();
            cache.release(b).map_err(|(e, _)| e).unwrap();
        }
    }
}

#[test]
fn invalid_batches_fail_before_any_mutation() {
    let model = common::stories();
    let c = model.config();
    let mut cache = KvCache::new(c, KvFormat::F32, 2).unwrap();
    let mut other = KvCache::new(c, KvFormat::F32, 1).unwrap();
    let mut a = cache.begin().unwrap();
    let mut foreign = other.begin().unwrap();
    let mut scratch = model.batch_scratch(8, 2).unwrap();
    let opts = RunOptions::SCALAR;
    let stats = |cache: &KvCache| cache.pools().iter().map(|p| p.stats()).collect::<Vec<_>>();
    let before = stats(&cache);
    let mut run = |cache: &mut KvCache, session: &mut _, tokens: &[u32]| {
        model.forward_batch(
            cache,
            &mut [BatchItem { session, tokens }],
            &mut scratch,
            opts,
        )
    };
    assert_eq!(run(&mut cache, &mut foreign, &[BOS]), Err(Error::InvalidId));
    assert_eq!(
        run(&mut cache, &mut a, &[]),
        Err(Error::Shape("batch item without tokens"))
    );
    assert_eq!(
        run(&mut cache, &mut a, &[BOS, 512]),
        Err(Error::Shape("token outside the vocabulary"))
    );
    assert_eq!(
        run(&mut cache, &mut a, &[BOS; 9]),
        Err(Error::Limit("batch tokens"))
    );
    assert_eq!(stats(&cache), before);
    assert_eq!(a.position(), 0);
    // Context limit: fill to 510, then 3 more tokens would exceed 512.
    let mut big = model.batch_scratch(512, 1).unwrap();
    model
        .forward_batch(
            &mut cache,
            &mut [BatchItem {
                session: &mut a,
                tokens: &[BOS; 510],
            }],
            &mut big,
            opts,
        )
        .unwrap();
    let before = stats(&cache);
    assert_eq!(
        model.forward_batch(
            &mut cache,
            &mut [BatchItem {
                session: &mut a,
                tokens: &[BOS; 3],
            }],
            &mut big,
            opts,
        ),
        Err(Error::Limit("sequence length"))
    );
    assert_eq!(stats(&cache), before);
    assert_eq!(a.position(), 510);
    // Scratch for another model shape, too many items, bad capacities.
    let synthetic = common::synthetic();
    let mut wrong = synthetic.batch_scratch(4, 1).unwrap();
    assert_eq!(
        model.forward_batch(
            &mut cache,
            &mut [BatchItem {
                session: &mut a,
                tokens: &[BOS],
            }],
            &mut wrong,
            opts,
        ),
        Err(Error::InvalidId)
    );
    assert_eq!(
        model.batch_scratch(0, 1).unwrap_err(),
        Error::Shape("batch capacity")
    );
    assert_eq!(
        model.batch_scratch(2, 3).unwrap_err(),
        Error::Shape("batch capacity")
    );
    assert_eq!(
        model.batch_scratch(1 << 20, 1).unwrap_err(),
        Error::Limit("batch tokens")
    );
    let mut b = cache.begin().unwrap();
    let mut one = model.batch_scratch(2, 1).unwrap();
    assert_eq!(
        model.forward_batch(
            &mut cache,
            &mut [
                BatchItem {
                    session: &mut a,
                    tokens: &[BOS],
                },
                BatchItem {
                    session: &mut b,
                    tokens: &[BOS],
                },
            ],
            &mut one,
            opts,
        ),
        Err(Error::Shape("batch item count"))
    );
    cache.release(a).map_err(|(e, _)| e).unwrap();
    cache.release(b).map_err(|(e, _)| e).unwrap();
    other.release(foreign).map_err(|(e, _)| e).unwrap();
}
