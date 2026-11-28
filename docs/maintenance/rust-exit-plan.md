# Rust-exit plan

> Written 2026-11-21 against `main` at `af7dd900` (claims #1396-#1414). The
> [manuscript](../manuscript/README.md) is the sole specification. The
> numbers per crate and per compiler module are in the generated
> [census](rust-census.md) (`aspect maintenance rust-census`). The compiler
> waves are in [self-hosting-plan.md](self-hosting-plan.md); the first wave of
> this plan is [SH6](../agent-prompts/self-host-sh6/README.md).

**The owner's goal (2026-11-21):** "Drive this until absolutely no rust code
is left except the bare minimum that is impossible to get rid of."

## Where we are

| What | Non-test Rust lines | How it leaves |
|---|--:|---|
| The compiler (`src/`) and its own crates (syntax, core, fmt, lint, LSP, doc, ...) | 199k | ported module by module by SH1-SH5 into the hybrid build; deleted in one retirement PR after Stage 2 (SH6 lane 2) |
| Libraries with a Vesper port that is unfinished or above 1.5x | 198k | SH6 lanes 4-12: finish, reach 1.5x, flip |
| Libraries already Vesper-only (Aura, Pact, Simpulse, Loom solver) | 13k | done; their frozen seeds go at the retirement |
| Crates that wrap a C or OS API | 61k | SH6 lanes 13-16: Vesper calls the C API or syscalls directly |
| Crates that wrap a Rust-only library Vesper can replace | 17k | SH6 lane 18 |
| Shims over vetted Rust-only libraries | 8k today | SH6 lane 17 shrinks them to one crypto shim; Cranelift's shim stays |
| Port infrastructure (hybrid, port harness crates) | 5k | deleted at the retirement |
| Kept Rust on purpose (DDC auditor, `*42` oracles) | 14k | stay, owner call (see below) |
| **Total** | **519k** | |
| Rust tests (`tests/`, `crates/*/tests`) | 355k | SH6 lane 19's recipe, then each flip converts its own |

## The flip bar

**Owner, 2026-11-21:** a module's Rust may be deleted once its Vesper port has
full function and is within 1.5x of Rust's speed; optimization can come later.

In practice, a crate or compiler module flips when:

1. The differential shows 0 differences over the crate's tests (or, for a
   compiler module, the full suite and the bootstrap corpus), and every test
   of the crate passes against the Vesper build.
2. The ABBA benchmark's 95% interval has its upper end at or below 1.5x of
   Rust's time (release builds, the port-compiler profile, the same host).
   For a compiler module, the whole-compiler build time with the swap is
   also within 1.5x.
3. The crate's Rust tests are converted or kept as a differential against the
   last Rust commit.

`aspect ci flip-gate -- <crate>` (SH6 lane 1) checks 1 and 2 and prints the row for
the census. A lane that passes flips without asking the owner; the recipe is
below.

## Flipping a module

Every flip PR runs `aspect ci flip-gate -- <crate>` and pastes its two output lines
(the verdict and the scoreboard row) into the PR body. A lane that gets PASS
flips and deletes without asking the owner.

1. **Write the gate's configuration** at `crates/<crate>/flip-gate.json`, or at
   `scripts/ci/flip-gate/<module>.json` for a compiler module. List the build
   commands, tests against the Vesper build and the seed, the differential,
   one `abba` entry per workload, and the Rust paths to count for the
   scoreboard. Write commands as argument arrays. Use an object with `argv`
   and `env` for command-specific variables, `cargo_target` and `args` for
   paths relative to Cargo's target directory, or `sequence` for commands
   that run in order and stop at the first failure. See
   `scripts/ci/flip-gate/vesper-span.json` for a worked example.
2. **ABBA.** Either point `rust` and `vesper` at two commands that run the
   same workload once (an `abba once <workload> <rust|vesper> <batches>`
   example, or the Rust and hybrid compilers on one package), and the gate
   alternates them Rust, Vesper, Vesper, Rust for 30 epochs; or let the
   harness do its own ABBA and print
   `flip-gate pairs <workload> <rust_ns>:<vesper_ns> ...`, one pair per
   epoch. Use release builds, the port-compiler profile and an idle host;
   size each run at 0.1 s or more so process start stays small.
   **Both halves get the host CPU:** the gate runs the build and ABBA
   commands with `RUSTFLAGS="-C target-cpu=native"` (added to any flags
   already set), and the port harness builds the port objects for the same
   CPU features as the Rust half (#1454). Running a benchmark by hand, set
   it yourself. `--portable` turns it off; tests and the differential keep
   your own RUSTFLAGS. Give benchmark builds `--release` or their own target
   directory so the flag does not rebuild the test artifacts.
3. **Differential.** The differential command must exit 0. A harness that
   counts its cases prints
   `flip-gate differential differences=<D> cases=<C>`, which goes into the
   row; D must be 0.
4. **Run it** with `VESPER_PORT_COMPILER=<prebuilt vesper> aspect ci flip-gate -- <crate>`.
   Exit 0 is PASS (every test and the
   differential pass, and every workload's 95% interval has its upper end at
   or below 1.5x), 1 is FAIL (a failing test, a difference, or a lower end
   above 1.5x), 2 is INCONCLUSIVE (an interval crosses 1.5x: rerun on a
   quieter host or with more epochs, `--epochs 60`), 3 a configuration
   error. The interval is vesper-bench's exact binomial median interval
   (`analyze_pairs`), computed on exact ratios.
5. **Flip.** On PASS: make Vesper the only build (a crate outside the
   compiler's closure deletes its Rust; a crate the compiler links moves
   its Rust engine to `seed/`, #1102), convert or keep its Rust tests
   (lane 19), keep `flip-gate.json` working against the last Rust commit or
   the seed, and add the gate's row to the scoreboard below and the crate's
   STATUS in `.aspect/rust-census.axl`.

## Where Rust can be deleted, and when

- **Libraries outside the compiler's closure** (Warp, Prism, Quarry, Flux,
  Ignis, Grammar, Bench, Aegis, Chorus, ...) are deleted as soon as they pass
  the gate.
- **Libraries the compiler links** (alloc, tensor, serde, ebpf, silicon,
  kinetic, resonance, crucible, semver, loom-calculus, beacon, pact,
  simpulse) cannot lose their Rust while the Rust compiler links it. They flip
  to the seed pattern (#1102). SH6 lane 3 moves tool-only uses out of the
  compiler binary so most of them can be deleted early; the rest go at the
  retirement.
- **The compiler** stays Rust until Stage 2. Each module is swapped into the
  hybrid build by its SH2-SH5 lane. When every compiler row of the census
  passes the gate (the diverse `*42` oracles are exempt; they move to their
  own crate first, with the tests that import them), the hybrid with every swap becomes the default compiler,
  the Vesper compiler rebuilds itself bit for bit (Stage 2), and one
  retirement PR deletes `src/`, the compiler's crates, the seeds, the hybrid
  and the port harness crates.

## The retirement checklist

SH6 lane 2 (#1397) owns this section. The Rust compiler (`src/`, its own
crates, the frozen seeds, the hybrid and the port harness, about 238k
non-test lines) goes in **one** retirement PR, and that PR merges only when
every box below is ticked. Each box names how it is checked.

### The default switch

The hybrid installs its swaps in one of three sets
(`crates/vesper-hybrid/src/swaps.rs`, `VESPER_HYBRID_SWAPS`; each swap's
flip-gate row is in its own file, `crates/vesper-hybrid/src/swap/<file>.rs`,
#1897):

| Set | What runs from Vesper | Use |
|---|---|---|
| `all` (default) | every swap | the port lanes' differentials and benchmarks |
| `gated` (`VESPER_HYBRID_GATED=1` too) | only the swaps whose flip gate passed; every other module stays on its Rust path | the set the default `vesper` switches to |
| `none` | no swap: the Rust compiler behind the Vesper driver | checking the driver alone |

A swap joins `gated` in the PR that passes its gate: that PR adds the gate
configuration (`scripts/ci/flip-gate/<name>.json`), the scoreboard row
below, and changes the swap's row in its `src/swap/` file to `pass(...)`.
`crates/vesper-hybrid/tests/swaps.rs` checks that every `gated` swap has
both and that the others really stay on Rust, and
`gated_hybrid_matches_the_default_compiler` (tests/differential.rs) runs the
gated set over the whole corpus with 0 differences. Today `gated` holds
every swap whose row in its `src/swap/` file is `pass(...)`, one PASS
row of the scoreboard below per gate configuration. A hybrid started as `vesper` (Bazel's
`//:vesper_shipped`, a copy of the hybrid under the shipped
name) or built with the `gated-default` feature runs `gated` when no
variable names a set: that is the compiler the default `vesper` switches
to.

### Before the retirement PR

1. **Every compiler row passes the gate.** Every module of the census's
   root-compiler section and every crate of class `compiler` has a PASS row
   in the scoreboard (0 differences, ABBA upper end at or below 1.5x, its
   tests pass against the Vesper build). Checked by: `gated` equals `all`
   in `src/swap/` (no `open(...)` row left), and no module of the census is
   still answered by Rust in the default compiler. Open today, largest
   first: the native walk and the CLIF passes (`native`, `native_clif_opt`,
   `native_cold_outline`, `native_vectorize`; #1509, #1829); the front-end
   modules not yet swapped (`check`, `trace_effects`, `dyn_*`, `regions`,
   `pact_frontend`, `forge_frontend`); `loops` and `atomics` (0.69x,
   not yet swapped); `lsp`, `tooling`, `database`
   (#1828); `test_runner`, `anvil_*`, `doctest`; `lean*`,
   `verify_commands`, `proof_commands` (#1341); `ffi_lowering`, `tiled`,
   `vector`, `native_vector` (#1830); `native_session`, `native_aggregates`;
   `seq`; and every swap that has a differential but no gate configuration
   yet (each `open(...)` row of `src/swap/` says which).
2. **The driver runs no Rust pass.** `HybridDriver::hd_step`
   (`crates/vesper-hybrid/src/driver.rs`) is gone: package loading,
   checking and artifact emission run in Vesper, so the Vesper compiler is
   a whole program and not a set of passes the Rust session calls. Checked
   by: `modules/driver` imports no `HybridDriver` function, and the driver
   differential (`crates/vesper-hybrid/tests/driver.rs`) passes.
3. **Stage 2 is reproducible.** The complete compiler, built by itself to
   wasm and run by the pinned Wasmtime v50.0.1 (`@wasmtime//:wasmtime`,
   `bootstrap/stage2.sh`), reaches the fixed point stage2 == stage3 bit for
   bit, and `bootstrap/stage2.lock` records the digests. Checked by:
   `bootstrap/stage2.sh verify` and `.github/workflows/bootstrap-stage2.yml`
   (manual dispatch and nightly). Today `@vesper/stage2` rebuilds itself
   bit for bit (#1690). Since #2671 the gate (`crates/vesper-stage2-gate`)
   boots from the committed seed `bootstrap/stage2-seed/stage2.wasm` with no
   Rust compiler, and every compiler package it builds (listed in
   `COMPILER_PACKAGES` in `crates/vesper-stage2-gate/tests/support/packages.rs`)
   comes out byte-identical from stage A (built by the seed), from stage B
   (built by itself, when the seed is stale) and from the precompiled last
   stage under the pinned wasmtime CLI (#1928). Four packages are
   gaps, listed in `STAGE2_PACKAGE_GAPS` with the reason: the native
   Cranelift glue (`codegen-base`, `codegen-control-emit`,
   `codegen-numeric`: they assemble and check, since the Stage 2 loader
   reads each package's `[package] vsys` types, but they drive the native
   backend over `vesper-cranelift-sys`, and Stage 2 emits only Wasm, where
   the Cranelift imports and `Ffi[Cranelift]` entry points have no host) and `driver-commands`, whose printing, help and
   formatting go through the `HybridCommands` import that only the hybrid's
   Rust implements (item 2). `@vesper-std/process` builds for WASI
   (`src-wasip2`: every start is refused with Unsupported, since WASI 0.2
   cannot start a process), and the Wasm backend takes `Ffi[StdProcess]`
   as a host capability that imports nothing. The
   test fails when a gap closes or fails before the Wasm backend, and `stage2_fixed_point_lists_every_package`
   fails when a package under `packages/` is in no list. What is not yet
   covered is the whole compiler as one program, which waits for item 2.
   `bootstrap/seed/runner/vwasm.c` is frozen (owner decision): it is not a
   Stage 2 runner.
4. **The seed rebuilds the compiler with no Rust.** `bootstrap/seed/vesper.wasm`
   is committed with the script that regenerates it, and a CI job with no
   Rust toolchain builds the compiler from the seed and gets the bytes of
   `bootstrap/stage2.lock`. `bootstrap/stage1.lock` still reproduces, from
   the seed instead of Rust.
5. **The kept Rust stands alone.** The Rust that stays builds without the
   Rust compiler:
   - `crates/vesper-oracles` no longer depends on the root `vesper` crate
     (#1929): the arithmetic oracle's worker drives the `vesper` binary as a
     process (`--vesper PATH`), with `--no-ast-opt`/`--no-ir-opt` for its
     four optimizer settings, so it tests the Vesper compiler unchanged.
   - `vesper-codegen-verified` no longer links the frozen seeds of
     `vesper-loom-calculus`, `vesper-loom-cas` and `vesper-loom-trust`
     (#1929): `src/trust.rs` is its own copy of the attestation rules,
     pinned to the seeds' behaviour by `tests/trust_vectors.rs`. It must not
     call the Vesper libraries instead, which the compiler it audits builds.
   - `src/verified_codegen` (the Stage 1 guard) is in `src/`. Owner
     decision (2026-11-25): it is ported to Vesper (#1886,
     `@vesper/verified-codegen`), and `bootstrap/stage1.rs` uses the port
     with `bootstrap/stage1.lock` reproduced.
   - `vesper-cranelift-sys` and `vesper-crypto-sys` link with no Rust
     compiler crate.
6. **No test is lost.** Every test under `tests/` and `crates/*/tests` that
   drives the Rust compiler runs against the Vesper compiler or is converted
   to a Vesper test (lane 19's recipe). None is deleted without a
   replacement.
7. **All platforms.** Linux x86_64, macOS (AArch64) and Windows (x86_64
   MSVC) build and pass the test suite with the Vesper compiler as the
   default, and every required check is green on the retirement branch.

### The retirement PR

- Makes the Vesper compiler (built from the seed, through Stage 2) the
  default `vesper` for Cargo, Bazel and the release workflows.
- Deletes `src/` (with `codegen_swap*`, `ir_swap*`, `swap_input*` and
  `compiler_modules*`), the crates of class `compiler` in the census, every
  frozen `crates/*/seed` (27 today), `crates/vesper-hybrid`, the port
  harness crates (`vesper-port-abi`, `vesper-port-build`,
  `vesper-port-harness`, `vesper-port-pilot`) and the codegen test crates
  (`vesper-codegen-swap-test`, `vesper-codegen-numeric-test`).
- Keeps `vesper-cranelift-sys`, `vesper-crypto-sys`,
  `vesper-codegen-verified` and `crates/vesper-oracles`.
- Removes the deleted crates from the root `Cargo.toml` (in its own commit,
  with `Cargo.lock` regenerated by cargo) and runs
  `tools/bazel/sync_build_files.py`.
- Sets the deleted rows to `gone` in the STATUS tables of
  `.aspect/rust-census.axl` (never the generated census), adds its row to
  the scoreboard, and reports the Rust lines deleted in the PR body and in
  `docs/parallel-results/rust-exit-stage2-retirement.md`.
- Closes #1397.

## Bootstrapping without Rust

**Owner, 2026-11-21: a committed wasm seed, Zig-style.** The compiler, built
by its own wasm backend, is committed under `bootstrap/seed/` together with a
small wasm runner written in C (or the seed's wasm2c output). A machine with a
C compiler builds the runner, runs the seed to build stage 1 from source,
and stage 1 builds stage 2. A script regenerates the seed from the current
compiler, and CI checks that the seed rebuilds the compiler to the same
bytes. The Stage 1 lock (`bootstrap/stage1.lock`) and the verified backend's
digests stay as they are. SH6 lane 2 owns this.

## The irreducible minimum

What the plan expects to be left, and why it cannot go:

| Rust left | About | Why |
|---|--:|---|
| `vesper-cranelift-sys` | 2.4k | Cranelift is a Rust library with no C API. Owner decision 2026-11-19: Stage 2 keeps Cranelift behind this shim. It goes only if SH7's Vesper-native backend reaches Cranelift's level. |
| `vesper-crypto-sys` (SH6 lane 17) | about 1-2k | Vetted crypto and TLS (RustCrypto, libcrux, dalek, rustls) are never hand-rolled. Each entry is a direct call into the library. An owner call could replace them with a C library (BoringSSL, aws-lc), which would remove this shim too. |
| A naga shim, if lane 15 cannot precompile shaders | a few functions | WGSL to SPIR-V translation exists only in Rust. |

Kept Rust on purpose (owner, 2026-11-21: "keep oracles in rust"):

| Rust left | Lines | Why |
|---|--:|---|
| `vesper-codegen-verified` (the DDC auditor) | 1.4k | Diverse double-compiling needs a second, independent implementation. Rewriting it in Vesper would compile it with the compiler it audits. |
| `crates/vesper-oracles` (the `*42` arithmetic, resource, Lean and lexer oracles, formerly `src/bin`) | 12.7k | The same reason: diverse test oracles. |

Everything else talks to C or the OS (libc, syscalls, io_uring, KVM,
perf_event, AF_XDP, GTK, Vulkan, wasmtime's C API, BLAKE3's C library),
which Vesper reaches through `.vsys` with no Rust in between, or does work
Vesper can do itself.

## Waves

| Wave | What | Claims |
|---|---|---|
| SH2-SH5 (running) | The compiler's modules ported and swapped into the hybrid | their own claims |
| **SH6** | Flip gate and census; Stage 2, the wasm seed and the retirement; the compiler's closure; finish and flip every ported library; the OS and C-API crates on `.vsys`; one crypto shim; Rust-only libraries replaced; Rust tests to Vesper | #1396-#1414 ([plan](../agent-prompts/self-host-sh6/README.md)) |
| SH6b | The verified backend widened to the compiler (DDC check of §10.2) | after SH6 lane 2 |
| SH7 (optional) | Vesper-native backend; replaces Cranelift only at its level | after SH6 |
| Gap wave 1 | The code-generation and interop gaps the SH6 reports name (Option results, cold inlining, vectors, export calls, slices, platform interop, foreign memory), the wasm seed's breadth, the native walk, and the unowned rows | #1498-#1506, #1508-#1510 ([plan](../agent-prompts/rust-exit-gaps-1/README.md)) |
| Later gap waves | Planned from each wave's reports | as they come |

## Scoreboard

SH6 lane 1 keeps this section: one row per flip, with its gate verdict and
the Rust lines deleted.

| Flip | Lane | Differential | Speed (ABBA) | Rust deleted | PR |
|---|---|---|---|--:|---|
| `bounds-hoist` (PASS; compiler module, its 2,017 lines go at the retirement) | SH3-12 | 0 differences on 325 cases; tests 2/2 pass | pass 1.15x [1.06x, 1.22x] n=12; build 1.07x [1.01x, 1.09x] n=30 | 0 | #1251 part 4 |
| `optimize` (PASS, compiler module) | G1-4 | 0 differences; tests 2/2 pass | checker 1.12x [1.00x, 1.19x] n=12; producer 0.70x [0.64x, 0.74x] n=12; pass 0.79x [0.70x, 0.83x] n=12 | 0 (2,867 at the retirement) | #1501 |
| `dyn` (PASS; compiler module, `src/dyn_fn.rs` and `src/dyn_trait.rs` go at the retirement) | FW-4 | 0 differences; tests 1/1 pass | used 1.13x [1.08x, 1.15x] n=12; forced 0.82x [0.80x, 0.86x] n=12 | 0 (2,313 at the retirement) | #2207 |
| `effects` (PASS; compiler module, `src/capability_effects.rs` goes at the retirement) | SH2-8 | 0 differences; tests 1/1 pass | effects-with-encoding 0.32x [0.31x, 0.33x] n=12 | 0 (2,535 at the retirement) | #2207 |
| `ebpf` (PASS; compiler module, `src/ebpf_backend*` goes at the retirement) | SH4-16 | 0 differences; tests 1/1 pass | ebpf-with-encoding 1.09x [1.03x, 1.16x] n=12 | 0 (2,336 at the retirement) | #2207 |
| `doc` (PASS; compiler module, the renderers of `crates/vesper-doc` go at the retirement) | SH5-15 | 0 differences; tests 1/1 pass | render 0.94x [0.93x, 0.97x] n=12 | 0 (1,969 at the retirement) | #2207 |
| `tensor-check` (PASS; compiler module: the swap answers the diagnostics, while `analyze` for an accepted module and `wire::check` still run in Rust; `src/tensor_types` goes at the retirement) | SH2-14 | 0 differences on 1,518 cases; tests 1/1 pass | all 0.99x [0.96x, 1.08x] n=12; tensor 0.88x [0.81x, 0.91x] n=12 | 0 (5,084 at the retirement) | #2289 |
| `ir-check` (PASS; compiler module, `src/ir_check.rs` goes at the retirement) | SH5-10 | 0 differences over every corpus world and its mutants; tests 1/1 pass | ir-check-per-call-encode-vesper-decode 0.80x [0.77x, 0.82x] n=12 | 0 (1,210 at the retirement) | #2289 |
| `ir-opt` (PASS; compiler module: the swap answers `ir_opt_policy::optimize`, the default `opt` path, with Rust's policy where the package refuses; `src/ir_opt.rs` and `src/ir_opt_policy.rs` go at the retirement) | SH3-4 | 0 differences over every corpus world and its mutants; tests 1/1 pass | ir-opt-swap 0.88x [0.85x, 0.91x] n=12 | 0 (1,475 at the retirement) | #2289 |
| `wit-component` (PASS; compiler module, `src/wasm_backend/component.rs` and the wit-component dependency go at the retirement) | SH5-11 | 0 differences on 1,383 cases; tests 1/1 pass | wrap 0.05x [0.05x, 0.05x] n=12; build 0.67x [0.65x, 0.68x] n=12 | 0 (933 at the retirement) | #1334 flip |
| `vesper-aether` virtqueue walks (PASS) | SH6-14 | 0 differences; tests 2/2 pass | virtqueue 1.44x [1.36x, 1.47x] n=30 | 331 (now the tests' reference) | #1494 |
| `native-walk` (PASS; compiler module, the walk's planning in `src/native.rs` and `src/native_export.rs` goes at the retirement) | G1-11 | 0 differences on 978 cases; tests 1/1 pass | entries 1.00x [0.98x, 1.03x] n=12; exports 0.99x [0.93x, 1.03x] n=12 | 0 | #2056 |
| `doctest-extract` (PASS; compiler module, `src/doctest_extract.rs` goes at the retirement) | SH5-14 | 0 differences; tests 1/1 pass | extract 1.04x [1.02x, 1.07x] n=12 | 0 (310 at the retirement) | #2207 |
| `doctest-guard` (PASS; compiler module, `src/doctest_guard.rs` goes at the retirement) | SH5-14 | 0 differences; tests 1/1 pass | guard 0.48x [0.45x, 0.49x] n=12 | 0 (279 at the retirement) | #2207 |
| `doctest-generate` (PASS; compiler module, `src/doctest_generate.rs` goes at the retirement) | SH5-14 | 0 differences; tests 1/1 pass | generate 0.75x [0.74x, 0.77x] n=12 | 0 (131 at the retirement) | #2207 |
| `doctest-verdict` (PASS; compiler module, `src/doctest_verdict.rs` goes at the retirement) | SH5-14 | 0 differences; tests 1/1 pass | verdict 1.21x [1.20x, 1.22x] n=12 | 0 (221 at the retirement) | #2207 |
| `test-mutants` (PASS; compiler module, `src/test_mutants.rs` goes at the retirement) | SH5-14 | 0 differences; tests 1/1 pass | sites 0.53x [0.53x, 0.54x] n=12 | 0 (283 at the retirement) | #2207 |
| `sugar` (PASS; compiler module, `crates/vesper-syntax/src/sugar_expand*.rs` goes at the retirement) | SH1-14 | 0 differences on 3,872 cases; tests 1/1 pass | expand 1.33x [1.28x, 1.36x] n=12 | 0 (3,282 at the retirement) | #1157 part 7 |
| `verified-codegen` (PASS; compiler module, `src/verified_codegen` goes at the retirement) | SH6-2 | 0 differences on 4,724 cases; tests 1/1 pass | compile 1.02x [0.97x, 1.05x] n=12 | 0 (9,760 at the retirement) | #1886 |
| `stack-bound` (PASS; compiler module, `src/stack_bound.rs` goes at the retirement) | SH2-10 | 0 differences; tests 6/6 pass | hybrid path 1.20x [1.18x, 1.26x] n=12 | 0 (at the retirement) | #2359 |
| `parse` (PASS; compiler module, `crates/vesper-syntax/src/parser.rs` goes at the retirement) | SH1-12 | 0 differences; tests pass | parse 0.23x n=12 | 0 (at the retirement) | #2389 |
| `test-discovery` (PASS; compiler module, `src/test_discovery.rs` goes at the retirement) | SH5-14 | 0 differences; tests pass | test-discovery 1.26x n=12 | 0 (at the retirement) | #2453 |
| `run` (PASS; compiler module, `src/runtime.rs` and `src/regions/eval.rs` go at the retirement) | #2053 | 0 differences on 1,173 cases; tests 1/1 pass | run 0.53x [0.51x, 0.54x] n=12 | 0 (4,037 at the retirement) | #2053 |
| `freestanding` (PASS; compiler module, `src/native_freestanding.rs` goes at the retirement) | SH4-14 | 0 differences; tests pass | freestanding-link 1.23x n=12 | 0 (at the retirement) | #2517 |
| `pact-declarations` (PASS; compiler module, `src/pact_frontend.rs` goes at the retirement) | SH2-11 | 0 differences; tests pass | declarations-swap-point-with-encode-and-decode 0.02x n=12 | 0 (at the retirement) | #2517 |
| `codegen-calls` (PASS; compiler module, `src/native.rs` goes at the retirement) | SH4-5 | 0 differences; tests pass | discovery-entry 0.06x n=12 | 0 (at the retirement) | #2572 |
| `block-plan` (PASS; compiler module, `src/native.rs` goes at the retirement) | SH4-11 | 0 differences; tests pass | block-plans 0.87x n=12 | 0 (at the retirement) | #2617 |
| `ffi-lowering` (PASS; compiler module, `src/ffi_lowering.rs` goes at the retirement) | SH4-10 | 0 differences; tests pass | ffi-type-mapping 0.33x n=12 | 0 (at the retirement) | #2617 |
| `codegen-export` (PASS; compiler module, `src/native_export.rs` goes at the retirement) | SH4-12 | 0 differences; tests pass | binding-generator-with-encode-and-unpack 1.45x n=12 | 0 (at the retirement) | #2647 |
