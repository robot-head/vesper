# Wave nine lane 27: Tensor reference fixtures and differential harness (§28)

Claim: [#604](https://github.com/robot-head/vesper/issues/604). Base:
`5a018bd` on `main`, a descendant of the required `37f7b8b`. Edits are
confined to `crates/vesper-tensor/**` and this report. There is no root patch
and no new dependency.

## Scope change

This lane began as the full assignment: llama.cpp-exact Q4_K, Q8 activation
dot products, GPT-2 byte-level BPE with a `tokenizer.json` reader, per-channel,
per-block and MXFP8 FP8 scaling with AVX-512 kernels, and quantized GPU
kernels on lavapipe. On 2026-11-16 the owner approved a new plan. The Tensor
formats and tokenizer will be ported to Vesper source, so no new Rust features
should be added that would later be thrown away. This lane was therefore
narrowed to the specification side: golden vectors from independent
implementations, plus a differential harness that the Rust crate passes now
and the Vesper port must pass later. The Q4_K quantizer port was already
written and verified when the plan changed. It replaces the crate's old
approximate quantizer, so it is kept.

## What was built

- **`formats::quantize_q4_k` now reproduces llama.cpp.** It is a
  transliteration of `quantize_row_q4_K_ref` and `make_qkx2_quants`, using
  GGML's `nearest_int` bit trick, FP32 in C source order, no fused
  multiply-add, and C's `uint8_t` truncation of the 6-bit codes. The new
  public helpers are `nearest_int`, `make_qkx2_quants` and
  `quantize_q4_k_block`. On the full SmolLM-135M `blk.11.ffn_down` tensor,
  576 x 1536, all **3,456 of 3,456** super-blocks equal the bytes in
  QuantFactory's llama.cpp-quantized `SmolLM-135M.Q4_K_S.gguf`. That was
  checked locally. CI checks the first 96 rows (576 super-blocks).
- **`fixtures/reference/`** holds 2.7 MB of golden vectors with
  `MANIFEST.md`, `manifest.json` (peers, versions, readable GPT-2 cases) and
  `make_reference.py`, which regenerates the fixtures offline from the two
  SmolLM byte ranges. Contents:
  - Q4_K from llama.cpp.
  - Q8_0 and Q4_0 weights from gguf-py 0.19.0.
  - Q8_K and Q8_0 activations, and FP32 dot products from GGML's generic
    `vec_dot_{q4_K_q8_K,q8_0_q8_0,q4_0_q8_0}`.
  - FP8 E4M3/E5M2 codes and scales from ml_dtypes 0.7.0 under three rules:
    per channel, per 128x128 block, and OCP MXFP8.
  - The GPT-2 `tokenizer.json` with 41 corpus cases. Their ids come from HF
    `tokenizers` 0.23.3 and were asserted equal to `tiktoken` 0.14.0.
- **`tests/reference_fixtures.rs`** (5 tests):
  - The crate's Q4_K, Q8_0 and Q4_0 quantizers equal the peer bytes.
  - A test-side transliteration of GGML's Q8_K quantizer and the three
    generic dot kernels equals the fixture bytes and FP32 bits. The
    generator's numpy transliteration and this Rust one were written
    separately from the C source and agree bit for bit on all 1,152 dots.
  - The crate's `Fp8::encode_saturating`, applied under each scaling rule,
    reproduces ml_dtypes' codes, and the scales are bit-identical.
  - The GPT-2 case file is well formed.

## Checks

```sh
cargo fmt --all -- --check                                                    # pass
cargo +1.100.0 clippy -p vesper-tensor --locked --all-targets -- -D warnings  # pass
cargo test -p vesper-tensor --locked                                          # pass, all targets
python3 tools/bazel/sync_build_files.py                                       # no changes
```

The four required CI checks run on the PR. The existing model-format sweep
(`tests/model_formats.rs`, Q4_K cosine floor on the synthetic model) still
passes with the new quantizer.

## Decisions

- **Peers, not ourselves.** The auto-mode policy did not allow building
  llama.cpp from source in the cloud. So the Q4_K reference is a published
  llama.cpp-quantized GGUF whose F32 source is public, fetched by HTTP range
  requests, about 4 MB in total. Hugging Face was reachable this session.
  QuantFactory's file was made without an importance matrix. The exact match
  confirms that.
- **The Q8 reference is GGML's generic kernels.** GGML's AVX2 and NEON
  kernels reorder FP32 sums, so their bits differ by platform. The generic C
  code is the one platform-independent definition.
- **MXFP8 layout is left open.** OCP MX fixes the scale rule, not the byte
  layout, so scales and elements are stored separately. For an all-zero
  block the scale code is 0 (2^-127).
- **`<|endoftext|>` in text** is token 50256 in `ids` (HF default, tiktoken
  `allowed_special="all"`). `ordinary_ids` in `manifest.json` treats it as
  plain text.

## Deferred to the Vesper-source port (not built in Rust)

- A Q8-activation inference path (Q8_K/Q8_0 integer dots in `Matrix`), and
  AVX-512 integer kernels. The host has AVX-512 F/BW/VNNI. The fixtures and
  test spell out the exact arithmetic.
- The GPT-2 byte-level BPE, the regex pre-tokenizer and the `tokenizer.json`
  reader. The case file and tokenizer are the acceptance vectors.
- Per-channel and per-block FP8 scales and MXFP8 as `Matrix` weight formats.
- Quantized (Q8_0/Q4_0) GPU kernels, FlashDecoding attention, subgroup
  operations and on-device argmax on lavapipe. Under the new plan the GPU
  glue stays in Rust, but no new kernels were added in this lane.
  `crates/vesper-gpu-compute` is unchanged.
