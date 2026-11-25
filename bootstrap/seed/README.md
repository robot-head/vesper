# The bootstrap seed

Owner decisions:
- 2026-11-21: once the Rust compiler is deleted, the repository bootstraps from a **committed wasm seed**, the way Zig does.
- 2026-11-24: **`wasmtime` (`v50.0.1`) CLI**, wired hermetically via Bazel in [MODULE.bazel](../../MODULE.bazel) (`@wasmtime//:wasmtime`, [tools/bazel/wasmtime.bzl](../../tools/bazel/wasmtime.bzl)), is the official **Stage 2 bootstrap runner** for full multi-package compiler self-hosting. The C11 core interpreter ([runner/vwasm.c](runner/vwasm.c)) is frozen for Stage 1 / core-differential corpus checks.

SH6 lane 2 (#1397) owns this directory. The plan is in
[rust-exit-plan.md](../../docs/maintenance/rust-exit-plan.md), and the lane
report is
[rust-exit-stage2-retirement.md](../../docs/parallel-results/rust-exit-stage2-retirement.md).

## How Stage 2 runs

```sh
# Run any Wasm target under the pinned Bazel toolchain:
bazel run @wasmtime//:wasmtime -- --version

# Rebuild a package (or self-rebuild @vesper/stage2) using the official Stage 2 runner:
bootstrap/stage2.sh run bootstrap/seed/vesper.wasm packages/vesper-stage2 target/stage3.wasm main
bootstrap/stage2.sh precompile target/stage2.wasm target/stage2.cwasm
bootstrap/stage2.sh verify --precompile target/stage2.wasm
```

Under Bazel (`test/wasm_seed_package_io`), `BUILD.bazel` places `@wasmtime//:wasmtime`
on `PATH` for `stage2_self_compilation_fixed_point` (which executes `stage2.wasm`
and precompiled `stage2.cwasm` via the hermetic `wasmtime` CLI and verifies
`stage2.wasm == stage3.wasm`) and `stage2_compiles_compiler_packages` (42/42 pure-Vesper
compiler packages).

- `vesper.wasm` (committed once the remaining driver/compiler gaps land) is the
  Vesper compiler built by its own wasm backend with portable effects.
- [bootstrap/stage2.sh](../stage2.sh) wraps `wasmtime run --dir <workspace>::. --dir <out>::/out --argv0 stage2 ...`
  and checks Stage 2 sidecar diagnostics (`.err.txt`).

## What is here today

| Piece | State |
|---|---|
| Hermetic `@wasmtime//:wasmtime` (`v50.0.1`) | Wired in `MODULE.bazel` / `tools/bazel/wasmtime.bzl` for Linux (`x86_64`, `aarch64`), macOS (`aarch64`, `x86_64`), and Windows (`x86_64`, `aarch64`). Official Stage 2 CLI runner via `bootstrap/stage2.sh` and `test/wasm_seed_package_io`. |
| `runner/vwasm.c` (frozen) | A core WebAssembly interpreter covering the MVP, sign extension, saturating truncation, bulk memory, multi-value, funcref tables and tail calls. Frozen; kept for historical Stage 1 / core-differential checks (`tests/stage2_seed_runner.rs`, `tests/wasm_seed_package_io.rs`). |
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
