# Codegen Speed (#1426)

Workstream E codegen speed improvements on the Cranelift-backed native backend and kinetic `fastmath`.

## What changed

### Wave 1 (PR #1606, merged `bc9ae7cf`)
1. **Scalar `f_sin` / `f_cos` in `crates/vesper-kinetic/vesper/src/fastmath.ves`**:
   - Optimized scalar `f_sin` and `f_cos` fastmath paths while keeping all `fastmath` ULP bounds, Annex F special values, `DetFloat` policy, and cross-arch sweep hashes intact.
2. **G10 register-returned small `Option`/`Result` enums (`src/native.rs`, `src/native_aggregates.rs`)**:
   - Fixed `return_exit` when `target.join` is `None` (early return from a small-enum function) to return the scalar words via `return_words` instead of falling through without returning register words.
   - Fixed `return_value` when `self.own_abi.result_words.is_some()` to pass `self.result_buffer.unwrap_or(value)`.
3. **Eliminate header reloads in inlined callees (`src/native_clif_opt.rs`, `src/seq/native.rs`)**:
   - Iterated `merge_blocks` -> `fold_constant_branches` -> `remove_unreachable` in a 3-pass fixpoint in `native_clif_opt::run` so unreachable blocks from folded `Hoisted(TryGet)` guards collapse into one block for loop vectorization (`inlined_add_rank_hoists_header_and_vectorizes`).
4. **Cold outlining (`src/native_cold_outline.rs`, `src/compiler_modules.rs`, `src/native.rs`, `src/native_verifiers.rs`)**:
   - Added `native_cold_outline::outline` to split cold error/tail/early-exit regions into `::cold#N` helper functions, skipping `ConstTime` functions, deferring outlining after the instance lowering loop to preserve `FuncId` order, keeping `shared_tails` stack slots in `outside_slots`, and guarding `Udiv`/`Sdiv`/`Urem`/`Srem` arms.

### Wave 2 (follow-up #1426)
5. **Prevent edge-case guard inversion in `AsymmetricTail` (`src/native_cold_outline.rs`)**:
   - Require `non_const >= 2` in `is_fast_return_arm` and `calls > 0 || !region.has_return` in `OutlineKind::AsymmetricTail`, so 0-call early-return guards (e.g. `if v == 0 { return 64; }` in `clz64`) no longer invert and outline the hot path.
6. **Narrow `ireduce`/`uextend` constant & round-trip folding (`src/native_clif_opt.rs`)**:
   - Added `fold_narrow_extends` to fold `ireduce(iconst)`, `uextend(iconst)`, `ireduce(uextend(x))`, and nested `uextend`/`ireduce` chains.
7. **Inline `mul_wide` to `imul` + `umulhi` (`src/native.rs`)**:
   - At `operands_only` call sites (when no `codegen_swap` `Call` hook is active), lower `mul_wide(a, b)` directly to `imul` + `umulhi` instead of calling the 65-instruction 32-bit fallback body.

## Targeted verification (`--release`)

- `vesper`: `native_bytes` (9/9), `native_clif_opt` (6/6), `native_cold_outline` (2/2), `native_inline_cost` (7/7), `native_option_results` (1/1), `native_vectorize` (8/8), `self_host_codegen_calls` (2/2)
- `vesper-kinetic`: `fastmath` (5/5), `bench_batches` (3/3)

## ABBA benchmark measurements (Windows x86_64 MSVC, `--release`)

| Crate | Workload | Rust | Vesper | Vesper / Rust (median) | 95% CI / Range | < 1.5x gate? |
|---|---|---:|---:|---:|---|---|
| `vesper-kinetic` | `fastmath` (30 epochs, 64 ops) | 30,791 ns/run | 36,335 ns/run | **1.25x** | range `0.85x..1.78x` (+23%..+25%) | Yes |
| `vesper-quarry` | `planner_abba` (9 epochs, 200 passes) | 5.7–7.4 us/query | 8.0–9.6 us/query | **1.364x** | epochs `1.149x..1.666x` (`3638..3639 bp`) | Yes |
| `vesper-warp-umem` | `abba_echo_loop` (30 epochs x 16 ops) | — | — | **1.485x** | 95% CI `[1.222, 1.816]` (Inconclusive) | Median < 1.5x (CI upper 1.816x) |
| `vesper-warp` | `ack` (12 epochs) | 31 ns/pkt | 37 ns/pkt | **1.20x** | 95% CI `1.165x..1.315x` | Yes |
| `vesper-warp` | `buffers` (12 epochs) | 728 ns/chunk | 1,026 ns/chunk | **1.41x** | 95% CI `1.304x..1.471x` | Yes |
| `vesper-warp` | `transfer` (12 epochs) | 16,984 ns/round | 19,346 ns/round | **1.14x** | 95% CI `1.089x..1.199x` | Yes |
| `vesper-warp` | `frames` (12 epochs) | 207 ns/pkt | 359 ns/pkt | 1.62x | 95% CI `1.485x..1.796x` | No |
| `vesper-warp` | `recovery` (12 epochs) | 31 ns/pkt | 73 ns/pkt | 2.46x | 95% CI `2.286x..2.533x` | No |
| `vesper-serde` | `f64_encode` (9 epochs) | — | — | **0.96x** | epochs `0.92..1.14` (`-391..-390 bp`) | Yes |
| `vesper-serde` | `parse_scalar` | — | — | 1.66x | epochs `1.34..1.84` (`6625..6626 bp`) | No |
| `vesper-serde` | `encode_doc` | — | — | 2.15x | epochs `1.87..2.28` | No |
| `vesper-serde` | `tiles` | — | — | 2.20x | epochs `1.80..2.44` | No |
| `vesper-serde` | `parse_simd` | — | — | 2.57x | epochs `1.76..3.52` | No |
| `vesper-serde` | `f64_decode` | — | — | 2.69x | epochs `2.16..3.04` | No |
| `vesper-tensor` | `bpe_encode` (20 epochs) | 877.2 us | 856.9 us | **0.984x** | 95% CI `[0.968, 0.990]` | Yes |
| `vesper-tensor` | `gguf_header` | 166.1 us | 168.2 us | **1.028x** | 95% CI `[0.932, 1.115]` | Yes |
| `vesper-tensor` | `llama2c_decode` | 74.2 us | 82.4 us | **1.113x** | 95% CI `[1.104, 1.161]` | Yes |
| `vesper-tensor` | `llama2c_load` | 326.8 us | 384.1 us | **1.164x** | 95% CI `[1.103, 1.200]` | Yes |
| `vesper-tensor` | `safetensors_header` | 3,736.2 us | 5,183.3 us | **1.366x** | 95% CI `[1.307, 1.409]` | Yes |
| `vesper-tensor` | `quantize_q4_k` | 3,225.7 us | 4,587.0 us | **1.421x** | 95% CI `[1.400, 1.433]` | Yes |
| `vesper-tensor` | `kv_append_read` | 180.9 us | 263.5 us | **1.468x** | 95% CI `[1.441, 1.484]` | Yes |
| `vesper-tensor` | `dequantize_q8_0` | 343.4 us | 538.8 us | 1.632x | 95% CI `[1.578, 1.686]` | No |
| `vesper-tensor` | `dequantize_q4_k` | 387.4 us | 777.9 us | 1.875x | 95% CI `[1.612, 1.924]` | No |
| `vesper-tensor` | `shape_plan` | 1,253.1 us | 2,593.3 us | 2.081x | 95% CI `[2.010, 2.114]` | No |
| `vesper-tensor` | `matvec_q8_0` | 101.8 us | 294.5 us | 2.853x | 95% CI `[2.715, 2.915]` | No |
| `vesper-tensor` | `attention` (two-pass / online) | 423.0 / 440.8 us | 1,227.3 / 1,242.0 us | 2.949x / 2.845x | 95% CI `[2.893, 2.968]` / `[2.766, 2.887]` | No |
| `vesper-tensor` | `sample_top_p` | 250.7 us | 784.7 us | 3.132x | 95% CI `[3.035, 3.166]` | No |
