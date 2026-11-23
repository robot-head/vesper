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
vwasm --arg vesper --arg build ... bootstrap/seed/vesper.wasm  # seed builds stage 1
stage1/vesper build ...                              # stage 1 builds stage 2
```

- `vesper.wasm` (not committed yet) is the Vesper compiler built by its own
  wasm backend with the portable effect lowering.
- `runner/vwasm.c` runs it. Build the C11 source with the adjacent
  `seed_files.h` header and the C standard library.
- A regeneration script rebuilds `vesper.wasm` from the current compiler.
  CI checks that the seed, run by `vwasm`, rebuilds the compiler to the
  same bytes.

## What is here today

| Piece | State |
|---|---|
| `runner/vwasm.c` | Done. A core WebAssembly interpreter covering the MVP, sign extension, saturating truncation, bulk memory, multi-value, funcref tables and tail calls. It takes the first core module of a `wasm32-wasip2` component. Its WASI imports cover stdout, guest arguments, environment, initial directory and access to explicitly mapped files. It refuses WasmFX, SIMD, threads, memory64 and exceptions when it loads a module, so it never runs them wrongly. |
| Runner differential | `tests/stage2_seed_runner.rs`. Every corpus program the wasm backend builds (portable effects, optimizer on and off) runs in Wasmtime and in `vwasm`. They must agree on trap or return, the result bits, stdout and a digest of the whole linear memory. |
| Stage 1 front end under the runner | The same test builds the Stage 1 front end (`packages/vesper-compiler`) with the wasm backend. `vwasm` then reproduces all 11 results in `bootstrap/stage1.lock`: 10 CST digests and the byte-class checksum. |
| Parser with filesystem input | The Vesper backend builds `@vesper/front` to Wasm. The test compares native and Wasmtime parse tables across the corpus, then compares the C runner's output for one mapped source file. |
| Backend running under the C runner | `wasm_backend_emits_runnable_core_under_seed_runner` compiles the Vesper backend to Wasm. The test executes a small emitted program and then gives the backend its own encoded Place IR. The C runner must reproduce the complete native backend Wasm core byte-for-byte. The Rust compiler still produces the typed IR for this comparison. |
| `vesper.wasm` | Blocked. The wasm backend cannot build the compiler yet (see below). |

To reproduce the backend comparison from source on the host, run:

```sh
cargo test --locked -p vesper --test wasm_seed_package_io wasm_backend_emits_runnable_core_under_seed_runner -- --exact
```

These checks run in the [Stage 2 prerequisite workflow](../../.github/workflows/bootstrap-stage2.yml).

## What blocks the committed seed

The Vesper wasm backend builds a package over `@vesper/front` and
`@vesper-std/fs`. The package reads source files and writes parse tables.
`front_end_parses_like_native` in `tests/wasm_seed_package_io.rs` compares
those tables with the native frontend's output. The backend supports the
loops, strings, sequences, handlers and WASI file operations used by that
package.

The hybrid driver now runs its command logic in Vesper. It still crosses
`HybridDriver::hd_step` to a Rust session for package loading, checking and
code generation. A seed must run those passes itself and emit the compiler
artifact without asking a host Rust compiler or spawning a native linker.
See the [driver report](../../docs/parallel-results/self-host-driver.md)
and the [Wasm package results](../../docs/parallel-results/rust-exit-gaps-wasm-seed-input.md).

The next handoff is from the assembled AST to the checker. Today
`src/check.rs::check_core` calls Rust's `World::collect` after desugaring
and elaboration. Vesper checker passes receive tables built from that Rust
world. A standalone compiler must collect declarations, type-check bodies
and pass their types, calls, moves and effects to IR lowering directly.

Stage 2 requires Stage 1 to build and witness-check the compiler in the
Loom sandbox. The resulting compiler must then rebuild the same bytes
before recording `bootstrap/stage2.lock`. The frontend differential
exercises the input and parsing portion of that chain.

## Building the runner

```sh
cc -std=c11 -O2 -o vwasm bootstrap/seed/runner/vwasm.c -lm
cl /nologo /O2 /std:c11 bootstrap\seed\runner\vwasm.c   # MSVC
vwasm --dump program.wasm                 # imports and exports
vwasm --invoke 'vesper:program/entry@0.1.0#main' --print program.wasm
vwasm --arg vesper --arg build --env MODE=release --cwd /src program.wasm
```

The WASI environment starts with an empty argument list, empty environment,
and absent initial working directory. Each `--arg` adds one guest argument
in order, including the executable name if the program expects it. Each
`--env` adds a key/value pair. `--cwd` supplies a guest directory name.
Place these options before the module path. Positional arguments after
the path still supply flat numeric parameters to the invoked core export.

The runner exposes a virtual `/` containing the individual guest paths named
by `--input GUEST HOST` and `--output GUEST HOST`. Inputs can be read. Outputs
can be created or written. For example, `--input /src/main.ves main.ves`
makes the host's `main.ves` available under the guest name `/src/main.ves`.
The runner reports file type and length. Directory enumeration and timestamps
are unavailable.

Exit status: 0 on success (the `wasi:cli/run` result when that export is
invoked), 2 for a usage or load error, 3 for a trap.
