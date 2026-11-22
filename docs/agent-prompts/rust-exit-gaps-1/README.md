# Rust-exit gap wave 1: the compiler gaps that keep ports above the bar

> Plan written 2026-11-22 against `main` at `aceafcf4` (claims #1498-#1506,
> #1508-#1510). The [manuscript](../../manuscript/README.md) is the sole
> specification. The program is [rust-exit-plan.md](../../maintenance/rust-exit-plan.md);
> the numbers are in the [census](../../maintenance/rust-census.md). This wave
> runs beside SH6 ([plan](../self-host-sh6/README.md)), whose lanes keep
> porting and flipping. Confirm live ownership on the issue board before acting.

**Why this wave.** The owner asked (2026-11-21) to prioritize closing compiler
gaps, and then (2026-11-22) to start the next wave from what is done. The
SH6 lane reports show the same handful of code-generation gaps keeping ports
above the 1.5x bar: `Option` results through memory, vectors on the stack, cold
inlined callees on hot paths, the cost of a call across the export ABI, and
slices that cannot be bound. Then there are the interop gaps that block the
OS crates, the wasm seed's breadth, and the compiler rows no lane owns.

**Census at `aceafcf4`:** 543k lines of non-test Rust:
- 207k in the compiler and its own crates.
- 192k in libraries whose port is unfinished or above the bar.
- 65k in wrappers over C or OS APIs.
- 17k in wrappers of Rust-only libraries.
- 12k in shims.
- 14k kept on purpose (the DDC auditor and oracles).
- 21k already Vesper-only or seeded.

There are also 384k lines of Rust tests. The total rose by 24k since SH6
started, because new compiler passes and new crates (the crypto and BLAKE3
shims, CLIF clean-up) landed faster than flips deleted Rust. Closing the gaps
below is what lets the flips catch up.

## Already owned (not in this wave)

| Gap | Owner |
|---|---|
| Native code size: CLIF clean-up, `bool` materialization, header reloads of slice views and seq fields, spills and frames in large functions, enum construction, Quarry `\|\|` chains | native code size thread, #1426 |
| `try_get`/`try_set` loop vectorizer | #1474 |
| Proven indices still checked; hoisting the remaining checks (Ignis items 1-2) | bounds-hoisting thread |
| C callbacks, foreign function pointers, `.vsys` floats, runtime-symbol imports (graphics gaps 1-3, 5-8) | FFI lowering, #1287 (PRs #1456-#1459) |
| The C wasm runner and the seed itself | SH6 lane 2, #1397 |

## Lanes

| Lane | Prompt | Claim | Starts | Exclusive boundary |
|---|---|---|---|---|
| 1 | [Option and Result results in registers](01-option-results.md) | #1498 | now; continues SH5 lane 7 (#1330), which this lane closes | the enum-result hunks of `src/native_aggregates.rs`, `src/native_abi*` and the call-result hunks of `src/native.rs`, the matching hunks of `packages/vesper-codegen-*` (coordinate with the SH4 family that owns each), `tests/native_option_results*.rs` |
| 2 | [Cold inlined callees off the hot path](02-cold-inline.md) | #1499 | now (handed over by #1426) | the inliner-policy and cold-path hunks of `src/ir_inline.rs` (Rust) and `packages/vesper-ir-inline` (Vesper port, keep both in step), the slot-placement hunks of `src/native.rs` that this needs, `tests/native_cold_inline*.rs` |
| 3 | [Vectors in registers and the SIMD paths](03-vectors-in-registers.md) | #1500 | now | `src/native_vector.rs`, the vector hunks of `src/vector.rs` and `src/tiled/`, the vector hunks of `packages/vesper-codegen-numeric` (SH4 lane 11 owns that package: coordinate, or hand over the patch), `tests/native_vector_registers*.rs` |
| 4 | [The optimizer's checker within 1.5x](04-optimizer-checker.md) | #1501 | now | `packages/vesper-optimize/**` (checker files), `scripts/ci/flip-gate/optimize.toml`, `tests/self_host_optimize*.rs` hunks for the checker, report |
| 5 | [Cheap calls across the export ABI](05-export-calls.md) | #1502 | now | the export ABI hunks of `src/native_export.rs` and `packages/vesper-codegen-export` (SH4 lane 12, #1286: coordinate), `crates/vesper-port-abi/**`, `crates/vesper-port-build/**`, the binding generator, `tests/export_call_cost*.rs` |
| 6 | [Slices as values](06-slices-as-values.md) | #1503 | now (manuscript change, flagged) | the slice-binding hunks of the checker (`src/seq/check.rs`, the E1521 site) with the matching `packages/vesper-check-regions-seqs` hunks, the inliner's slice-parameter rule in `src/ir_inline.rs` and `packages/vesper-ir-inline`, byte-slice `cmp`/`eq` lowering, the smallest manuscript hunk (flagged in the PR), `tests/slice_values*.rs` |
| 7 | [Platform interop: per-target imports, assembly objects, CPU intrinsics](07-platform-interop.md) | #1504 | now | the per-target `vsys` and `objects` manifest hunks of `src/packages.rs` and `packages/vesper-check-names-packages` (SH2 lane 6, #1178: coordinate), `src/vsys_commands.rs` (`host_symbols`, `build`), the intrinsic hunks of the native backend, the runtime-import hunk of `src/seq/aot_runtime.rs` (SH4 lane 9: coordinate), the smallest manuscript hunks (flagged), `tests/platform_interop*.rs` |
| 8 | [Foreign memory: pinned buffers and in-place views](08-foreign-memory.md) | #1505 | now (manuscript change, flagged) | the smallest manuscript hunks in §16.3 (flagged), the `.vsys` parser hunks in `crates/vesper-ffi`, and their checker and lowering hunks (`src/ffi_lowering/`; SH4 lane 13, #1287, owns its port: coordinate with its open PRs #1456-#1459), `packages/vesper-std/ffi/**`, `tests/foreign_memory*.rs` |
| 9 | [Wasm seed breadth: slices, strings and ChunkSeq](09-wasm-seed-strings.md) | #1506 | now; continues the wasm backend lane's plan (#1289) after #1492 (loops) | the slice, string and sequence hunks of `src/wasm_backend/**` and its Vesper port `packages/vesper-wasm-backend` (keep both in step; the hybrid differential must stay at 0), `tests/wasm_seed_breadth*.rs` |
| 10 | [Wasm seed breadth: input, heap and a whole package](10-wasm-seed-input.md) | #1508 | after lane 9's first PR | the host-capability, heap and handler hunks of `src/wasm_backend/**` and `packages/vesper-wasm-backend`, `tests/wasm_seed_package*.rs`; the C runner itself stays with SH6 lane 2 (#1397) |
| 11 | [Port the native walk](11-native-walk.md) | #1509 | now (after the SH4 family hooks it drives) | new `packages/vesper-codegen-walk/**`, the hybrid hook that installs it (serialize with other hybrid PRs), `tests/self_host_native_walk*.rs`; `src/native.rs` and `src/native_clif_opt.rs` are read-only to this lane (the native code size thread #1426 and gap lanes 1-3 edit them; re-sync the port to their changes) |
| 12 | [Leftover modules and sys crates](12-leftovers.md) | #1510 | now | `packages/vesper-ir-opt-policy/**` (new), the database rows of `packages/vesper-memo` (from SH5 lane 16), new tour package files, `crates/vesper-blake3-sys` and its users' `.vsys` hunks, tests named after each, report |

**Start order.** Every lane starts now except lane 10, which follows lane 9's
first PR. Lanes 1-3 touch the native backend next to #1426. Each agrees its
split with #1426 on that issue before editing, and they merge one at a time
in the order their PRs are green.

## Rules

The SH6 "Rules for every lane" apply, plus:

- **Measure on the ports.** A gap lane's acceptance is the ports that
  reported the gap, re-measured through the flip gate. When a port now
  passes, tell its SH6 lane.
- **Rust and Vesper in step.** A change to a Rust pass that has a Vesper port
  changes the port in the same PR, or is handed to the port's lane.
- **Manuscript changes are flagged** in the PR body for the owner (lanes 6,
  7 and 8).
