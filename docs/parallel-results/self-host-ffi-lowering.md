# Self-hosting SH4 lane 13: FFI lowering in Vesper (part 1)

Claim: #1287. Prompt: `docs/agent-prompts/self-host-sh4/13-ffi-lowering.md`.
Stacks on #1259 (branched from its head `8ac0f94`; `origin/main` was
`e73cfd6`). Rust stays the default build: nothing here replaces Rust code,
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
  `types::Generated`, `slice_length`, `target_support` and `op_plan`. It only
  forwards to the code the compiler runs (precedent: `check::core_typing_probe`
  of SH2 lane 7). Flagged for the owner: it is the one Rust file this lane
  touches, and no lowering behaviour changes.
- **`tests/self_host_ffi_lowering.rs`**: the differential, an ignored ABBA
  timing, and the input word format in its header. BUILD.bazel data and
  size entries.

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

## Stage 1 guard

No guarded path is touched (`src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**`, `packages/vesper-compiler/**`),
no swap point is added, and the probe module is not called by the compiler.
The Bootstrap Stage 1 workflow on this branch is the evidence (linked from
the PR).

## Compiler gaps

1. `affine` and `effect` are keywords and cannot be parameter or local names
   (`fn f(affine: bool)`), E0002. Known: #1244 (SH3 lane 5).
2. Constants are not importable across modules on this base (#1239 not
   merged under #1259), which keeps the package in one file.

## What is left

- Part 2: the `Rewriter` (`Ffi[Lib]` rows, `Lib::f` call sites, handle
  misuse, slice sites and instances) over `@vesper/ast`, compared through the
  public `check_source` outcome (`ForeignChecked::calls`, errors).
- Part 3, after lane 1's (#1275) first PR: emission of `native.rs`
  (marshalling, copy-back, `CStr`, `errno`, result validation, `isolated(mpk)`
  trampolines) through `@vesper/cranelift`, with CLIF and object
  differentials.
- Part 4: the hybrid `ffi_lowering` module and its `ffi` hook install, the
  full suite and the SH4 scoreboard row.
