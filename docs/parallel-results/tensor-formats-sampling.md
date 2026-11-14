# Wave seven lane 20: Tensor formats, containers, BPE, sampling, batching and SIMD (§28)

Claim: [#343](https://github.com/robot-head/vesper/issues/343). Base:
`283c8115` on `main` (a descendant of the required `c025b9a`). Edits are
confined to `crates/vesper-tensor/**`, the new `lean/TensorKv.lean` and this
report. The only root change is the regenerated `Cargo.lock`, in its own
commit. The crate now depends directly on `memmap2 = "=0.2.3"`, which was
already in the lockfile through `cranelift-jit`. No new crate was downloaded.

## What was built

| Module | Contents |
| --- | --- |
| `formats` (new) | Q4_K (GGML `block_q4_K`, 144 bytes per 256 weights, 6-bit packed sub-block scales and mins), MXFP4 (GGML `block_mxfp4`, E8M0 exponent and 16 bytes of E2M1 nibbles per 32 weights), and OCP FP8 E4M3FN and E5M2 with a decode table and a saturating round-to-nearest-even encoder. Block validators reject non-finite scales and the reserved E8M0 `0xFF`. |
| `quant` (extended) | `WeightFormat` covers F32, F16, BF16, Q8_0, Q4_0, Q4_K, MXFP4, F8E4M3 and F8E5M2. `Matrix` stores either owned FP32 or raw block bytes (`WeightBytes`), owned or borrowed from a file mapping. `Matrix::from_bytes` validates length, block alignment and finiteness. `matvec_with(kernel, …)` and `matmul_with(kernel, …)` run every format. `matmul_with` decodes each weight block once per batch and is bit-identical to `matvec_with`. |
| `mmap` (new) | `Mapping::open` (a read-only `memmap2` mapping, 64 GiB cap) and `Mapping::read` (the same bytes in memory). `WeightBytes::{Owned, Mapped}` gives zero-copy matrix storage. |
| `gguf` (new) | A GGUF v2/v3 reader: metadata of every value type, tensor infos, alignment, and bounded counts and offsets. It rejects duplicate keys or tensors, overlaps, misalignment and truncation. A v3 writer is included for tests and conversion. |
| `safetensors` (new) | A safetensors reader with a strict private JSON parser: no duplicate keys, depth cap, tensors must tile the buffer exactly, and offsets and shapes are checked. Dtypes are F32, F16, BF16, F8_E4M3 and F8_E5M2. An FP8 scale is read from `<name>_scale`. A writer is included. |
| `tokenizer` (new) | `Encoder`, llama2.c `run.c` BPE: dummy-prefix space, UTF-8 codepoint lookup with byte fallback (`<0xXX>` as id + 3), and merges of the highest-scoring adjacent pair, leftmost on ties. A heap with lazy invalidation reproduces `run.c`'s choices in O(n log n). Prompts are capped at 1 MiB. |
| `sampling` (new) | `Sampler` with `run.c` semantics: xorshift64* `Rng`, temperature with softmax, `sample_mult`, and `sample_top_p` (cutoff `(1 - p)/(n - 1)`, ties by ascending id). Buffers are preallocated. |
| `simd` (new) | `Kernel::{Scalar, Simd}`. `Simd` is an eight-lane dot product with separate multiply and add (no FMA), a sequential tail, and a fixed reduction order. There are AVX2 (runtime-detected) and NEON implementations plus the portable `lanes_reference_dot`, and all three give the same bits. `DotAcc` accumulates in chunks. `dot_acc_bytes` and `axpy_bytes` read little-endian FP32 or BF16 K/V bytes directly. AXPY and scale round once per element, so they are bit-identical to the scalar loops. |
| `attention` (extended) | `paged_gqa_attention_with(…, kernel, visible)` uses the SIMD dot and AXPY over the KV pages. `visible` limits a query to the first `n` positions, which gives causal batched prefill. The old entry point is unchanged. |
| `llama` (extended) | `RunOptions { attention, kernel }`, `Model::forward_with`, and `Model::forward_batch` with `BatchItem`/`BatchScratch`. One call runs several sessions, each with one or more tokens (batched prefill and multi-sequence decode, up to 4096 tokens). Every owner, token, length, capacity and free-page check runs before any KV write. Also `Model::from_gguf`/`to_gguf` (llama.cpp tensor names, interleaved RoPE), `Model::from_safetensors`/`to_safetensors` (Hugging Face Llama names, `rotate_half` RoPE with Q/K rows permuted), `HfConfig`, `Tokenizer::from_gguf`, `Tokenizer::encoder`, `generate` (prompt + sampler, batched prefill), `greedy_sampler`, and `Model::quantized` for every format. |

### Ownership

`Session` is still the linear owner of its pages: non-Clone, non-Copy,
`#[must_use]`, its only exit is `KvCache::release`, and drops are counted by
`leaked_sessions()`. `forward_batch` borrows sessions through `BatchItem`; it
neither takes nor releases them. This is host-library discipline enforced by
Rust ownership. It is not evidence that the Vesper source typing rules for
`KvPageLease` are implemented.

## Tests and evidence

All Tensor tests from before this lane still pass. The lane adds 32
integration tests in 8 files plus JSON unit tests.

- `tests/formats_interop.rs` (7). The peers are gguf-py 0.19.0 (llama.cpp's
  Python package), safetensors 0.8.0, numpy 2.5.4 and ml_dtypes 0.6.0.
  `fixtures/make_interop.py` regenerates the fixtures offline. Results, all
  bit for bit:
  - Our dequantization of F32, F16, BF16, Q8_0, Q4_0, MXFP4 and Q4_K equals
    gguf-py's.
  - Our Q8_0, Q4_0, MXFP4 and BF16 quantizers produce gguf-py's bytes.
  - All 256 FP8 codes of both kinds decode as ml_dtypes does, and our RNE
    encoder matches ml_dtypes on the sample values.
  - The stories260K model that gguf-py converted to Q8_0 GGUF equals our own
    Q8_0 conversion and is mapped without copying.
- `tests/containers.rs` (4): malformed GGUF and safetensors give typed errors
  and never panic. Covered: every truncation, bad magic and version,
  duplicates, overlaps, misalignment, absurd counts, bad JSON and bad dtypes.
  Writer round trips are also checked.
- `tests/model_formats.rs` (5):
  - An F32 GGUF round trip reproduces the 200-token reference text, and the
    model compares equal.
  - An HF safetensors round trip (permuted Q/K, half RoPE) reproduces the
    reference text.
  - Mapped and read loads give identical logits.
  - Missing or extra tensors and bad metadata give typed errors.
  - A per-format sweep and a logits cosine check (figures below).
- `tests/batching.rs` (4):
  - Batched prefill, mixed batches of several sequences, and uneven prompt
    lengths give logits bit-identical to one-token `forward` steps, for both
    attention schedules, both KV formats and both kernels.
  - SIMD and batched greedy decoding reproduce the llama2.c 200-token
    reference text.
  - A rejected batch (foreign session, bad token, over capacity, out of
    pages) leaves every KV pool unchanged.
- `tests/simd_kernels.rs` (3):
  - The SIMD dot product equals `lanes_reference_dot` bit for bit at lengths
    0 to 300. The scalar kernel equals the sequential loop.
  - Chunked `DotAcc` equals one call, and the byte (FP32/BF16) variants equal
    the FP32 ones.
  - Every format's SIMD and scalar matvec and matmul equal their references
    over the dequantized rows.
  - SIMD paged attention is within 1e-5 relative of scalar, and `visible = n`
    on a longer sequence is bit-identical to an n-token sequence.
- `tests/tokenizer_sampling.rs` (5):
  - The encoder equals a direct O(n²) transliteration of `run.c` `encode` on
    generated ASCII and UTF-8 prompts, including byte fallback.
  - With `VESPER_LLAMA2C_TOKENIZER` pointing at llama2.c's 32 000-token
    `tokenizer.bin`, the four prompt vectors of llama2.c's `test.c` encode
    exactly (run here; skipped in CI).
  - The sampler matches `run.c` arithmetic: the xorshift sequence, the
    `sample_mult` and `sample_topp` boundaries, and temperature 0 as argmax.
  - Invalid configurations give typed errors.
- `tests/lean_kv.rs` (3):
  - Hygiene: no `sorry`, `admit` or `axiom`.
  - With `VESPER_LEAN` set, `lean lean/TensorKv.lean` compiles. The 11
    `#print axioms` lines may name only `propext`, `Quot.sound` and
    `Classical.choice`; here they name only `propext` and `Quot.sound`.
  - A differential test runs 200 rounds of 300 random begin, append,
    rollback and release operations against both the Rust `KvPool` and a
    transliteration of the Lean `step`. Results, free pages and page counts
    agree.
- `tests/steady_state_alloc_batch.rs` (1) uses a counting global allocator.
  After warm-up it counts 0 allocations for SIMD decode, top-p sampling, and
  batched prefill and decode through `forward_batch`. It also measures that a
  mapped GGUF load allocates 66,672 bytes of headers and tables while 664,544
  bytes of weights stay mapped.

### Format accuracy (stories260K, x86_64 Linux)

Leading greedy tokens that agree with the FP32 reference, out of 200 (release
build):

| F16 | BF16 | Q8_0 | Q4_0 | MXFP4 | F8E4M3 | F8E5M2 |
| --- | --- | --- | --- | --- | --- | --- |
| 200 | 200 | 114 | 27 | 20 | 71 | 40 |

Q4_K leaves stories260K unchanged: none of its matrices has a width that is a
multiple of 256, so Q4_K is tested on the synthetic model instead. On the
synthetic dim-256 model, last-position logits have these cosine similarities
to FP32: F16 1.000000, BF16 0.999998, Q8_0 0.999989, E4M3 0.999664, E5M2
0.997969, Q4_K 0.997827, Q4_0 0.997474, MXFP4 0.994296. The tests assert
floors below these values.

### Measured throughput

Command: `cargo run --release -p vesper-tensor --example throughput`. Host:
cloud x86_64 Linux VM, 4 vCPU (Intel Xeon, 2.10 GHz), AVX2 detected,
single-threaded, release profile, best of 3 runs per cell. "Synthetic-512" is
a random dim-512, 8-layer, hidden-1536 model with 8 query heads over 4 KV
heads and a 4096-token vocabulary; it is generated in memory, so no output
text is checked. The VM is shared and these numbers moved by up to about 2x
between sessions, so read them as relative.

| Model | Kernel | Decode | Prefill 128, one by one | Prefill 128, batched | 8 sequences, batched |
| --- | --- | ---: | ---: | ---: | ---: |
| stories260K F32 | Scalar | 4808 | 5398 | 5930 | 6439 |
| stories260K F32 | SIMD | 4364 | 5595 | 6801 | 7459 |
| synthetic-512 F32 | Scalar | 29.6 | 35.4 | 75.7 | 66.5 |
| synthetic-512 F32 | SIMD | 203.5 | 230.3 | 479.5 | 269.8 |
| synthetic-512 Q8_0 | Scalar | 60.3 | 47.7 | 95.2 | 112.1 |
| synthetic-512 Q8_0 | SIMD | 96.7 | 77.1 | 173.8 | 179.1 |
| synthetic-512 Q4_K | Scalar | 49.8 | 41.8 | 78.1 | 68.9 |
| synthetic-512 Q4_K | SIMD | 97.9 | 111.0 | 453.8 | 286.9 |
| synthetic-512 MXFP4 | Scalar | 53.3 | 49.3 | 91.9 | 96.6 |
| synthetic-512 MXFP4 | SIMD | 75.4 | 65.2 | 193.8 | 147.3 |
| synthetic-512 F8E4M3 | Scalar | 33.6 | 30.0 | 69.1 | 60.8 |
| synthetic-512 F8E4M3 | SIMD | 55.0 | 53.7 | 389.2 | 207.6 |

All figures are tokens per second.

- stories260K (dim 64) is too small for SIMD to help with decode.
- On the dim-512 model, SIMD decode is several times faster than scalar.
- Batched prefill reuses each decoded weight block across the batch, which
  helps the block formats most.

### Commands and results

The private target directory was used throughout.
`VESPER_LEAN=…/lean-4.24.0-linux/bin/lean` (the GitHub release .zip) and
`VESPER_LLAMA2C_TOKENIZER=…/llama2.c/tokenizer.bin` were set for the crate
test runs.

```sh
cargo fmt --all -- --check                                                    # pass
cargo +1.100.0 clippy -p vesper-tensor --locked --all-targets -- -D warnings  # pass (x86_64; NEON path is checked by macOS CI)
cargo clippy -p vesper-tensor --locked --all-targets -- -D warnings           # pass (1.97)
cargo test -p vesper-tensor --locked                                          # pass, all targets (~30 s)
cargo test -p vesper-tensor --locked --release                                # pass, all targets
cargo run -p vesper-tensor --release --example throughput                     # table above
cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings       # see PR / CI
cargo test --workspace --locked                                               # see PR / CI
```

## Decisions

- **Hand-written container parsers.** GGUF and safetensors are simple
  length-prefixed formats. Small checked parsers keep "existing dependencies
  only" and make every malformed input a typed error. The safetensors header
  uses a private strict JSON parser instead of pulling serde_json into the
  crate. Writers exist so tests and conversion do not depend on Python.
- **Interop is checked against independent writers, not ourselves.** Every
  fixture byte was produced by gguf-py, safetensors or ml_dtypes. Our writers
  are only used for round trips.
- **mmap with `memmap2` 0.2.3.** It was already in the lockfile, so no
  download was needed. Mapped weights are read only through bounds-checked
  slices and `from_le_bytes`, never by casting pointers. `Mapping::read`
  gives the same parsing without the mapping's safety contract.
- **SIMD numerics.** The SIMD kernel does not try to match the scalar kernel
  bit for bit, since that would rule out vector reassociation. It is defined
  by the portable eight-lane reference instead, and AVX2 and NEON match that
  reference bit for bit. Not using FMA keeps the two ISAs identical. Greedy
  decoding with the SIMD kernel still reproduces the reference text exactly,
  which meets the token-for-token acceptance criterion. Attention
  normalization keeps `/ sum` rather than multiplying by `1 / sum`, so the
  scalar outputs did not change.
- **Batched prefill is causal attention via `visible`.** Each token in a
  batch attends to the KV positions up to its own, with the same arithmetic
  as a single step. That is why batched logits are bit-identical and not
  just close.
- **Prechecks before any write.** `forward_batch` validates the whole batch,
  including free pages for every page boundary it will cross, before
  appending K/V. A rejected batch therefore needs no rollback.
- **Quantization policy.** `Model::quantized(format)` quantizes a projection
  only when its width is a whole number of blocks: 32 for Q8_0, Q4_0 and
  MXFP4, 256 for Q4_K, and any width for F16, BF16 and FP8. The function
  reports how many matrices it quantized. Norms stay FP32. FP8 uses one
  per-tensor scale (`amax / max_finite`).
- **RoPE layouts.** GGUF and llama2.c use interleaved pairs. HF safetensors
  uses `rotate_half` (`RopeStyle::Half`). `to_safetensors` applies the HF
  conversion permutation to Q/K rows, so both layouts produce the same text.
- **BOS is never merged.** As in `run.c`, BOS and EOS are added around the
  merged pieces, not merged into them.

## Remaining gaps

- **GPU execution stays deferred**, as the assignment says. There are no
  AMX, SME2, SVE2, AVX-512 or RVV kernels: the SIMD paths are AVX2 and NEON
  only, and other targets fall back to the portable lane reference.
- **No io_uring weight streaming.** `io_uring_setup` returns EPERM on the
  cloud host. Weights are mapped or read whole.
- **Q4_K quantizer.** It uses a simple min/max quantizer per sub-block, not
  GGML's iterative `make_qkx2_quants`. Q4_K *decoding* matches gguf-py bit
  for bit, but our Q4_K bytes are not llama.cpp's bytes for the same input.
  GGUF files quantized by llama.cpp still load and run correctly.
- **FP8 scaling is per tensor only.** There are no per-channel or per-block
  scales, and no MX block-scaled FP8 (MXFP8).
- **Activations stay FP32.** There are no Q8 activation dot products as in
  GGML's fast path. Weights are decoded to FP32 per block.
- **Q4_K end to end.** stories260K has no 256-wide matrices, so Q4_K
  inference is tested only on the synthetic model. No real Q4_K checkpoint is
  bundled.
- **Tokenizer coverage.** Only the llama2.c/SentencePiece-style BPE is
  supported, from `tokenizer.bin` or GGUF `tokenizer.ggml.*` arrays. A GGUF
  whose `tokenizer.ggml.model` is not `llama` (for example GPT-2 byte-level
  BPE) is rejected with a typed error. There is no regex pre-tokenizer and no
  `tokenizer.json` reader.
- **The mmap safety contract cannot be enforced.** If another process
  truncates or rewrites a mapped weight file, the mapping can fault. Use
  `Mapping::read` for files the process does not control.
- **The Lean results are about the model only.** `lean/TensorKv.lean` proves
  page conservation, the one-page-per-16-tokens invariant, release and
  streaming-loop conservation, and the GQA head, offset and page index bounds
  for its own model. The differential test compares the Rust pool with that
  model on random operation sequences, which is evidence on those inputs
  only. It is not a theorem about the Rust code.
- **No source-level integration.** There is still no compile-time
  `Tensor[r, DType, Shape]` typing, no Region scratchpads and no `eff [...]`
  checking. All checks run on the host.
