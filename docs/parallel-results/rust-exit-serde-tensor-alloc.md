# Rust exit SH6 lane 11: Serde, Tensor and Alloc

Claim [#1406](https://github.com/robot-head/vesper/issues/1406), continuing
[#905](https://github.com/robot-head/vesper/issues/905) (P1 lane 8, Tensor),
the serde scalar port (#916, #982) and the Strata alloc port (#903). Branch
`claude/project-thread-633g8y`, base `ab965f2a` (main, 2026-11-21), merged
with main at `c6e8bae0` before the final measurements. Edits are inside
`crates/vesper-tensor/**`, `crates/vesper-serde/**` and this report. No root
patch, no new crate or dependency.

This is part 1. All three crates are compiler-linked, so every flip uses the
seed pattern (#1102): the Vesper port becomes the only implementation in the
package that crates outside the compiler use, and the frozen Rust moves into
the crate's `seed/` (deleted at the retirement, SH6 lane 2).

## What changed

**Tensor: a seed crate root.** `crates/vesper-tensor/seed/src/lib.rs` includes
`../../src/lib.rs` as a module and adds `seed/src/engine/`, the Rust
implementations that `src/` no longer has. `src/` keeps the shared API: each
flipped function forwards to `crate::port`-backed code under
`cfg(vesper_port)` and to `crate::engine::…` otherwise (no `src/` file names
a seed file). The seed's `BUILD.bazel` takes `glob(["src/**/*.rs"])` plus the
parent's `:rust_srcs`, with `crate_root = "src/lib.rs"`. `mod json` became
`pub(crate)` and a few fields `pub(crate)` so the engine can build the API
types.

**Tensor: four modules flipped** (Rust moved to the seed, 798 lines out of
`src/`, 811 in `seed/src/engine/`):

| Module | Port | Rust moved to | Speed work |
|---|---|---|---|
| GGUF header reader (`Gguf::parse`) | `store.ves` | `engine/gguf.rs` | none needed |
| BPE prompt encoder (`tokenizer::Encoder`) | `tokens.ves` | `engine/tokenizer.rs` | hole-based heap sifts, wrapping node offsets, a per-call cache of one-byte pieces, a per-thread work buffer in the adapter |
| llama2.c readers (`Config::validate`, `Model::from_llama2c`, `Tokenizer::from_llama2c`) | `llama.ves` | `engine/llama.rs` | finiteness tested two floats per 64-bit read (SWAR on the exponent fields) |
| safetensors JSON header reader (`json::parse`) | `text.ves` | `engine/json.rs` | strings copied eight bytes at a time up to the first quote, backslash or control byte; the UTF-8 check only for strings with a raw byte from 0x80; duplicate-key hashing a word at a time; per-thread tape and string buffers in the adapter |

**Tensor: faster, not yet at the bar.** The K/V pool's free-page search now
stops at the first free page (the Rust build's `position`), and lease checks
bound the page index once. `decode_piece` answers ordinary pieces first.

**Serde: faster JSON encoder, not yet at the bar.** `encode.ves` skips eight
plain bytes per step in the preflight and writes them as one word, copies
short plain tails and raw nodes as blocks, writes `null`/`true`/`false` as
words and compares duplicate keys with `eq`. A new `encode_doc` workload in
`examples/serde_abba.rs` (2,000 objects with escaped and non-ASCII strings,
integers, floats, booleans, null and arrays) measures the whole encoder; the
existing `f64_encode` row covers floats only. The driver now takes workload
names to run a subset.

**Alloc:** measured only (below).

## Flip-gate verdicts

`scripts/ci/flip_gate.py` (SH6 lane 1) has not merged, so these are the
recipe of `docs/maintenance/rust-exit-plan.md` run by hand. Differential:
every test of the crate passes against both packages (`cargo test -p
vesper-tensor -p vesper-tensor-seed`; 0 failures), including the
cross-build differentials in `tests/abba.rs`, and `port/test-port.sh`'s
divergent builds each fail their guard test. Speed: `tests/abba.rs`
(`vesper-bench` `run_abba`, 20 epochs, one process, release), cloud
container, 4 vCPU Xeon, shared and noisy, compiler at the merge with main.

| Tensor workload | Before (main `ab965f2`) | After | Verdict |
|---|---|---|---|
| GGUF header, stories260K | 1.21x [1.11, 1.23] | 1.27x [1.23, 1.33] | **flipped** |
| BPE encode, 16 KiB prompt, tok512 | 1.78x [1.73, 1.81] | 1.19x [1.13, 1.25] | **flipped** |
| llama2.c load, stories260K + tok512 | 1.87x [1.83, 1.90] | 1.13x [1.10, 1.16] | **flipped** |
| safetensors header, 5,000 tensors | 1.87x [1.82, 1.91] | 1.43x [1.39, 1.46] | **flipped** |
| K/V pool, 1,024 FP32 appends and reads | 1.85x [1.80, 1.89] | 1.50-1.70x across runs (best [1.48, 1.52]) | above the bar |
| llama2.c decode, 4,096 tokens (new) | - | 1.69x [1.62, 1.76] | above the bar |
| dequantize Q8_0 64x4096 | 2.11x | 2.14x [2.09, 2.19] | above the bar |
| dequantize Q4_K 64x4096 | 2.09x | 2.68x [2.60, 2.76] | above the bar |
| matvec Q8_0 64x4096, scalar | 2.71x | 2.69x [2.65, 2.73] | above the bar |
| attention, two-pass / online | 3.5x / 3.3x | 3.3x / 3.3x | above the bar |
| shape plan, 64 layers | 3.45x | 3.37x [3.28, 3.46] | above the bar |
| quantize Q4_K 16x4096 | 4.12x | 4.0x [3.88, 4.14] | above the bar |
| sample, top-p, vocab 32,000 | 3.96x | 4.2x [4.13, 4.33] | above the bar |

Serde (`examples/serde_abba.rs`, 9 ABBA epochs, median and epoch range; the
driver reports the median regression, not an interval):

| Serde workload | Before | After | Verdict |
|---|---|---|---|
| encode_doc (new), mixed document | 2.43x (1.76-3.36) | 1.71x (1.42-2.37) to 1.85x (1.57-2.50) | above the bar |
| f64_encode, 4,000 values | 0.90x | 0.96x (0.89-1.16) | floats only; the encoder flips as one module |
| parse_scalar | 2.13x | 2.47x (1.74-3.05) | above the bar |
| parse_simd (Rust stage 1, port stage 2) | 2.43x | 2.75x | above the bar |
| f64_decode | 3.28x | 3.44x | above the bar |
| tiles | 2.27x | 2.33x | above the bar |

Alloc (`examples/region_abba.rs`, two binaries): `vec_push` 3.22x, `values`
3.07x, `bytes` 4.70x; `btree` (Rust in both arms, the noise control) 1.05x.
Every region call crosses the C ABI for a few table updates, so the boundary
dominates; nothing was flipped.

## Rust lines

798 lines of Rust implementation left `crates/vesper-tensor/src/` (the
package `vesper-tensor` builds none of it); 811 lines are in
`crates/vesper-tensor/seed/src/engine/`, which only the compiler's seed
builds. Nothing is deleted outright yet: the compiler links the seed until
Stage 2. Tests did not move: the seed still runs every test, so the frozen
Rust engine stays the differential for the flipped modules (the Aura
pattern). The census row is not updated in this PR because the census
script (`scripts/rust-census.py`) is still in the open plan PR #1415; part 2
updates it.

## Compiler gaps

Seen in callgrind on the port objects (with a local, uncommitted compiler
patch that names local functions after their Vesper paths; that naming
would be a cheap profiling aid in `src/native_export.rs`).

1. **Slice headers are reloaded on every access in a loop.** The data
   pointer, offset and length of an `in` slice parameter are loaded again
   for each `w[i]`, with a bounds check each time, even when nothing in the
   loop stores through any slice. `validate_pages` in `pool.ves` costs about
   85 instructions per lease against about 10 in Rust. Repro:

   ```vesper
   pub fn sum_pairs(in w: Slice[u64], n: u64) -> u64 eff [Fail[RuntimeError]] {
       let mut s: u64 = 0;
       let mut at: u64 = 0;
       for _i in 0..n {
           s = wrapping_add(s, w[at]);
           at = wrapping_add(at, 2);
       }
       s
   }
   ```

   `objdump -d` shows `mov (%rsi),…; mov 0x10(…),…; add 0x8(…),…` inside
   the loop.
2. **`try_get` / `try_read_*` results go through stack slots.** The
   `Option` is stored to a stack slot (tag and payload), then reloaded and
   tested; `match s.try_get(i) { Some(b) => …, None => 0 }` costs about 12
   instructions where a bounds check and a load would do.
3. **Large frames and spills in every function.** Small helpers (for
   example `decode_piece`, `candidate`) get 0x200-0x2e0-byte frames and save
   five callee-saved registers, so a per-call export such as
   `Tokenizer::decode` pays about 150 instructions of overhead; that is
   what keeps decode at 1.7x.
4. **Ports build for the baseline target.** `vesper-port-harness` passes
   `--target <cargo triple>`, so Cranelift uses the baseline x86-64 ISA,
   `find_not_in` falls back to one byte per step (no SSSE3 `pshufb`), and no
   kernel can use AVX. The Rust seed is built for the same baseline, but
   LLVM autovectorizes the dequantize and matvec loops with SSE2, which
   Cranelift does not; the kernels need source vectors (manuscript §14.3)
   to close the 2-3x gap.

None of these hunks is in this lane's boundary (codegen belongs to the SH4
and SH5 lanes, the harness to the port infrastructure), so they are handed
to the coordinator.

## Left

- Tensor: K/V pool (1.5-1.7x), decode (1.7x), the kernels (dequantize,
  matvec, attention, quantize, sampling: 2-4x; source-vector rewrites),
  shape plan (3.4x).
- Serde: the encoder (1.7-1.85x: the Rust-side flattening of the value tree
  is about 9% of the port's instructions, the per-value walk most of the
  rest), the scalar and two-stage parsers, float decode, tiles, and the SIMD
  stage 1 with source vectors.
- Alloc: everything (3-5x); the region calls need either inlining across
  the boundary or batching.
- The census row (after #1415 merges) and lane 19's test conversion once the
  seed is retired.
