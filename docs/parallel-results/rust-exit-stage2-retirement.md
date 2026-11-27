# SH6 lane 2: Stage 2, the bootstrap seed and the Rust compiler's retirement

Claim #1397. Prompt: `docs/agent-prompts/self-host-sh6/02-stage2-retirement.md`
(plan PR #1415). Owner decision (2026-11-21): bootstrap from a committed wasm
seed, the way Zig does.

## Part 1: the C runner (#1429)

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

## Since part 1

- **#1690**: `@vesper/stage2` (packages/vesper-stage2) built to wasm by
  `@vesper/wasm-backend` compiles itself: stage2.wasm == stage3.wasm.
- **#1824**: owner decision, the Stage 2 runner is the prebuilt Wasmtime
  v50.0.1 CLI, pinned in `MODULE.bazel` (`@wasmtime//:wasmtime`,
  `tools/bazel/wasmtime.bzl`, `bootstrap/stage2.sh`). `vwasm.c` is frozen:
  no new imports or features for Stage 2.
- **#1844**: all 42 pure-Vesper compiler packages compile under
  `@vesper/stage2`; stage2 == stage3 == stage3_cli == stage3_cwasm.

The gaps part 1 listed (loops, slices and string bytes, input) are closed
by the wasm backend port (#1492, #1506, #1508).

## Part 4a: the gated switch and the retirement checklist (lock #22)

Base: `main` at `907e9782`, rebased after #1871 and #1870.

### What changed

- **`crates/vesper-hybrid/src/swaps.rs`**: the table of the hybrid's 37
  swaps, each with its flip-gate verdict (`pass(config, PR)` or
  `open(why)`), and `VESPER_HYBRID_SWAPS`:
  - `all` (the default, unchanged behaviour): every swap;
  - `gated` (or `VESPER_HYBRID_GATED=1`): only the PASS swaps, today
    `component` (wit-component, #1334), `hoist` (bounds-hoist, #1251) and
    `optimize` (#1501); every other module runs its Rust path;
  - `none`: no swap.

  An unknown value exits 2 before `main`, so a typo cannot silently run
  every swap. The per-swap opt-outs (`VESPER_HYBRID_SEQ=0` and the rest)
  still apply inside a set. `VESPER_HYBRID_STATS=1` prints the set and one
  line per swap. The Vesper driver and command layers are the hybrid's
  entry and run in every set.
- **`install`** (`crates/vesper-hybrid/src/hybrid.rs`) asks `swaps::on`
  before each install; the IR passes are filtered one by one.
- **`crates/vesper-hybrid/tests/swaps.rs`**: `gated` turns on exactly the
  PASS rows, each of which has its `scripts/ci/flip-gate/<config>.toml` and
  a PASS scoreboard row in the plan; under `gated` the parser, the whole
  layout, the IR check, the inliner and the IR optimizer answer 0 calls
  from Vesper while the optimizer and bounds hoisting run; the alias is
  the same set; `all` is the default; `none` installs nothing; a misspelt
  set is refused.
- **`tests/differential.rs`**: `gated_hybrid_matches_the_default_compiler`
  runs the gated set over the corpus (Cargo: every file; Bazel: every 4th,
  `VESPER_GATED_STRIDE=4`). The test's timeout is now `long`.
- **`docs/maintenance/rust-exit-plan.md`**: "The retirement checklist":
  the three sets, how a swap joins `gated`, the seven conditions for the
  retirement PR and what that PR deletes and keeps.

### Evidence

Linux x86_64, local; debug hybrid against the release default compiler
(the times compare different profiles, so they are not a speed result).

| Check | Result |
|---|---|
| `gated_hybrid_matches_the_default_compiler` | 554 files, 2,216 runs, **0 differ** (9 runs are the accepted optimizer equal-cost ties, settled by the Rust producer as in the default set) |
| `hybrid_matches_the_default_compiler` (`all`, unchanged) | 554 files, 2,202 runs, **0 differ**; 14 wasm builds only the hybrid compiles |
| `tests/swaps.rs` | 5/5 pass |
| The rest of `tests/differential.rs` | pilot, pact, wasm and component tests pass |

Stage 1 guard: no guarded path changed; the change is inside
`crates/vesper-hybrid` and docs.

**Rust lines deleted:** 0. This part adds the switch and its tests.

## Part 5a: the gated default build and the verified codegen gate

### What changed

- **`gated-default`** (`crates/vesper-hybrid`): a feature that makes
  `gated` the set when neither `VESPER_HYBRID_SWAPS` nor
  `VESPER_HYBRID_GATED` names one. It is the build the default `vesper`
  switches to (piece S1 of the default switch, below), so that build needs
  no environment variable. Without the feature nothing changes: the
  default stays `all`, and an empty `VESPER_HYBRID_SWAPS` means the build's
  default.
- **`scripts/ci/flip-gate/verified-codegen.json`**: the verified code
  generator's gate. The differential is every corpus world through
  monomorphization, the front half, the lowering and the whole compile on
  five targets; the `compile` workload times the swap's whole path from the
  checked world (optimized program, `compile_input`'s encoding,
  `compile_words`, decoding) against Rust's `build` from the same world.
  `compile_flip_gate` (tests/self_host_verified_codegen.rs) now prints one
  pair per epoch for the gate. The `verified_codegen` row is
  `pass("verified_codegen", "verified-codegen", "#1886")`, so the gated
  set runs it.

### Evidence (Linux x86_64, release, `-C target-cpu=native`)

| Check | Result |
|---|---|
| `aspect ci flip-gate -- verified-codegen` | **PASS**: 0 differences on 4,724 cases, tests 1/1, compile **1.02x** [0.97x, 1.05x] n=12 |

**Rust lines deleted:** 0. `src/verified_codegen` (9,760 lines) goes at
the retirement.

## Part 5b: the gated hybrid under the shipped name (piece S2)

### What changed

- **`src/swaps.rs`**: a hybrid started as `vesper` (the file name without
  `.exe`) defaults to `gated`, like a `gated-default` build. Started as
  `vesper-hybrid`, as the port lanes and their tests run it, it still
  defaults to `all`. The variables override both.
- **`//:vesper_shipped`**: the hybrid binary copied under the
  shipped name (`shipped/vesper`, `shipped/vesper.exe` on Windows), so
  Bazel has the gated compiler without a second build of the crate. The
  Rust `//:bin/vesper` is unchanged: it still builds the port objects and
  is what every task runs until piece S4 moves them.
- **`tests/swaps.rs`**: `the_shipped_name_runs_the_gated_set` copies the
  hybrid to a `vesper` file and checks that it runs exactly the gated set
  with no variable, that the parser stays on Rust, and that
  `VESPER_HYBRID_SWAPS=all` still turns every swap on.

**Rust lines deleted:** 0.

## Part 5c: the acceptance tasks run the gated `vesper` (piece S4)

### What changed

- **`.aspect/lib/bazel_exec.axl`**: one constant, `VESPER =
  "//:vesper_shipped"`, names the compiler the tasks run as `vesper`.
  Every acceptance task that ran `//:bin/vesper` (native acceptance,
  diagnostics, fmt, editor, recovery, source tooling and macros, format
  proof, canonical protocols, doc links, Logos CI) now runs it, and
  `aspect ci prebuild-acceptance` builds it. These are the tasks of the
  required `Native and acceptance scripts` check.
- **`//:vesper_shipped`** moved from `crates/vesper-hybrid` to the root
  package, at `<bin>/shipped/vesper`, with the CLI tools as data. The
  tool lookup searches `<exedir>/../crates/vesper-tools/bin/`, so the
  shipped binary finds `vesper-grammar` and the other tools the same way
  the Rust `//:bin/vesper` does.
- The Rust `//:bin/vesper` is unchanged and still builds the port objects
  (`vesper_port.bzl`, the hybrid's `build.rs`).

The task self-tests (`aspect ci tests` for format-proof, native-cli,
diagnostics, doc-links, source-tooling, the fixture suites and bazel-cli)
pass locally.

**Rust lines deleted:** 0.

## Part 5d: docs and the default-CLI check name the shipped `vesper` (piece S5)

### What changed

- **README, `docs/bazel.md` and the website's setup tabs** build and run
  the shipped compiler: `bazel build //:vesper_shipped` and
  `bazel-bin/shipped/vesper`, or with Cargo
  `cargo build --locked -p vesper-hybrid --features gated-default`. The
  README says what the hybrid is and that `//:bin/vesper` is the Rust
  compiler that builds the ported parts.
- **`aspect ci check-default-cli-dependencies`** checks the normal
  dependency graph of both packages whose binary is the default `vesper`
  (`vesper` and `vesper-hybrid`) for Wasmtime and the Loom sandbox; the
  hybrid has none today. Its self-test covers a forbidden dependency that
  only the hybrid has.
- Release validation runs no `vesper` binary of its own, so it needs no
  change.
- S3 (the Cargo rename) moves to the retirement PR; see the pieces list
  below.

**Rust lines deleted:** 0.

## The default switch, in pieces

Plan of 2026-11-26. The default `vesper` switches to the gated hybrid
in pieces, each of which keeps the Rust binary and changes no output:

- S1: the `gated-default` build (part 5a).
- S2: Bazel's `//:vesper_shipped`, the gated hybrid under the
  shipped name (part 5b); the Rust binary stays the boot compiler of the
  port rules.
- S3: the same for Cargo: the root package's binary is renamed and the
  hybrid crate's binary becomes `vesper`. Deferred to the retirement PR
  (part 5d): the rename touches every root test that runs
  `CARGO_BIN_EXE_vesper` (180 files), the port harness's nested build and
  every lane's local recipe, while the Rust binary still has to exist to
  build the ports. Until then Cargo builds the shipped compiler with
  `-p vesper-hybrid --features gated-default`.
- S4: the required checks run their CLI tests against the gated hybrid
  (part 5c).
- S5: docs, release validation and the default-CLI dependency check
  (part 5d).

That switch alone deletes no Rust: the ports are still compiled by the
Rust compiler. A flipped module's Rust can go one PR at a time once Stage 2
builds the port objects, which needs `hd_step` gone and Stage 2 to emit
native objects (the `codegen-*` gaps in `STAGE2_PACKAGE_GAPS`).

## Retirement blockers found

Besides the compiler rows that have not passed the gate (checklist item
1) and `hd_step` (item 2):

1. **`crates/vesper-oracles` links the Rust compiler.** Its `Cargo.toml`
   depends on the root `vesper` crate: the arithmetic oracle's worker
   drives the parser, checker, optimizer, evaluator and native backend in
   process. The oracles stay Rust (owner), so their system under test must
   become the Vesper compiler as a process or through a C ABI before
   `src/` can go.
2. **`vesper-codegen-verified` links three frozen seeds**
   (`vesper-loom-calculus-seed`, `vesper-loom-cas-seed`,
   `vesper-loom-trust-seed`), which the retirement deletes.
3. **`src/verified_codegen`** (the Stage 1 guard) lives in `src/`. Owner
   decision (2026-11-25): port it to Vesper (#1886).
4. **The Stage 2 fixed point is of `@vesper/stage2`**, the standalone
   compiler package, not of the compiler the hybrid runs. #1928 extends it
   to the packages: every compiler package Stage 2 builds (49) comes out
   byte-identical from the boot Stage 2, from the self-built `stage2.wasm`
   and from `stage2.cwasm` under the pinned wasmtime CLI, and the four that
   it cannot build yet are listed with the reason
   (`STAGE2_PACKAGE_GAPS` in `tests/wasm_seed_package_io.rs`). The seed
   still has to be the complete compiler (driver, checker, backends) as one
   program, which needs items 1 and 2 of the checklist first.

## Left

- Part 4b: remove `HybridDriver::hd_step` (checklist item 2).
- Part 2: commit `bootstrap/seed/vesper.wasm` and its regeneration script
  once the complete compiler builds to wasm; the no-Rust CI check.
- Part 3: `bootstrap/stage2.lock` for the complete compiler and the CI
  gate on it.
- Part 5e onward: piece S3 (with the retirement) of the default switch, then the
  retirement PR (the plan's checklist).
- Census: lane 2's rows (`codegen_swap`, `ir_swap`, `swap_input`,
  `compiler_modules`, `vesper-hybrid`) change only at the retirement.
