# Port wave P1 lane 8: vesper-tensor formats, quantization and sampling

Claim [#905](https://github.com/robot-head/vesper/issues/905). Base `main` at
69c79b9 (2026-11-18). Plan: `docs/agent-prompts/port-p1/08-tensor-formats.md`.

This is the first part of the lane. It splits the crate into the seed copy
and the port-backed build, and ports the element and block formats, every
quantizer, the row decoder of `Matrix`, sampling and the FP32 kernels that
sampling and decoding use. The tokenizer, the container parsers, the K/V pool
and the shape planner are the second part (see "Not ported yet").

## Builds

`vesper-tensor` is in the compiler's dependency closure (`src/tensor_types`
evaluates source tensor types on the host kernels), so it follows the
seed-copy rule of `docs/maintenance/porting-a-crate.md`:

| Package | Path | What it is | Who links it |
|---|---|---|---|
| `vesper-tensor-seed` | `crates/vesper-tensor/seed` | the Rust build of `../src`, library `vesper_tensor`, no build script | the compiler, `vesper-kinetic`, `vesper-gpu-compute`, `vesper-fusion` |
| `vesper-tensor` | `crates/vesper-tensor` | `build.rs` links the Loom package `vesper/` and sets `cfg(vesper_port)` | its own tests only |

The plan's default would have given the port to the dependents outside the
compiler's closure (`vesper-gpu-compute`, `vesper-fusion`). The owner's parity
rule (2026-11-17) overrides it: a slower port stays non-default, so every
dependent links the seed by rename and nothing changes for them.
`tests/build_kind.rs` checks which build each package is
(`vesper_tensor::IMPLEMENTATION`).

## API inventory (re-measured before porting)

Public modules and the part of each this PR ports:

| Module | Public API | This PR |
|---|---|---|
| `formats` | `QK_K`, `Q4_K_BYTES`, `MXFP4_BLOCK`, `MXFP4_BYTES`, `E2M1_DOUBLED`, `q4_k_scale_min`, `dequantize_q4_k_block`, `check_q4_k_block`, `nearest_int`, `make_qkx2_quants`, `quantize_q4_k`, `quantize_q4_k_block`, `e8m0_half`, `check_mxfp4_block`, `dequantize_mxfp4_block`, `quantize_mxfp4`, `Fp8 {max_finite, decode, decode_exact, is_finite_code, encode_saturating}`, `quantize_fp8` | every function (constants and `Fp8::decode`'s table stay; the table is built from the ported `decode_exact`) |
| `quant` | `BLOCK`, `Q8_0_BYTES`, `Q4_0_BYTES`, `encode_fp16`, `encode_bf16`, `decode_bf16`, `WeightFormat`, `Matrix` (`f32`, `quantized`, `from_bytes`, accessors, `quantize`, `to_f32`, `dequantize_row`, `matvec`, `matvec_with`, `matmul_with`), `dequantize_q8_0_block`, `dequantize_q4_0_block`, `quantize_q8_0`, `quantize_q4_0` | the codecs, the block functions, the quantizers, the payload check of `from_bytes` and the row decoder behind `to_f32`, `dequantize_row`, `matvec*` and `matmul_with` |
| `sampling` | `Rng {next_u32, next_f32}`, `SamplerConfig`, `Sampler {new, config, rng, probabilities, sample}`, `sample_mult`, `sample_top_p` | `Rng`, `Sampler::sample`, `sample_mult`, `sample_top_p` |
| `ops` | `rms_norm`, `rope`, `rope_half`, `silu`, `swiglu`, `swiglu_matvec(_with)`, `softmax`, `argmax`, `add_assign` | `rms_norm`, `silu`, `swiglu`, `softmax`, `argmax`, `add_assign` |
| `tokenizer` | `BOS`, `EOS`, `MAX_PROMPT_BYTES`, `Encoder {new, id, encode}` | second part |
| `gguf`, `safetensors`, `json` | container readers and writers | second part |
| `kv` | `KvPool` and its types | second part |
| `shape` | `Dimensions`, `CheckedShape`, GQA helpers | second part |
| `host` | the small FP32/Q4_0 graph executor, `decode_fp16` | outside the lane's boundary (`src/host.rs`) |
| `attention`, `llama` | paged GQA attention, model loading and inference | second part (pure tensor parts), host glue stays |
| `simd`, `mmap` | `std::arch` kernels, file mapping | stay Rust (F11 #587; memory mapping) |

Unit tests in the crate: `json.rs` (1) and `kv.rs` (3). Neither module is
ported yet, so both run unchanged in both packages.

## What was ported

The Loom package `@vesper/tensor` (`crates/vesper-tensor/vesper`, 3,070
lines, 41 exported functions; part 1 had 1,509 lines and 36 exports):

| File | Rust source | Contents |
|---|---|---|
| `lib.ves` | all | the exports, `Fault`, the `Libm` import, and the kernels that call `exp` (softmax, SwiGLU, `Sampler::sample`) |
| `num.ves` | `quant.rs`, `host.rs` | binary16 and BF16 encode/decode, float predicates |
| `ocp.ves` | `formats.rs` | FP8 E4M3FN/E5M2 decode and saturating encode, E8M0, GGML `nearest_int` |
| `qk.ves` | `formats.rs` | Q4_K: scale/min unpacking, dequantization, `make_qkx2_quants`, `quantize_row_q4_K_ref` |
| `rows.ves` | `quant.rs`, `formats.rs` | Q8_0, Q4_0, MXFP4 blocks and quantizers, FP8 matrices, the row decoder, the payload check |
| `sampling.ves` | `sampling.rs`, `ops.rs` | xorshift64*, `sample_mult`, `sample_top_p` (heapsort of packed candidates), argmax, RMSNorm, residual add |

The Rust glue is `src/port.rs`. Each ported Rust function keeps its signature
and has a `#[cfg(vesper_port)]` twin that forwards to the binding
(`port/binding.rs`); the Rust body stays under `#[cfg(not(vesper_port))]` for
the seed.

Decisions:

- **f32 at the boundary.** The export ABI carries `f64` but not `f32`. An
  `f32` crosses as its bit pattern: a `u32` scalar, and `&[f32]` lent as
  `&[u32]` with `bytemuck::cast_slice` (no copy; `u32` slices are lent in
  place). The Vesper side converts with `from_bits_f32`/`to_bits_f32`.
- **Bit-exactness.** Every operation is FP32 in the Rust order, without fused
  multiply-add, so every result equals the Rust build bit for bit, and
  `tests/reference_fixtures.rs` (llama.cpp Q4_K bytes, gguf-py Q8_0/Q4_0,
  ml_dtypes FP8 and MXFP8 codes) passes against the port. No ported path
  uses `sin` or `cos`, so the #893 accuracy gap does not apply.
- **`exp` stays the platform's.** The Rust build's softmax and SiLU call
  `f32::exp` and document no cross-ISA claim. Vesper's `exp_f32` is fdlibm in
  f64 rounded once, which can differ in the last bit and change a sampled
  token. The port imports the Rust `f32::exp` through a `.vsys` library
  (`Libm`, shim in `src/port.rs`), so probabilities stay identical. Moving to
  Vesper's `exp_f32` is a one-line change once the crate fixes its own
  `exp`.
- **Errors.** Vesper raises `Fault {Shape, NonFinite, Limit}`; the glue maps
  each to the `Error` the Rust build returns at that call, with the same
  message. `ArithOverflow` and `RuntimeError` cannot occur on valid calls;
  where the Rust function returns `Result`, they map to `Error::Overflow` and
  `Error::Malformed`, and where it panics on bad input (short slices), the
  port panics too.
- **Row pieces.** The port decodes a row in pieces of 256 values (eight
  Q8_0/Q4_0/MXFP4 blocks, one Q4_K block) per call instead of one block, to
  cut boundary crossings. The crate's contract for `row_chunks` is that every
  piece but the last is a multiple of eight, which keeps chunked SIMD sums
  equal to whole-row sums, and the batching and SIMD tests pass.
- **Top-p candidates.** The Rust build sorts `(f32, u32)` pairs. The port
  packs each candidate as `(!bits(p) << 32) | id`: probabilities are
  non-negative, so ascending words are descending probabilities with ties
  by ascending id, the order the Rust build uses. `Sampler` keeps a
  preallocated `Vec<u64>` in the port build, so `Sampler::sample` still
  allocates nothing (`tests/steady_state_alloc*.rs` pass). The standalone
  `sample_top_p` keeps its `&mut Vec<(f32, u32)>` parameter and borrows a
  word buffer per call.

### Part 2: prompt encoding and the model file readers

| File | Rust source | Contents |
|---|---|---|
| `tokens.ves` | `tokenizer.rs` | the llama2.c BPE encoder: UTF-8 units, byte fallback, and the merge heap with lazy invalidation, keyed by (score in `total_cmp` order, leftmost first) as in the Rust build; a 64-bit FNV-1a open-addressing table over the flattened vocabulary replaces the `HashMap` (lowest id on duplicates) |
| `text.ves` | `json.rs` | the strict safetensors-header JSON reader: grammar, escapes, surrogate pairs, UTF-8 validation (`String::from_utf8` rules), integer limits, nesting and duplicate keys (a hash table per document, keyed by object) |
| `store.ves` | `gguf.rs` | the GGUF v2/v3 reader: every count, length, string, bool, array, duplicate key and tensor name, `general.alignment`, tensor geometry, alignment, bounds and overlap check |

The readers do not build Rust values. Each checks the whole input and
writes a tape of words, and the Rust wrapper turns the tape into the same
`Value`, `TensorInfo` and metadata types (`json::from_tape`,
`gguf::from_tape`). GGUF arrays are checked in place and decoded by the
wrapper from the file bytes, so the tape stays a few words per entry. The
tape and the hash tables are sized from the input and grow when the port
asks (a status code), and loading the stories260K model still allocates less
than the Rust build (58,881 bytes against 66,672 in
`steady_state_alloc_batch`). Failures come back as a code and up to two
details, which the wrapper maps to the Rust build's error and message.
`safetensors.rs` itself stays Rust: past the JSON it only interprets the
`Value` tree.

## What stays in Rust

| File | Reason | Unblocked by |
|---|---|---|
| `src/simd.rs` | `std::arch` kernels | source `VVec` (F11, #587) |
| `src/mmap.rs` | the one `memmap2::Mmap::map` call | a `.vsys` mapping surface (second part) |
| `src/host.rs` | outside the lane's exclusive boundary; `host::decode_fp16` stays Rust, the port has its own copy | the lane that owns `host.rs` |
| `ops::rope`, `ops::rope_half` | `powf`, `sin`, `cos` from the platform library; Vesper's `sin`/`cos` lack Payne-Hanek reduction (#893) and would change bits | a `.vsys` import like `Libm`, or Vesper transcendentals the Rust build adopts |
| `kv`, `shape`, `attention`, `llama` | not ported yet | a third part of this lane |
| `safetensors.rs` past the JSON reader | interprets the parsed `Value` tree only | nothing; it could move with the tree builder |
| `Value` tree building in `json.rs`, `gguf.rs` | the export ABI cannot return a recursive Rust enum; the port returns a checked tape | richer export types |
| `seed/` | the compiler's copy | self-hosting Stage 2 |

The crate's `unsafe` count is unchanged in the Rust build. The port-backed
build adds the generated binding (`port/binding.rs`, `unsafe` FFI calls) and
one `#[unsafe(no_mangle)]` shim function; the Vesper source has none.

## Unit tests and test changes

- No ported module has `#[cfg(test)]` tests. The four in `json.rs` and
  `kv.rs` run in both packages.
- `tests/model_formats.rs`, `tests/lean_kv.rs` and
  `tests/steady_state_alloc_batch.rs` found their files through
  `CARGO_MANIFEST_DIR`; they now step out of `seed/` when the seed runs them.
- New: `tests/build_kind.rs` (which build each package is) and
  `tests/abba.rs` (the benchmarks, and a non-ignored check that both builds
  produce the same bytes and values).
- Part 2 adds three differential tests to `tests/abba.rs`: the same 4 KiB
  prompt encoded by both builds; 4,000 safetensors headers (mutations of a
  valid header plus random token soup, UTF-8 edge cases, the nesting limit
  and a 5,000-tensor header that makes the port grow its tape) whose results
  and errors must match; and about 6,000 GGUF files (the two fixtures, two
  written files with every metadata type, every header truncation and 6,000
  single-byte edits) whose results and errors must match, reaching at least
  12 distinct errors. `tokenizer_sampling` already compared the encoder with
  a literal `run.c` transliteration.

All 100 tests pass against both packages (`cargo test -p vesper-tensor -p
vesper-tensor-seed`, Linux x86_64). After part 2, the port-backed package passes
94 tests (8 ignored benchmarks) and the seed 90 (it does not build
`tests/abba.rs`), Linux x86_64; `port/test-port.sh` passes with the eight
divergent edits.

## Divergence checks

`build.rs` `DIVERGENT` (feature `vesper-port-divergent`) makes one edit per
ported Rust module; `port/test-port.sh` (run by
`scripts/test-port-harness.sh` on macOS and Windows) requires each named test
to fail and an unchanged test of the same file to pass:

| Ported module | Edit | Fails | Still passes |
|---|---|---|---|
| `formats.rs` (Q4_K, `qk.ves`) | wrong high bits of a packed minimum | `q4_k_bytes_equal_llama_cpp` | `gpt2_case_file_is_well_formed` |
| `formats.rs` (FP8, `ocp.ves`) | normal codes get the next exponent | `fp8_channel_block_and_mxfp8_codes_equal_ml_dtypes` | same |
| `quant.rs` (`rows.ves`) | Q4_0 rounds with 8.25 | `q8_0_and_q4_0_weights_equal_gguf_py` | same |
| `sampling.rs` (`sampling.ves`) | the crossing token leaves the nucleus | `sampling_follows_the_distribution_and_the_nucleus` | `rng_matches_run_c_xorshift` |
| `ops.rs` (`lib.ves`) | softmax divides by twice the sum | `silu_swiglu_softmax_argmax` | `rope_matches_f64_rotation_and_rejects_odd_widths` |
| `tokenizer.rs` (`tokens.ves`) | byte-fallback pieces looked up as lowercase `<0xab>` | `encoder_matches_run_c_and_round_trips` | `rng_matches_run_c_xorshift` |
| `json.rs` (`text.ves`) | a leading zero is accepted before two more digits | `both_builds_read_the_same_safetensors_headers` | `both_builds_encode_the_same_prompt` |
| `gguf.rs` (`store.ves`) | the default alignment is 16 | `both_builds_read_the_same_gguf_files` | same |

A first tokenizer edit (equal-score merges take the rightmost pair) passed
every test: `tok512` never offers two mergeable pairs with the same score, so
the tie rule has no test. The lowercase edit replaced it.

Checked locally: the divergent build fails exactly these and passes the
controls. Bazel (Linux) builds the port and runs every test against both
packages, but does not build a divergent object yet: the port-backed crate
links its object through its build script, so a divergent Bazel build needs a
second crate instance. The divergence check runs on macOS and Windows CI.

## Benchmark

`tests/abba.rs`, `vesper-bench` `run_abba`, 20 epochs x 4 operations per
arm, release build, one process holding both builds (`seed` is a
dev-dependency). Cloud container, Intel Xeon @ 2.10 GHz, 4 vCPU, rustc 1.97,
port compiled by the in-tree `vesper` at 69c79b9 (Cranelift, inliner on).

| Workload | Rust median | Vesper median | Vesper/Rust median | 95% interval |
|---|---|---|---|---|
| dequantize Q4_K 64x4096 (`to_f32`) | 262.0 us | 1024.8 us | 3.66x | [3.62, 3.73] |
| dequantize Q8_0 64x4096 (`to_f32`) | 256.2 us | 1026.1 us | 4.00x | [3.98, 4.04] |
| matvec Q8_0 64x4096, scalar kernel | 193.8 us | 1146.1 us | 5.85x | [5.50, 6.10] |
| quantize Q4_K 16x4096 | 5493.5 us | 22665.7 us | 4.13x | [4.09, 4.23] |
| sample, T 0.8, top-p 0.9, vocab 32,000 | 392.5 us | 1748.4 us | 4.67x | [4.46, 4.78] |
| BPE encode, 16 KiB prompt, `tok512` (part 2) | 1177.3 us | 3236.0 us | 2.75x | [2.69, 2.77] |
| safetensors header, 5,000 tensors (part 2) | 4738.0 us | 11683.5 us | 2.47x | [2.43, 2.51] |
| GGUF header, stories260K (part 2) | 65.5 us | 106.6 us | 1.63x | [1.57, 1.66] |

The part 2 rows were measured on the same machine with the compiler at
153f264. The safetensors and GGUF rows time the whole `from_vec`, so the
Rust work both builds share (copying the file, building the values, and for
safetensors the tensor table) dilutes the ratio; the GGUF reader is the only
port inside the 2x rule.

Every workload is past the plan's 2x stop rule. Under the owner's parity rule
the port stays non-default and the Rust code stays. Where the time goes, from
the source (not profiled yet): every element read and write is a checked
`s[i]`/`set` with a failure status, every float goes through
`from_bits_f32`/`to_bits_f32`, the decode loops are scalar where the Rust
build auto-vectorizes, and `matvec` adds a boundary crossing per 256 values
and a decoded buffer. BPE encoding is not ported yet, so it has no
benchmark in this part.

## Compiler and tooling gaps found

1. **`.vsys` imports in packages depend on file order.** `build --lib` on a
   Loom package resolves `// @export vsys` libraries only if `lib.ves` sorts
   first among the package's `src/*.ves` files; with a module named `half.ves`
   it fails with `unknown effect 'Ffi'`. The modules here are named to sort
   after `lib`. A `.vsys` library is also visible only in the root module.
2. **No `f32` in the export ABI** (scalars, slices, results).
3. A literal range `for i in 0..4` is `i64`; the port starts ranges at a
   typed `u64_zero()`.
4. `Fail::raise` in a row with several `Fail` types raises the last one, so
   `Fault` is last in every row.
5. `w.set(i, w[j])` on an `inout` slice fails E0410 (the read overlaps the
   active loan of `w`); the port reads into a local first. Rust allows the
   same statement for `Copy` elements.
6. Exported fault enums carry no payload, so a failure that needs details
   (the GGUF version, a tensor type, a block alignment) writes them to an
   `inout` status slice before raising.
7. The export ABI has no recursive or owned result types, so the readers
   return a tape of words that Rust turns into values.

## Platforms run

Locally: Linux x86_64 (both packages, the divergence check, clippy 1.100,
`vesper fmt --check`). CI: the four required checks on the PR.

## Was the Rust code deleted?

No. The port is 1.6x to 5.9x slower than the Rust build, and the compiler
links the seed until Stage 2.

## Not ported yet (third part of the lane)

`kv.rs` (the paged K/V pool's bookkeeping; the storage and the borrowed
views stay Rust), `shape.rs` (the symbolic dimension algebra, which needs
symbols interned), the pure tensor parts of `attention.rs` and `llama.rs`,
and the memory map as a `.vsys` surface. The crate has no `tokenizer.json`
reader, so part 1's list naming one was wrong.
