# Self-hosting SH4 lane 13: FFI lowering in Vesper (parts 1, 2 and 3)

Claim: #1287. Prompt: `docs/agent-prompts/self-host-sh4/13-ffi-lowering.md`.
Stacks on #1259 (branched from its head `8ac0f94`; `origin/main` was
`e73cfd6`) and on lane 1's #1309 (merged in, now at `32a334a`, for the
emission). Rust stays the default build: nothing here replaces Rust code,
and the default compiler installs no hook.

## What changed

- **`packages/vesper-ffi-lowering`** (`@vesper/ffi-lowering`): the `.vsys`
  type mapping of `src/ffi_lowering/types.rs` and the call plans of
  `src/ffi_lowering.rs` in Vesper.
  - `read_library` decodes a library's **canonical descriptor bytes**
    (`LibraryDescriptor::canonical_bytes`, the bytes its BLAKE3 identity is
    taken over) into tables: enums, functions, parameters, wire structs
    with field offsets, extern types with drop imports.
  - `generate` is `Generated::new`: generated wire structs (padding hidden,
    byte arrays as `u8` tuples, nested structs), enums, linear/affine handle
    structs, every function's source shape with its error, the
    `ForeignInvalid` flag, and the first `DuplicateLibrary` error in Rust's
    order.
  - `append` is `Generated::append` for pass 1 (slices open as `$open`) and
    pass 2 (one operation per slice-length vector, `op_name` spelling, lent
    `Slice` lengths), written as the AST dump.
  - `check_collisions`, `demangle`, `slice_length`, `op_plan` (with the
    target's blocked libraries), `target_support` and
    `FunctionDescriptor::abi`.
- **`src/ffi_lowering.rs`**: a behaviour-free `#[doc(hidden)] pub mod
  plan_probe` at the end of the file, so the test can call the private
  `types::Generated`, `slice_length`, `target_support`, `op_plan` and (part
  2) the `Rewriter` (`rewrite`, and `arities`: pass 1 plus the checker, as
  `check_module` computes the pass 2 lengths). It only
  forwards to the code the compiler runs (precedent: `check::core_typing_probe`
  of SH2 lane 7). Flagged for the owner: it is the one Rust file this lane
  touches, and no lowering behaviour changes.
- **`tests/self_host_ffi_lowering.rs`**: the differential, an ignored ABBA
  timing, and the input word format in its header. BUILD.bazel data and
  size entries.

### Part 2: the source rewrite

- **`packages/vesper-ffi-lowering/rewrite`** (`@vesper/ffi-rewrite`): the
  `Rewriter` of `src/ffi_lowering.rs` over the `@vesper/ast` tree.
  `Ffi[Lib]` rows become `Ffi$Lib`, `Lib::f(..)` calls become the library's
  operations, and every typed error of the pass is raised:
  `MalformedCapability`, `UnknownLibrary`, `Realtime`, `ForeignValue`,
  `HandleMisuse`, `ForeignHandler`, `UnknownForeignFunction` and
  `UnsupportedType`. The first error by span wins, as `finish` picks it.
  Pass 1 records each call's slice arguments. Pass 2 takes the lengths per
  call span, records the slice-length vectors used and the calls, and names
  the operations with `@vesper/ffi-lowering`'s `put_op_name`. The library
  tables, shapes and handles come from `@vesper/ffi-lowering`, which now
  exports `s_new` and `slice_count`.
- **`tests/self_host_ffi_lowering_rewrite.rs`**: the differential. Each pass
  writes its first error, its tables and the canonical AST dump of the
  rewritten module (the dumper is a copy of tests/self_host_ast.rs's, as in
  the other AST lanes). There is also an ignored ABBA timing. BUILD.bazel
  gets the test's data and size entries.

### Part 3: native emission of foreign calls

- **`src/codegen_swap.rs`**: `pub mod ffi`, the `ffi` family's step
  requests (`BEGIN`, `ARG`, `REFUSE`, `CALL`, `RESULT`, `FINISH`) and their
  word encodings. It is appended after lane 1's modules, and nothing else
  in the file changes.
- **`src/ffi_lowering/native.rs`**: one dispatch line in `foreign_perform`
  and `foreign_perform_swapped` at the end of the file. When an `ffi` hook
  is in effect, the call is offered in steps. If the hook declines `BEGIN`,
  the unchanged Rust emission runs. Flagged for the owner: these are
  dispatch hunks in the Rust lowering. The coordinator approved them on
  2026-11-21, and lane 1 was told.
- **`packages/vesper-ffi-lowering/emit`** (`@vesper/ffi-emit`): `ffi_hook`,
  the emission in Vesper through `@vesper/cranelift`. It covers scalar,
  bool and enum arguments, handles, `Option` guards, pointers and copied
  slices with wire structs (byte order, byte arrays, padding, nesting),
  slices lent in place, C strings and their refusal block, the call,
  `errno`/`GetLastError`, copy-backs, result widening, and validation of
  bool, enum and handle results, `NonNull` traps and C string frees.
- **`crates/vesper-codegen-swap-test/tests/self_host_ffi_emission.rs`**:
  the differential, using lane 1's harness crate.

## Design decisions

- **Input = canonical descriptor bytes.** They are public, complete (every
  enum, function, parameter form, wire struct declaration, extern type and
  drop import) and stable (they are the library's identity). Porting the
  `.vsys` parser itself belongs with `vesper-ffi`, not this lane.
- **Rust reference through a probe, not a re-derivation.** `types` is
  private and port lanes must not change Rust lowering, so the probe exposes
  the existing functions unchanged.
- **One file, no dependencies.** On this base constants are not importable
  across modules (#1239 is not under #1259 yet), so the package is one
  `lib.ves` with its own small interner.

- **Emission in steps, not one `INSTRUCTION` request.** The graph walk's
  `ffi` request carries only operand values. Rust's `foreign_perform`
  evaluates each argument against `Codegen` state a hook cannot see:
  variables passed to a foreign call are memory-homed, and `in`/`inout`
  arguments are place loads. The steps leave that state in Rust, which
  evaluates the arguments, declares the callee, does the `inout` restores,
  builds the `errno` pair and does a refused call's `Ok` store. The hook
  emits everything between those points, in the order Rust does, so CLIF
  can match line for line. Layout facts (option tags, field offsets, wire
  fields' source offsets) cross as words.
- **The rewrite edits rows in place.** A callee rename overwrites the
  callee's expression row, and a row rewrite overwrites the effect row.
  Both depend only on the row and the imports, so a row the parser shares
  gets the rewrite every Rust copy gets. Pass 2 parses the source again,
  just as Rust clones the module.
- **Drop items are not compared.** `check_module` appends the release
  functions of affine handles (`drops::generated`) before it rewrites.
  Those items are generated, not source, and they belong with
  `drops.rs`, so the differential rewrites the parsed source only.
  `plan_probe::arities` likewise skips them.
- **`isolated(mpk)` stays in Rust.** Its trampolines need
  `define_function_bytes`, which the shim does not have, so the hook
  declines `BEGIN` for those calls.

## Numbers

Corpus: 68 distinct libraries (every `.vsys` file, every `// @export vsys`
directive, the `.vsys` sources of 15 FFI test files with each target for a
`@TARGET@` placeholder, and 6 hand-written libraries), as 106 cases (single
libraries, neighbouring pairs, and all pairs of the hand-written ones). Under
Bazel the `crates/` sources are not declared, which leaves 50 libraries.

| Check | Result |
|---|---|
| Cases differing (56,506 text lines) | **0 of 106** |
| Errors reached | `DuplicateLibrary`, `NameCollision`, `TargetMismatch`, `IsolationUnavailable`, `SliceArgument`, shape errors |
| Mutation check (two edits to the port) | caught on the first case |
| Package compile, unoptimized / optimized | 6.6 s / 69 s |
| Whole corpus, ABBA | Vesper 81 ms, Rust 294 ms (0.27x) |

The timing is from the debug test binary, so the Rust side is unoptimized
while the Vesper entry is compiled with `optimize`. It is not a parity
number; the parity bar for this lane is the whole compile with emission
swapped (part 3).

**Part 2, the rewrite.** The FFI corpus is 140 cases:

- every Vesper source string of 9 FFI test suites;
- every `tests/fixtures` `.ves` file that names a library, each with the
  libraries it names, once direct and once `isolated(mpk)`;
- hand-written `Shapes` and `Bridge` sources;
- textual mutants of the Bridge fixture.

The repository corpus is every `.ves` file of the repository (969) with
the Bridge library. Pass 2 runs whenever pass 1 succeeds and the checker
gives lengths.

| Check | Result |
|---|---|
| FFI corpus cases differing (58,331 text lines) | **0 of 140** |
| Repository files differing (2,168,902 text lines) | **0 of 969** |
| Errors and tables reached | all 8 rewriter errors, slice sites, slice-length vectors (including `[]` for an arity mismatch), calls with lent slices |
| Mutation check (ties between equal error spans keep the last error) | caught (`hand Bridge`) |
| Package compile, unoptimized / optimized | 60 s / 387 s |
| FFI corpus, ABBA | Vesper 64 ms, Rust 425 ms (0.15x) |

This is not a parity number either. The Rust side runs in the debug test
binary and also computes the pass 2 lengths with the checker, which the
Vesper side reads as input.

**Part 3, emission.** The corpus has three parts:

- the `.vsys` fixtures with their programs (bridge, with both handler
  tiers, plus sandbox and aead), for all four targets;
- a hand-written `Forms` library and program reaching every parameter and
  result form, for all four targets;
- the `.vsys` packages (std fs, process, env, cranelift, codegen-base), for
  the host, with every 8th entry.

Every function that performs a foreign effect is an entry, built with the
optimizer on and off.

| Check | Result |
|---|---|
| CLIF and object differences (358 builds, 732 functions) | **0** |
| Requests answered from Vesper | 7,946 (2,288 declined: graph-walk `INSTRUCTION` requests and `isolated(mpk)` calls) |
| Mutation check (`select` operands swapped in the `Option` guard) | caught: CLIF and object differ |
| Default build, 159 corpus files with and without `--no-opt` (base #1309 `cda32d9` vs this branch) | **318 of 318 objects byte-identical** (760 builds failed on both sides) |
| Test time (debug) | 6 min |

## Stage 1 guard

No guarded path is touched (`src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**`, `packages/vesper-compiler/**`),
and the probe module (parts 1 and 2) is not called by the compiler. Part 3's only
compiler changes are the behaviour-free `ffi` dispatch hunk in
`src/ffi_lowering/native.rs` (native object path only; no hook is installed
by default, so the default build is byte-identical, see the 318-object check
above) and the `codegen_swap::ffi` step constants in `src/codegen_swap.rs`.
The Bootstrap Stage 1 workflow on this branch is the evidence (linked from
the PR).

## Compiler gaps

1. `affine` and `effect` are keywords and cannot be parameter or local names
   (`fn f(affine: bool)`), E0002. Known: #1244 (SH3 lane 5).
2. Constants are not importable across modules on this base (#1239 not
   merged under #1259), which keeps the package in one file.
3. `while k < n { ... }` does not parse: `n { ... }` is read as a struct
   literal (E0002 "expected RBrace, found Lower"). Repro:
   `fn f(n: u64) { let mut k: u64 = 0; while k < n { k = wrapping_add(k, 1); } }`.
   The emitter uses `for` loops instead.
4. The shim has no `AbiParam` extension in `vcl_sig_param`/`vcl_sig_return`,
   no `define_function_bytes` and no declarations lookup. The first two
   keep the callee's signature and the `isolated(mpk)` trampolines in Rust.
   The third keeps the symbol collision checks in Rust (they emit no
   code).

## What is left

- Part 2 remainder: the drop items of `drops.rs` (generated release
  functions and their row checks).
- Part 3 remainder: `isolated(mpk)` trampolines and the callee's signature
  in Vesper, once the shim has `define_function_bytes` and parameter
  extensions.
- Part 4: the hybrid `ffi_lowering` module and its `ffi` hook install, the
  full suite and the SH4 scoreboard row.
