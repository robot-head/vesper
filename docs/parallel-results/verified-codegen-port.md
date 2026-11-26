# The verified direct code generator in Vesper (#1886)

Owner decision (2026-11-25): port `src/verified_codegen` (about 9.3k lines)
to Vesper, so that the Stage 2 retirement can delete it (checklist item 5 in
`docs/maintenance/rust-exit-plan.md`). The package is
`packages/vesper-verified-codegen` (`@vesper/verified-codegen`).

## Part 1: the slot IR and instruction selection

Base: `main` after #1884.

### What changed

- **`src/machine.ves`**: `machine.rs` in Vesper. It has the slot IR rows
  (`Lir`) and the abstract instructions (`M`) as copy structs of small
  codes. It also has `select` (one fixed `M` sequence per row), `M`'s
  display text and `table_listing`. Every row kind has its Lean counterpart
  in `lean/VerifiedCodegen.lean`. No Lean file changed.
- **`src/lib.ves`**: the harness entries `table_listing_text` and
  `select_text`. The second takes a row count and then four words per row.
- **`tests/self_host_verified_codegen.rs`**:
  - The Vesper table listing equals `verified_codegen::table_listing()`
    byte for byte. `tests/verified_codegen_lean.rs` already checks that
    listing against the Lean table.
  - `select` gives the same text on the table rows, on 20,000 random rows
    over every field's range, and on every slot-IR row that the Rust
    backend lowers from the corpus `main`s.

### Evidence (Linux x86_64, release)

| Check | Result |
|---|---|
| Table listing | identical (97 rows) |
| Random rows | 0 of 20,000 differ |
| Corpus | **0 of 12,488 rows differ** over 107 programs |
| ABBA, `select` + display over the corpus rows ×20 (Vesper includes the word encoding and text unpacking) | Vesper/Rust per epoch 0.82x-0.96x, median 0.91x, n=12 |

**Rust lines deleted:** 0. `machine.rs` (668 lines) goes when the Rust
backend's callers use the port (part 4).

**Stage 1 guard:** nothing under `src/verified_codegen/**`, `lean/**` or
`bootstrap/stage1*` changed.

## Part 2: the assembler, the encoders and the containers

### What changed

- **`src/asm.ves`** (`asm.rs`): the label-resolving buffer. It handles
  labels, fixups (rel32, imm26, imm19, `jal`, `auipc`+`jalr`), the copies of
  the abort exits with nearest-copy resolution, trap scopes and the Windows
  prologue and external records.
- **`src/x86_64.ves`**, **`src/aarch64.ves`**, **`src/riscv64.ves`**: `emit`
  for every `M`, plus `start`/`start_windows` and the between-function
  `exits` copies (the 256 KiB `EXIT_SPACING`).
- **`src/container.ves`** (`container.rs`): the static Linux ELF executable
  (with the heap segment when regions use it), the Mach-O AArch64 object,
  and the x86-64 COFF object with `.xdata` `UNWIND_INFO`, `.pdata` and its
  relocations.
- **`src/lib.ves`**: `build_words`, the back half of
  `verified_codegen::compile`. The input is the target, the protocol, the
  entry and every function's slot-IR rows; the output is the container's
  bytes, or the Rust error kind.
- **`tests/self_host_verified_codegen.rs`**: every corpus `main` the Rust
  backend builds, under both result protocols, goes through `build_words`
  from the Rust backend's own slot IR, and the bytes must be identical.

### Evidence (Linux x86_64 host, release)

| Target | Container | Differ |
|---|---|---|
| x86_64-unknown-linux-gnu | ELF executable | **0 of 214** |
| aarch64-unknown-linux-gnu | ELF executable | **0 of 214** |
| riscv64gc-unknown-linux-gnu | ELF executable | **0 of 214** |
| aarch64-apple-darwin | Mach-O object | **0 of 196** |
| x86_64-pc-windows-msvc | COFF object | **0 of 196** |

The macOS and Windows counts are lower because programs that need the
program heap are a Linux-only target error in both builds. No speed is
measured for part 2 yet. The ABBA comes with the swap (part 4), when the
whole `compile` is compared.

**Rust lines deleted:** 0. `asm.rs`, the three encoders and `container.rs`
(1,923 lines) go with the rest at the retirement.

**Stage 1 guard:** nothing under `src/verified_codegen/**`, `lean/**` or
`bootstrap/stage1*` changed. The package is left out of the Stage 1 parser
corpus (`tests/fixtures/self_host/driver.rs`), like the other ports that use
`use` and `pub const`.

## Part 3: the range certificate

### What changed

- **`src/certificate.ves`** (`certificate.rs`, 1,068 lines). It contains:
  - the Lean mirror (`supports`, `entails`, `checkRow`, `transfer`,
    `checkConst`, `checkNode`);
  - the trap-scope targets;
  - the production `validate`, with every rule and rejection text of the Rust
    validator, including the `{row:?}` Debug form of a row;
  - the untrusted `generate`: liveness, the interprocedural forward range
    analysis with entry and exit summaries, and the signed-constant flips;
  - `check` (widen, generate, validate).

  Facts, origins and location sets are sorted word sequences. The analysis
  visits rows in the Rust order, so the certificates are equal word for
  word. Label maps use `@vesper-std/collections`' `FlatTable`, a new
  dependency of the package.
- **`src/lib.ves`**: the harness entries `certificate_text` (a program in,
  `check` out) and `validate_text` (a program and a certificate in, the
  verdict out).
- **`tests/self_host_verified_codegen.rs`**, three differentials and one
  timing test:
  - **Corpus:** every corpus program the Rust backend lowers.
  - **Random programs:** 4,000 programs shaped like the lowering's output,
    with malformed shapes now and then.
  - **Mutated certificates:** 9,642 certificates mutated from the generated
    ones (facts dropped or changed, duplicate facts, signed constants, post
    facts, wrong annotation or function counts, entry preconditions,
    retargeted jumps and calls). Each is compared through `validate`.
  - **Timing:** the ABBA `certificate_flip_gate`.

  The Lean `validateProgram` is covered transitively. `lean_check_node` is
  a line-by-line port of the Rust mirror, which `tests/verified_codegen_lean.rs`
  checks against Lean `#eval`.

### Evidence (Linux x86_64 host, release)

| Check | Result |
|---|---|
| Corpus programs | **0 of 107 differ** (86 accepted, 21 rejected) |
| Random programs | **0 of 4,000 differ** (154 accepted) |
| Mutated certificates through `validate` | **0 of 9,642 differ** (9,498 rejected) |
| ABBA, `check` over the corpus ×5, best of 6 (Vesper includes the word encoding and text unpacking) | rust 448 ms, vesper 418 ms: **0.93x** |

**Rust lines deleted:** 0. `certificate.rs` goes with the rest at the
retirement. Its `optimize::certificate` namesake, the Place IR rewrite
proposals, is a different module and is not part of this port.

**Stage 1 guard:** unchanged, as in parts 1-2.

## Part 4a: the plan and the lowering

### What changed

- `packages/vesper-verified-codegen/src/lower.ves` ports `lower::plan` and
  `lower::function` of `src/verified_codegen/lower.rs`, together with the
  failure codes and raise sets of `fail.rs`. It covers:
  - shapes and variant layouts;
  - the status protocol, `?`, `Fail::raise` and implicit trap stubs;
  - pattern binding and `match` dispatch;
  - tuple indexing and tail self calls.

  Every error has the Rust `VerifiedError` text.
- It reads the world and the Place IR that `compile` lowers, after ir_opt,
  monomorphization and evidence passing. The input is encoded by
  `tests/common/ir_words.rs`, then a short extra section: the entry, the
  limits, whether the region prelude is active and the world's function
  names. Its output is every planned function's rows in the Rust `Debug`
  form.
- `src/verified_codegen.rs` gains `#[doc(hidden)]` test hooks:
  - `lowering_for_test`: the lowered program and the Rust result.
  - `lower_for_test`: the lowering alone, for timing.

  `compile` is split into `prepare` and `lower_program`, which do the same
  steps in the same order. The behaviour does not change: the Stage 1 lock
  hashes artifacts, not source.
- Not ported yet (part 4c): `handle` expressions (non-resuming `Fail`
  handlers and region blocks) and region operations. For a program that
  has one, the package answers `not ported` and the test counts it apart.

### Evidence (Linux x86_64 host, release)

| Check | Result |
|---|---|
| Corpus programs without handler sites, optimized and not | **0 of 222 differ** (150 lowered, 72 rejected with the same error text; 82 not ported) |
| ABBA, plan + lowering + text over the corpus ×5 (Vesper includes the decode) | rust 22.4 ms, vesper 23.9 ms: **1.07x** |

**Rust lines deleted:** 0. `lower.rs` and `fail.rs` go at the retirement.

## Part 4b: monomorphization

### What changed

- `packages/vesper-verified-codegen/src/mono.ves` ports
  `src/verified_codegen/mono.rs`. Starting from the entry, it collects the
  reachable instances `(function, type arguments)` breadth first. Each
  instance gets a copy of its function with every type substituted
  (parameters, result, effect arguments, values, locals, instruction and
  call types) and every direct callee renamed to the callee's instance,
  `name<T=i64,...>`. The instance limit and the `Unsupported::Generic`
  errors have the Rust text.
- An instance's copy shares the input's operand words wherever nothing
  changes (block headers, terminators, the fip trailer). Only substituted
  headers, call types and renamed calls get new words.
- The input is the world and the Place IR after ir_opt
  (`tests/common/ir_words.rs`), then an extra section: the entry, the
  instance limit, the generics of the program's functions and the
  `World::calls` entries of its calls, by name and type index. The output
  is the program's `put_program` words, which the test decodes with
  `ir_words::decode_program` and compares by the canonical dump.
- `front_text` runs monomorphization and then the part 4a lowering in one
  pass over one input, as `compile` will once evidence passing is ported.
  The test compares it with Rust's monomorphization, evidence passing and
  lowering on the programs that evidence passing leaves unchanged.
- `src/verified_codegen.rs` gains `#[doc(hidden)]` hooks `mono_for_test`,
  `front_for_test` and `front_timing_for_test`. `prepare` now calls the
  shared `optimized` step; the steps and their order are unchanged.
- Not ported yet (part 4c): `handle` expressions, whose nested handler
  bodies are named by AST spans. For a program with one, the package
  answers `not ported`.

### Evidence (Linux x86_64 host, release)

| Check | Result |
|---|---|
| Monomorphization, corpus programs, optimized and not | **0 of 222 differ** (42 with generic instances or `Generic` errors; 142 not ported) |
| Monomorphization and lowering in one pass, same corpus | **0 of 212 differ** (144 lowered; 126 not ported) |
| ABBA, front half over the corpus ×5: Rust mono + evidence + plan + lowering + text, Vesper decode + mono + plan + lowering + text | rust 32.9 ms, vesper 25.0 ms: **0.76x** |
| ABBA, monomorphization alone (Vesper includes the decode and the output words) | rust 10.5 ms, vesper 16.3 ms: 1.55x |

Monomorphization alone is over the bar because reading and writing the
IR words costs about as much as the whole Rust pass: an identity round
trip of the same inputs takes 10-13 ms. That cost is the crossing, which
Stage 2 removes, so the gate that decides the swap is the front half,
where the pass runs on the program the lowering reads.

**Rust lines deleted:** 0. `mono.rs` goes at the retirement.

## Part 4c-1: `handle Fail` handlers

### What changed

- `packages/vesper-verified-codegen/src/exprs.ves` reads the expressions of
  the Place IR's payload pool that the backend needs: closures and `handle`
  expressions, with their spans, free names, effect, state flag and clause
  patterns. `tests/self_host_verified_codegen.rs` `expr_rows` writes them
  right after the program section; the AST stays on the Rust side.
- `mono.ves` instantiates a handler's nested functions (body, `return`
  clause, operation clauses) with the parent's arguments.
- `lower.ves` ports `fail::handler` and the non-resuming `handle Fail[E]`
  lowering: the handled types join the failure codes, the plan resolves
  each handler and adds its nested functions, the raise-set fixpoint
  follows handlers, and a body call jumps to the `raise` clause on the
  handled code. `assigned_capture` and the continuation check (`reads`,
  over the free names of nested closures and handlers) have the Rust text.
- Not ported yet: region blocks and region operations (4c-2), and evidence
  passing (4c-3). For a program with one, the package answers `not ported`.

### Evidence (Linux x86_64 host, release, `-C target-cpu=native`)

| Check | Result |
|---|---|
| Lowering, corpus programs without handler sites, optimized and not | **0 of 278 differ** (186 lowered; 26 not ported, down from 82) |
| Monomorphization, same corpus | **0 of 364 differ** (none left unported) |
| Monomorphization and lowering in one pass | **0 of 270 differ** (178 lowered; 26 not ported) |
| ABBA, lowering ×5 | rust 26.5 ms, vesper 30.3 ms: **1.14x** |
| ABBA, front half ×5 | rust 51.1 ms, vesper 36.2 ms: **0.71x** |

**Rust lines deleted:** 0. `fail.rs` goes at the retirement.

## Part 4c-2: region blocks and region operations

### What changed

- `lower.ves` ports region capabilities: the plan gives each function its
  row's `r: Region` labels, a region block's body the enclosing ones plus
  its own (innermost last), and refuses a capability on the entry with the
  Rust text. A call passes one header pointer word per capability after
  its arguments; the prologue loads them.
- Region blocks open a header on the program heap (bump allocation,
  bounded by `HEAP_WORDS`), call the body like a nested function and close
  the block with the pin rule. A failure that leaves the body closes the
  open blocks, innermost first, before it propagates.
- `Region::box`, `get`, `set` and `used` follow the Rust rows, including
  the `vesper.region.accounting/1` bound checks. The payload size comes from
  `@vesper/check-layout` (`Layout::of`).
- Nothing in the corpus is answered `not ported` any more. Still left: evidence
  passing (4c-3), which the front test skips by comparing only programs that
  evidence leaves unchanged.

### Evidence (Linux x86_64 host, release, `-C target-cpu=native`)

| Check | Result |
|---|---|
| Lowering, corpus programs without handler sites, optimized and not | **0 of 304 differ** (204 lowered; none not ported, down from 26) |
| Monomorphization and lowering in one pass | **0 of 296 differ** (196 lowered; none not ported) |
| ABBA, lowering ×5 | rust 37.9 ms, vesper 37.9 ms: **1.00x** |
| ABBA, front half ×5 | rust 64.0 ms, vesper 43.5 ms: **0.68x** |

**Rust lines deleted:** 0. `lower.rs` goes at the retirement.

## Part 4c-3: evidence passing

### What changed

- `packages/vesper-verified-codegen/src/evidence.ves` ports
  `evidence::rewrite` on the monomorphized program. It runs function-value
  specialization (`specialize_function_arguments`, `substitute_tree`) and
  `prune`. Tail-resumptive handlers of user effects go through `site`,
  `tail_resumptive` and `specialize`: clause captures become `$ev<n>.<name>`
  evidence parameters, operations become clause calls, and effectful
  callees get `f<E#n>` instances. Region labels are relabelled from `$cap<start>`.
  Functions are edited by appending rows to the Place IR tables.
- `lower.ves` takes the `Sites` it returns: one escape code per code and
  site tag (`Fails::add_escapes`), recoded raise sets in the fixpoint, and
  `recode_status` after a call of a clause or a handler body.
- `front_text` is now monomorphization, evidence passing and lowering in
  one pass. The front test compares every corpus program, including those
  with handler sites and evidence errors. `FrontOutcome` lost its skip
  flags.

### Evidence (Linux x86_64 host, release, `-C target-cpu=native`)

| Check | Result |
|---|---|
| Monomorphization, evidence passing and lowering in one pass | **0 of 364 differ** (214 lowered; none not ported, up from 296 compared) |
| ABBA, front half ×5 | rust 90.8 ms, vesper 62.5 ms: **0.69x** |

**Rust lines deleted:** 0. `evidence.rs` goes at the retirement.

## Part 4d-1: `compile` end to end

### What changed

- `compile_words` (lib.ves) is `compile` in one Vesper call. It runs the
  front half (`front_rows`), the certificate (`certificate::certify`) and
  the back half (`back`). The back half runs the Rust checks in the Rust
  order: the heap, riscv64 off Linux, x86-64 off Linux and Windows, the
  per-row code-byte limit, `finish`, and the container.
- The lowering now fills a `Lowered` (machine.ves): rows, function starts,
  names, the entry's result words and label. `put_lowered` prints it as
  before. `build_words` reads its input into a `Lowered` and calls `back`.
- `Asm` keeps the first failure and its label (`why`, `why_label`), so
  every assembler error has its Rust text.
- `verified_codegen::compile` is split at `compile_prepared`, and
  `compile_timing_for_test` times it from the optimized program.

### Evidence (Linux x86_64 host, release, `-C target-cpu=native`)

| Check | Result |
|---|---|
| `compile` on the corpus, 5 targets × 2 protocols × optimize on/off: bytes, code size, machine instructions, result words, certificate or error text | **0 of 3,640 differ** (2,068 built) |
| ABBA, `compile` ×5 (host target) | rust 475.0 ms, vesper 490.7 ms: **1.03x** |

**Rust lines deleted:** 0. All of `src/verified_codegen` goes at the retirement.

## Part 4d-2: the `verified_codegen` hybrid swap

### What changed

- `verified_codegen::build` (container, bytes and code size) is now a swap
  point, `VerifiedBuildTable`, with Rust's `compile` as the default.
  `build_command` uses it, which covers `vesper build --codegen verified` and
  the driver's `VerifiedBuild` step.
- `crates/vesper-hybrid/modules/verified_codegen` and
  `src/swap/verified_codegen.rs` install the Vesper port. The swap encodes
  the optimized program with `tests/common/verified_words.rs`, which moved
  out of the test so the test and the hybrid share it. The Rust error text
  comes back as a `VerifiedError` through `VerifiedError::from_display`. A
  program the word format cannot hold keeps Rust's build.
  `VESPER_HYBRID_VERIFIED=0` turns the swap off.
- `crates/vesper-hybrid/tests/verified_codegen.rs` runs `vesper build
  --codegen verified` over `examples/` and `tests/fixtures/` for the three
  Linux targets, with the swap on and off. The comparison uses the hybrid
  with the swap off, not the default compiler, because the hybrid's AST
  optimizer already reassociates two of these programs differently from
  the default's (`examples/traits/shapes.ves`,
  `tests/fixtures/bounded_generics/nested.ves`). The divergent build
  (`divergent-verified-codegen`) empties every summary, and the
  differential catches it.

### Evidence (Linux x86_64, release, `-C target-cpu=native`)

| Check | Result |
|---|---|
| CLI builds, swap on vs off, 3 Linux targets: status, stdout, stderr, executable bytes | **0 of 1,605 differ** (333 built) |
| Wall time of those builds | Rust 11.54 s, Vesper 11.18 s: **0.97x** |
| Divergent build | caught (549 differ) |

**Rust lines deleted:** 0. All of `src/verified_codegen` goes at the retirement.

## Part 4d-3: the VerifiedBuild driver step

### What changed

- The Vesper driver (`crates/vesper-hybrid/modules/driver`) now builds a
  Linux ELF executable for `vesper build --codegen verified` itself. A new
  step, `VerifiedInput` (62), asks Rust for the compile input only: the
  checked world goes through the same optimizer and word encoding as the
  `verified_codegen` swap. The driver then calls
  `@vesper/verified-codegen::compile_bytes`, writes the executable,
  marks it `0o755` and prints the build line.
- Anything the step cannot take falls back to Rust's `VerifiedBuild` pass:
  dyn traits, Mach-O and COFF targets, a program the word format cannot
  hold, and a compile error (Rust's pass reports it with its own text).
- `VESPER_HYBRID_DRIVER_VERIFIED=0` turns the step off and leaves the
  `verified_codegen` swap on. `VESPER_HYBRID_VERIFIED=0` turns both off.
- The hybrid statistics report "driver built N verified executables in
  Vesper (M fell back to Rust)".

### Evidence (Linux x86_64, release, `-C target-cpu=native`)

| Check | Result |
|---|---|
| CLI builds, driver step on vs both swaps off, 3 Linux targets: status, stdout, stderr, executable bytes | **0 of 1,605 differ** (333 built) |
| Wall time of those builds | Rust 11.91 s, Vesper 14.42 s: **1.21x** |
| `examples/checked_math.ves`, 40 builds each | driver step 8-9 ms, swap only 7-8 ms, Rust 7 ms |
| `verified_build_runs_in_the_vesper_driver` | the driver wrote the executable |

**Rust lines deleted:** 0. Rust's `VerifiedBuild` pass stays as the
fallback until the retirement.

## Left

- Optional: compare the per-arch `encoding_listing` text.
- Part 4d-4: `coverage`, then the swap into `bootstrap/stage1.rs` with
  `bootstrap/stage1.lock` reproduced, and the Rust deleted at the
  retirement.
