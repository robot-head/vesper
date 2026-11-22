# Rust-exit plan

> Written 2026-11-21 against `main` at `af7dd900` (claims #1396-#1414). The
> [manuscript](../manuscript/README.md) is the sole specification. The
> numbers per crate and per compiler module are in the generated
> [census](rust-census.md) (`python3 scripts/rust-census.py`). The compiler
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

`scripts/ci/flip_gate.py` (SH6 lane 1) checks 1 and 2 and prints the row for
the census. A lane that passes flips without asking the owner; the recipe is
below.

## Flipping a module

Every flip PR runs `scripts/ci/flip_gate.py` and pastes its two output lines
(the verdict and the scoreboard row) into the PR body. A lane that gets PASS
flips and deletes without asking the owner.

1. **Write the gate's configuration** at `crates/<crate>/flip-gate.toml` (a
   compiler module: `scripts/ci/flip-gate/<module>.toml`). It lists the build
   commands, every test command (the crate's tests against the Vesper build,
   and the seed's), the differential, one `[[abba]]` table per workload and
   the Rust paths the flip deletes (`rust = [...]`, counted for the
   scoreboard). The format is in the script's header;
   `scripts/ci/flip-gate/vesper-span.toml` is a worked example.
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
4. **Run it:** `VESPER_PORT_COMPILER=<prebuilt vesper> python3
   scripts/ci/flip_gate.py <crate>`. Exit 0 is PASS (every test and the
   differential pass, and every workload's 95% interval has its upper end at
   or below 1.5x), 1 is FAIL (a failing test, a difference, or a lower end
   above 1.5x), 2 is INCONCLUSIVE (an interval crosses 1.5x: rerun on a
   quieter host or with more epochs, `--epochs 60`), 3 a configuration
   error. The interval is vesper-bench's exact binomial median interval
   (`analyze_pairs`), computed on exact ratios.
5. **Flip.** On PASS: make Vesper the only build (a crate outside the
   compiler's closure deletes its Rust; a crate the compiler links moves
   its Rust engine to `seed/`, #1102), convert or keep its Rust tests
   (lane 19), keep `flip-gate.toml` working against the last Rust commit or
   the seed, and add the gate's row to the scoreboard below and the crate's
   STATUS in `scripts/rust-census.py`.

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
| `src/bin/*42` (arithmetic, resource, Lean and lexer oracles) | 10.8k | The same reason: diverse test oracles. |

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
| Gap waves | Compiler gaps the SH6 lanes report, planned by lane 1 from the reports | as they come |

## Scoreboard

SH6 lane 1 keeps this section: one row per flip, with its gate verdict and
the Rust lines deleted.

| Flip | Lane | Differential | Speed (ABBA) | Rust deleted | PR |
|---|---|---|---|--:|---|
| | | | | | |
