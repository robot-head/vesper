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

## Left

- Optional: compare the per-arch `encoding_listing` text.
- Part 4c-2: region blocks and region operations. Part 4c-3: `evidence`. Part 4d:
  `coverage`, `cli`, then the swap into the hybrid and `bootstrap/stage1.rs`, with `bootstrap/stage1.lock`
  reproduced, and the Rust deleted at the retirement.
