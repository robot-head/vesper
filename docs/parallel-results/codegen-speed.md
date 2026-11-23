# Codegen Speed (#1426)

Workstream E codegen speed improvements on the Cranelift-backed native backend and kinetic `fastmath`.

## What changed

1. **Scalar `f_sin` / `f_cos` in `crates/vesper-kinetic/vesper/src/fastmath.ves`**:
   - Optimized scalar `f_sin` and `f_cos` fastmath paths while keeping all `fastmath` ULP bounds, Annex F special values, `DetFloat` policy, and cross-arch sweep hashes intact.
2. **G10 register-returned small `Option`/`Result` enums (`src/native.rs`, `src/native_aggregates.rs`)**:
   - Fixed `return_exit` when `target.join` is `None` (early return from a small-enum function) to return the scalar words via `return_words` instead of falling through without returning register words.
   - Fixed `return_value` when `self.own_abi.result_words.is_some()` to pass `self.result_buffer.unwrap_or(value)` rather than bypassing the register-return path.
3. **Eliminate header reloads in inlined callees (`src/native_clif_opt.rs`, `src/seq/native.rs`)**:
   - Iterated `merge_blocks` -> `fold_constant_branches` -> `remove_unreachable` in a 3-pass fixpoint in `native_clif_opt::run` so unreachable blocks from folded `Hoisted(TryGet)` guards are pruned before block merging, collapsing single-predecessor guards into one block for loop vectorization (`inlined_add_rank_hoists_header_and_vectorizes`).
4. **Cold outlining (`src/native_cold_outline.rs`, `src/compiler_modules.rs`, `src/native.rs`, `src/native_verifiers.rs`)**:
   - Added `native_cold_outline::outline_cold_regions` to split cold error/tail/early-exit regions into `::cold#N` helper functions (keeping hot loops free of cold spills/callee-saved pressure), skipping `ConstTime` functions and registering `::cold#N` children in `det_float.functions` / native emissions.
   - Tuned `region_worth_outlining` so trivial `<8` non-terminator-instruction cold stubs with a single dead stack slot (such as `float_main` in `native_verifiers_cli`) are not outlined into an extra call.

## Targeted verification (`--release`)

- `vesper` (`native_bounds_facts` 13/13, `native_clif_opt` 5/5, `native_cold_inline` 2/2, `native_cold_outline` 2/2, `native_hoist_headers` 3/3, `native_local_symbols` 1/1, `native_option_results` 1/1, `native_slice_headers` 3/3, `native_sroa` 4/4, `native_vectorize` 8/8, `native_verifiers_cli` 8/8)
- `vesper-kinetic`: `fastmath` (5/5), `bench_batches` (3/3)

## ABBA benchmark measurements (Windows x86_64 MSVC, `--release`)

| Crate | Workload | Rust | Vesper | Vesper / Rust (median) | 95% CI / Range | < 1.5x gate? |
|---|---|---:|---:|---:|---|---|
| `vesper-kinetic` | `fastmath` (30 epochs, 64 ops) | 30,791 ns/run | 36,335 ns/run | **1.25x** | range `0.85x..1.78x` (+23%..+25%) | Yes |
| `vesper-quarry` | `planner_abba` (9 epochs, 200 passes) | 5.7–7.4 us/query | 8.0–9.6 us/query | **1.364x** | epochs `1.149x..1.666x` (`3638..3639 bp`) | Yes |
| `vesper-warp-umem` | `abba_echo_loop` (30 epochs x 16 ops) | — | — | **1.336x** | 95% CI `[0.967, 1.714]` (Inconclusive) | Median < 1.5x (CI upper 1.714x) |
| `vesper-warp` | `ack` (12 epochs) | 53 ns/pkt | 70 ns/pkt | **1.31x** | 95% CI `1.231x..1.429x` | Yes |
| `vesper-warp` | `buffers` (12 epochs) | 1,051 ns/chunk | 1,135 ns/chunk | **1.08x** | 95% CI `1.019x..1.139x` | Yes |
| `vesper-warp` | `transfer` (12 epochs) | 27,688 ns/round | 31,448 ns/round | **1.14x** | 95% CI `0.953x..1.216x` | Yes |
| `vesper-warp` | `frames` (12 epochs) | 349 ns/pkt | 545 ns/pkt | 1.55x | 95% CI `1.503x..1.641x` | No |
| `vesper-warp` | `recovery` (12 epochs) | 42 ns/pkt | 118 ns/pkt | 2.79x | 95% CI `2.700x..2.877x` | No |
| `vesper-serde` | `f64_encode` (9 epochs) | — | — | **1.16x** | epochs `0.84..1.30` (`1571..1572 bp`) | Yes |
| `vesper-serde` | `parse_scalar` | — | — | 1.93x | epochs `1.60..2.42` | No |
| `vesper-serde` | `encode_doc` | — | — | 2.31x | epochs `1.93..2.85` | No |
| `vesper-serde` | `tiles` | — | — | 2.32x | epochs `2.09..2.84` | No |
| `vesper-serde` | `parse_simd` | — | — | 2.94x | epochs `2.40..3.68` | No |
| `vesper-serde` | `f64_decode` | — | — | 3.12x | epochs `2.88..3.49` | No |
| `vesper-tensor` | `bpe_encode` (20 epochs) | 1,032.3 us | 923.5 us | **0.974x** | 95% CI `[0.873, 1.067]` | Yes |
| `vesper-tensor` | `gguf_header` | 189.5 us | 181.1 us | **0.987x** | 95% CI `[0.923, 1.096]` | Yes |
| `vesper-tensor` | `llama2c_load` | 360.4 us | 383.2 us | **1.059x** | 95% CI `[1.007, 1.095]` | Yes |
| `vesper-tensor` | `llama2c_decode` | 138.2 us | 163.2 us | **1.130x** | 95% CI `[1.117, 1.135]` | Yes |
| `vesper-tensor` | `safetensors_header` | 5,071.9 us | 6,007.7 us | **1.285x** | 95% CI `[1.142, 1.394]` | Yes |
| `vesper-tensor` | `quantize_q4_k` | 3,944.0 us | 5,303.5 us | **1.369x** | 95% CI `[1.291, 1.523]` | Median < 1.5x (CI upper 1.523x) |
| `vesper-tensor` | `kv_append_read` | 216.1 us | 305.2 us | 1.518x | 95% CI `[1.438, 1.598]` | No |
| `vesper-tensor` | `dequantize_q8_0` | 374.5 us | 581.8 us | 1.533x | 95% CI `[1.425, 1.618]` | No |
| `vesper-tensor` | `dequantize_q4_k` | 439.9 us | 811.0 us | 1.837x | 95% CI `[1.654, 1.903]` | No |
| `vesper-tensor` | `shape_plan` | 1,494.2 us | 3,392.0 us | 2.261x | 95% CI `[2.022, 2.379]` | No |
| `vesper-tensor` | `matvec_q8_0` | 102.8 us | 274.8 us | 2.630x | 95% CI `[2.543, 2.725]` | No |
| `vesper-tensor` | `attention` (two-pass / online) | 621.1 / 619.8 us | 1,607.5 / 1,912.7 us | 2.673x / 2.847x | 95% CI `[2.566, 2.968]` / `[2.559, 3.266]` | No |
| `vesper-tensor` | `sample_top_p` | 259.5 us | 861.9 us | 3.231x | 95% CI `[3.041, 3.336]` | No |
