# The bootstrap seed

Owner decision (2026-11-21): once the Rust compiler is deleted, the repository
bootstraps from a **committed wasm seed**, the way Zig does. SH6 lane 2
(#1397) owns this directory. The plan is in
[rust-exit-plan.md](../../docs/maintenance/rust-exit-plan.md), and the lane
report is
[rust-exit-stage2-retirement.md](../../docs/parallel-results/rust-exit-stage2-retirement.md).

## How it will work

```text
cc -O2 -o vwasm bootstrap/seed/runner/vwasm.c -lm   # any C11 compiler, no Rust
vwasm bootstrap/seed/vesper.wasm build ...           # the seed builds stage 1
stage1/vesper build ...                              # stage 1 builds stage 2
```

- `vesper.wasm` (not committed yet) is the Vesper compiler built by its own
  wasm backend with the portable effect lowering.
- `runner/vwasm.c` runs it. It is one C11 file with no dependencies beyond
  the C standard library.
- A regeneration script rebuilds `vesper.wasm` from the current compiler.
  CI checks that the seed, run by `vwasm`, rebuilds the compiler to the
  same bytes.

## What is here today

| Piece | State |
|---|---|
| `runner/vwasm.c` | Done. A core WebAssembly interpreter covering the MVP, sign extension, saturating truncation, bulk memory, multi-value, funcref tables and tail calls. It takes the first core module of a `wasm32-wasip2` component and serves the WASI 0.2 imports the backend emits. It refuses WasmFX, SIMD, threads, memory64 and exceptions when it loads a module, so it never runs them wrongly. |
| Runner differential | `tests/stage2_seed_runner.rs`. Every corpus program the wasm backend builds (portable effects, optimizer on and off) runs in Wasmtime and in `vwasm`. They must agree on trap or return, the result bits, stdout and a digest of the whole linear memory. |
| Stage 1 front end under the runner | The same test builds the Stage 1 front end (`packages/vesper-compiler`) with the wasm backend. `vwasm` then reproduces all 11 results in `bootstrap/stage1.lock`: 10 CST digests and the byte-class checksum. |
| `vesper.wasm` | Blocked. The wasm backend cannot build the compiler yet (see below). |

## What blocks the committed seed

The wasm backend (`src/wasm_backend`, ported to `@vesper/wasm-backend` in
#1311) builds the native batch subset only. The compiler's packages need:

1. Loops (`for`, `while`, `break`, `return`). Today they are refused: "write
   the iteration as a tail-recursive function".
2. Slices, `Str`/`String` bytes, `ChunkSeq` and the other heap sequences.
   Today these are "IR operation unknown to the Wasm backend".
3. A way to read input. The seed has to read its sources and write its
   output. The backend's only host capabilities are `Stdout`, `Clock` and
   `Trace`. The smallest addition is `String` parameters on `main` (the
   canonical ABI already lowers them through `cabi_realloc`) plus a
   `Stdout` of bytes, or a WASI `stdin`/filesystem capability.
4. A compiler that runs without Rust, meaning Stage 2 (SH5 lane 12's driver,
   #1367, then every compiler row of the census at the flip bar).

Items 1-3 are backend breadth, the same Place IR the native backend lowers.
They must land in the Vesper port (`@vesper/wasm-backend`), because the Rust
backend is deleted at the retirement and the seed must be regenerable
without it.

## Building the runner

```sh
cc -std=c11 -O2 -o vwasm bootstrap/seed/runner/vwasm.c -lm
cl /nologo /O2 /std:c11 bootstrap\seed\runner\vwasm.c   # MSVC
vwasm --dump program.wasm                 # imports and exports
vwasm --invoke 'vesper:program/entry@0.1.0#main' --print program.wasm
```

Exit status: 0 on success (the `wasi:cli/run` result when that export is
invoked), 2 for a usage or load error, 3 for a trap.
