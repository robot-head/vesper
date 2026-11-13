# Wave six lane 21: Tensor end-to-end CPU inference with paged attention (§28)

Claim: [#263](https://github.com/robot-head/vesper/issues/263). Base:
`07322e3ba47890e939a48967a1ea8f62fdd7aac1` (descendant of the required
`cda092f`). Edits are confined to `crates/vesper-tensor/**` and this report.
No new dependencies; root `Cargo.toml`, `Cargo.lock` and `BUILD.bazel` files are
unchanged.

## What was built

`vesper-tensor` now runs a real public decoder-only model end to end on the
CPU and reproduces the upstream reference output byte for byte.

| Module | Contents |
| --- | --- |
| `ops` | RMSNorm, RoPE (interleaved pairs, base 10000), SiLU, SwiGLU, softmax, argmax, residual add, and the hand-fused `swiglu_matvec` |
| `quant` | `Matrix` in FP32, Q8_0 (34-byte blocks) or Q4_0 (18-byte blocks, the existing layout); GGML reference quantizers; dequantizing matvec; round-to-nearest-even binary16 and BF16 encoders |
| `attention` | `paged_gqa_attention`: grouped-query decode attention that reads K/V page by page through the pool's validated leases, with a two-pass schedule (llama2.c order) and an online FlashDecoding-style schedule (running max and sum, merged per 16-token page, no score buffer) |
| `kv` (extended) | `KvFormat::{Bf16, F32}`, `KvPool::with_format`, `append_f32`, `page_count`, `read_page`/`KvPage`, `KvError::Format`. `KvPool::new` and BF16 behaviour are unchanged |
| `llama` | llama2.c checkpoint and tokenizer parsers, `Model::forward`, `Model::quantized`, `KvCache` (one `KvPool` per layer), `Session`, `generate_greedy`, `Tokenizer::decode` |

The decode step follows design §28.3: RMSNorm, Q/K/V projections, RoPE on Q
and K, append K/V to the layer's pages (a new 16-token page lease is taken
exactly when `position % 16 == 0`), paged GQA attention, output projection and
residual, RMSNorm, fused SwiGLU, down projection and residual, final RMSNorm and
the classifier.

### Model and reference

The bundled model is Karpathy's `stories260K` (llama2.c format, MIT licence;
dim 64, 5 layers, 8 query heads over 4 KV heads, vocabulary 512, context 512).
Provenance, hashes and licence are in `crates/vesper-tensor/models/README.md`.
Hugging Face is blocked from the cloud host, so the files came from two
independent public GitHub mirrors with byte-identical blobs.

The reference is the `expected_stdout` that llama2.c's own `test_all.py`
(commit `350e04f`) checks `run.c` and the PyTorch model against: 200 greedy
steps from BOS. Our FP32 output matches it byte for byte with both attention
schedules, which also confirms that the mirrored files are the upstream ones.

`cargo run --release -p vesper-tensor --example generate` prints the story and
exits nonzero if the FP32 200-step text differs from the reference.

### Ownership

A `Session` owns one `Sequence` per layer plus its activation buffers. It is
neither Clone nor Copy, is `#[must_use]`, and its only exit is
`KvCache::release(session)`, which zeroes and frees every page. A release from
the wrong cache returns the session with the error. Dropping an unreleased
session is counted by `llama::leaked_sessions()` and its pages stay leased. This
is host-library discipline enforced by Rust ownership and a counter. It is not
evidence that the Vesper source typing rules (`linear struct KvPageLease`,
[T-Let]) are implemented.

## Tests and evidence

There are 22 new integration tests, and all the existing Tensor tests still pass.

- `tests/inference_ops.rs` (11 tests) checks against independent references.
  These are hand-computed values and separately written f64 implementations.
  - RMSNorm at lengths 1, 7, 64 and 172.
  - RoPE: identity at position 0, a hand rotation, and f64 rotation at
    positions up to 511.
  - SiLU, SwiGLU, softmax (including saturation), and argmax ties and NaN.
  - The fused SwiGLU matvec is bit-identical to the unfused kernels for FP32,
    Q8_0 and Q4_0.
  - The binary16 encoder round-trips all 65,536 encodings. Every midpoint
    between adjacent finite halves rounds to even, and one ulp either side
    rounds to the nearer half.
  - BF16 ties-to-even and overflow.
  - Q8_0 and Q4_0 blocks dequantize to hand values, and the quantizers follow
    the GGML rules.
  - Quantized matvec matches an f64 reference over the dequantized weights.
  - Paged attention matches a contiguous f64 reference at 1, 15, 16, 17, 32,
    33 and 48 tokens, for both schedules and both KV formats.
  - Typed rejection of bad GQA ratios, mismatched pool shapes, short scratch,
    empty sequences, foreign pools and wrong-format appends.
- `tests/generation.rs` (10 tests):
  - Header check.
  - Exact 200-token reference text with two-pass and with online attention.
  - BF16 KV cache keeps the reference prefix. 200 of 200 tokens agree on x86_64
    Linux; the test asserts 64.
  - Q8_0 and Q4_0 weights. 114 and 27 leading tokens agree on x86_64 Linux; the
    tests assert at least 48 and 12.
  - Logits for 40 positions match an independent f64 forward pass with
    contiguous K/V (relative error below 1e-4), for FP32 and Q8_0 weights.
  - Session release, foreign caches, leak counting and the sequence-slot limit.
  - Filling all 512 positions (32 pages per layer), then a typed
    `Limit("sequence length")` error.
  - Malformed checkpoints: truncated, trailing byte, NaN weight, zero or
    over-limit dimensions, `i32::MIN` vocabulary, non-divisible GQA, odd head
    width, and a huge header that is rejected without panicking.
  - Malformed tokenizers.
- `tests/steady_state_alloc.rs` (1 test) uses a counting global allocator. For
  FP32, Q8_0 and Q4_0 weights, for both KV formats and both attention schedules,
  100 decode steps after `KvCache::begin`, crossing 6 page boundaries, perform
  0 heap allocations. Generation's own output `Vec` and `KvCache::begin` do
  allocate; the claim covers `Model::forward` only.

Measured on the cloud host (4 vCPU x86_64, release build, no SIMD): 200 FP32
tokens in about 38–40 ms with the online schedule. That is one run of the
example, not a benchmark.

Commands, all run with
`CARGO_TARGET_DIR` private, `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`:

```sh
cargo fmt --all -- --check                                        # pass
cargo clippy --workspace --locked --all-targets -- -D warnings    # pass
cargo test -p vesper-tensor --locked                              # pass, all targets
cargo test --release -p vesper-tensor --test generation           # pass
cargo test --workspace --locked                                   # see the PR for the result
cargo run --release -p vesper-tensor --example generate           # matches reference
```

## Decisions

- **Checkpoint format.** The llama2.c format is a 28-byte header plus raw
  little-endian FP32, so a small checked parser is safer and simpler than
  adding a safetensors or GGUF dependency. It needs no unsafe casts. Weights
  are copied into owned vectors; this is not the zero-copy mmap mapping from
  §28.2.
- **FP32 KV format.** The existing pool stored only opaque BF16. Inference
  needed a cache that does not round, so the FP32 format was added. BF16 is
  still supported and tested: the model rounds K/V with round-to-nearest-even
  before appending.
- **Quantization policy.** `Model::quantized` quantizes each projection whose
  input width is a multiple of 32. In this model `w2` has 172 columns, so it
  stays FP32 in all 5 layers. The embedding, the shared classifier and the
  norms also stay FP32. Activations stay FP32: weights are dequantized element
  by element, with no Q8 activation dot products as in GGML's fast path.
- **Fusion (item 3).** egglog fusion stays with the §42 optimizer owners.
  Fusion is hand-written here: `swiglu_matvec` fuses both FFN projections with
  the gate, bit-identical to the unfused path, and the online attention
  schedule fuses scoring, softmax and value accumulation per page. The
  existing RMSNorm and Q4 fusion in `host` is unchanged.
- **Order of arithmetic.** FP32 accumulation order matches `run.c`. The
  transcendental functions come from the platform's math library, so exact
  logits are not claimed to match across ISAs or operating systems. Token-level
  tests use asserted floors for that reason.

## Remaining gaps

- **GPU execution (item 4) is unavailable.** It needs DirectX 12, Vulkan or
  Metal, and there is no AMX, SME2, SVE2 or RVV lowering. No `std::arch` SIMD
  was written; every kernel is scalar.
- **Not done:** Q4_K, MXFP4 and FP8 formats; safetensors or GGUF input; mmap or
  io_uring weight streaming; BPE prompt encoding (generation starts from BOS
  or from caller-supplied token IDs); temperature and top-p sampling; batched
  prefill; multi-sequence batching in one forward call.
- **No compile-time shape typing.** The shape and GQA checks run on the host
  through `shape::gqa_ratio`, `shape::gqa_kv_head` and the parser. Nothing here
  is source-level `Tensor[r, DType, Shape]` typing, Region scratchpads or
  `eff [...]` checking.
- **No Lean proofs.** The §28.4 KV conservation and GQA index theorems were
  not part of this lane. The Rust tests check page counts after release and
  index bounds, but they are not proofs.
- **Bundled model size.** The model and tokenizer add about 1 MiB to the
  repository.
