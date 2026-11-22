# SH6 lane 2: Stage 2, the bootstrap seed and the Rust compiler's retirement

Claim #1397. Prompt: `docs/agent-prompts/self-host-sh6/02-stage2-retirement.md`
(plan PR #1415). Owner decision (2026-11-21): bootstrap from a committed wasm
seed, the way Zig does.

## Part 1: the C runner (this PR)

Base: `main` at `ab965f2a`.

### What changed

- **`bootstrap/seed/runner/vwasm.c`**: a WebAssembly interpreter in C11,
  with no dependencies beyond the C standard library and no Rust. It covers:
  - the core MVP instruction set, sign extension, saturating truncation,
    bulk memory, multi-value block types, funcref tables, and tail calls
    (`return_call`, `return_call_indirect`);
  - one memory, one table, and globals;
  - for a `wasm32-wasip2` component, the first core module (wit-component
    puts the program's module first) with the WASI 0.2 functions the backend
    imports (`get-stdout`, `blocking-write-and-flush`, the stream drop and
    `monotonic-clock.now`), plus no-op Beacon trace imports.

  Features it doesn't run are refused when the module loads, never run
  wrongly: WasmFX, SIMD, threads, memory64, multiple memories, exceptions and
  GC types. Branch targets come from a side table built at load time, and
  calls use an explicit frame stack, so deep recursion doesn't use the C
  stack.
- **`tests/stage2_seed_runner.rs`**:
  - `vwasm_matches_wasmtime_on_the_corpus`: every corpus world with a
    parameterless `main` that the backend builds (portable effects, with
    and without the IR optimizer) runs in Wasmtime's core API and in
    `vwasm`. The two must agree on trap or return, the result bits, stdout,
    and the FNV-1a digest of the whole linear memory after the call.
  - `stage1_front_end_runs_under_vwasm`: the Stage 1 front end
    (`packages/vesper-compiler`, assembled the way `bootstrap/stage1.rs`
    assembles it) is built with the wasm backend and run by `vwasm`. It must
    reproduce every `result` in `bootstrap/stage1.lock`.
  - The test builds `vwasm.c` with the host C compiler: `$CC`, else `cc`,
    and on Windows `cl.exe` found through `find-msvc-tools`, which the root
    crate already depends on there.
- **`bootstrap/seed/README.md`**: the seed's layout, its current state and
  what blocks it.

### Evidence (Linux x86_64, local, release)

| Check | Result |
|---|---|
| Runner vs Wasmtime | 270 runs compared (24 of them trap in both), **0 differ**. 6 runs went over the 20M-instruction fuel limit and were skipped. The backend did not build 84 program and option pairs. |
| Stage 1 front end under `vwasm` | **11 of 11** `stage1.lock` results reproduced: 10 CST digests and the lexer byte-class checksum |
| Bootstrap corpus as components | All 10 programs return their `expected-status.txt` status under `vwasm` |
| `gcc -std=c11 -Wall -Wextra` and `clang -std=c11 -Wall -Wextra -pedantic` | no warnings |

**Stage 1 guard:** no file under `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt`, `lean/**`, `bootstrap/stage1*` or `packages/vesper-compiler/**`
changed. The new test only reads the front-end sources and the lock.

**Rust lines deleted:** 0. This part adds C (the runner) and a Rust test.

## Compiler gaps (block the committed seed)

The wasm backend can't build the compiler yet. The seed needs:

1. **Loops.** Repro: `fn main() -> u64 { let mut h: u64 = 0; for i in 0..10 { h = wrapping_add(h, i); } h }`
   fails with "unsupported Operation in 'main': loop, for, break and return
   are not lowered to Wasm yet".
2. **Slices and string bytes.** Repro: `fn go(in b: Slice[u8], i: u64) -> u64 { match b.try_read_u64_le(i) { Option::Some(x) => x, Option::None => 0, } } fn main(input: String) -> u64 { go(in input.as_bytes(), 0) }`
   fails with "IR operation unknown to the Wasm backend". The compiler's
   packages also use `ChunkSeq` and the other heap sequences everywhere.
3. **Input.** The backend's only host capabilities are `Stdout`, `Clock` and
   `Trace`. A seed has to read sources and write bytes. The smallest
   addition is `String` parameters on `main`, which the canonical ABI already
   lowers through `cabi_realloc` and which `vwasm` can serve, plus a byte
   `Stdout`. A WASI `stdin` or filesystem capability would also work.
4. **A Rust-free compiler.** That is Stage 2: SH5 lane 12's driver (#1367),
   then every compiler row of the census at the flip bar.

Items 1-3 are backend breadth over the same Place IR the native backend
lowers. They belong in the Vesper port (`@vesper/wasm-backend`, #1311),
because the Rust backend is deleted at the retirement and the seed must be
regenerable without it. No current lane owns this breadth.

## Left

- Part 2: once the backend can build the compiler, commit
  `bootstrap/seed/vesper.wasm`, add the regeneration script and the CI check
  that the seed rebuilds the compiler to the same bytes.
- Part 3: Stage 2 after #1367 merges. Build the compiler packages with the
  hybrid, rebuild them with the result, compare, record
  `bootstrap/stage2.lock`, and add `.github/workflows/bootstrap-stage2.yml`
  (manual dispatch and nightly).
- Part 4: the default switch for the hybrid, and the retirement checklist in
  `docs/maintenance/rust-exit-plan.md` once #1415 merges.
- Census row: `scripts/rust-census.py` arrives with #1415. The row is
  updated in the first part after that.
