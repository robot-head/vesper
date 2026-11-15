# Wave eight lane 20: Bridge in the compiler

Design coverage: §13.3 (`.vsys` bindings, `ForeignVal[T]`, `Ffi[Lib]`
confinement) and §13.4 (handler tiers, `isolated(mpk)`). Claim issue #469.

- Base: `1094e649c95e4d81eef4e705d13882ab1339542d` (origin/main after plan PR
  #477, a descendant of the required baseline `ea1ed29`).
- Branch: `claude/project-thread-a7i2lq`.
- Paths edited: new `src/ffi_lowering.rs`, `src/ffi_lowering/{types,native}.rs`,
  `tests/ffi_lowering.rs`, `tests/fixtures/ffi_lowering/**`, this report; a
  narrow hook commit in `src/native.rs`; root patch `pub mod ffi_lowering;` in
  `src/lib.rs` (own commit). No `src/check.rs` hook was needed (see
  Decisions), so this lane does not wait on #379/#375. No dependency,
  manifest or lockfile change: the root package already depends on
  `vesper-ffi`, `blake3` and `target-lexicon`. `vesper-ffi` is used read-only.

## What was built

### 1. `.vsys` imports resolved by the checker, with `Ffi[Lib]` effects

```rust
let mut imports = Imports::new();
imports.add(vsys_source, Handler::Direct)?;          // or Handler::IsolatedMpk
let checked = ffi_lowering::check_source(source, &imports)?;   // or check_module
let object = checked.object("entry", None)?;          // host or explicit target
```

Source names the library with no new syntax:

```text
fn sum(a: u32, b: u32) -> u32 eff [Ffi[Bridge]] { Bridge::bridge_sum(a, b) }
```

- Each imported library becomes an ordinary effect declaration `Ffi$Lib`
  (unspellable in source) whose operations are its functions; `eff [Ffi[Lib]]`
  rows are rewritten to it and `Lib::f(..)` calls to its operations. The
  ordinary checker then enforces the capability on every foreign call and
  through every caller: a call from a row without `Ffi[Lib]` is E0401, so a
  pure function can never reach a foreign symbol, directly or through a
  callee. Diagnostics are demangled back to `Ffi[Lib]` and `Lib::f`.
- Typed errors (`FfiErrorKind`): `Vsys`, `Parse`, `DuplicateLibrary`,
  `NameCollision` (generated wire-struct/enum/`ForeignInvalid` names vs source
  declarations and builtins), `UnknownLibrary` (`Ffi[Zlib]` not imported),
  `UnknownForeignFunction`, `UnsupportedType`, `SliceArgument`,
  `ForeignHandler` (`handle Ffi[Lib]` in source), `MalformedCapability`
  (labelled or multi-argument `Ffi[..]`), `ForeignValue` (a foreign function
  used as a value), `Realtime` (§32.2: realtime rows cannot carry `Ffi`),
  `Check` (checker diagnostics), `TargetMismatch`, `IsolationUnavailable`,
  `ArtifactMismatch`, `Codegen`.
- Generated source declarations: each `wire struct` becomes a `copy struct`
  (padding hidden, `ByteArray[N]` an `N`-tuple of `u8`, nested structs
  nested), each `.vsys` enum a `copy enum`, and `copy struct ForeignInvalid {
  raw: u64 }` when a result needs validation.
- Slices: the source language has no unbounded arrays, so a `CSlice[T]`
  argument is a homogeneous tuple of `T`, and its arity is its length. The
  frontend checks the module once with slice parameters open (typed `$open`,
  which nothing is compatible with, so the checker records each argument's
  own type), reads each slice argument's tuple arity, and checks the module
  again against one operation per distinct arity (`bridge_crc_update$4`,
  `bridge_crc_update$2`). The World the backend sees is fully concrete.
  `ForeignChecked::calls()` lists every call site with its slice lengths.

| `.vsys` | source |
| --- | --- |
| `u8`, `u32`, `u64`, `i64` / `usize` | same / `u64` |
| `bool` (uint8_t carrier), `enum E: uN` | `bool`, generated `E` |
| `nocapture in/out/inout p: T`, `Option[T]` | `in`/`inout` `T`, `Option[T]` (`None` = NULL) |
| `nocapture in/inout s: CSlice[T]`, `Option[CSlice[T]]` | `in`/`inout` tuple of `T`, `Option` of it (`None` = `(NULL, 0)`) |
| `ForeignVal[T]` total `T` | `T`; `i8/i16/i32` widen to `i64`, `u16` to `u32` |
| `ForeignVal[bool]`, `ForeignVal[E]` | `Result[T, ForeignInvalid]`, validated at the boundary |

### 2. Native lowering of foreign calls

`src/ffi_lowering/native.rs` is a child module of `crate::native` (like
`regions/native.rs`), entered from a one-line hook in `Codegen::perform`. Per
call it declares the C symbol (`Linkage::Import`, the `.vsys` ABI signature
with `uext`/`sext` on narrow integers) and:

- narrows scalars to their C carrier; maps `bool` to 0/1 and an enum's tag to
  its declared discriminant;
- marshals `in`/`out`/`inout` single pointers, wire structs and slices into a
  zeroed C-layout stack buffer (byte-swapping explicit-endian fields), and
  copies writable ones back into the source place after the call, also
  through nested structs, byte arrays and `Option` payloads;
- passes structs and slices whose source layout already is the C layout
  (only 64-bit native-endian fields, e.g. `wire struct Rec { id: u64,
  value: i64 }`, `CSlice[u64]`) by address, with no copy;
- validates `bool` and enum results before they become source values
  (`Err(ForeignInvalid { raw })` otherwise).

Hooks in `src/native.rs` (own commit, 4 hunks): the `mod ffi` declaration;
`capability_only` and `fail_only` count foreign effects like capability
effects (no handler evidence), so a function with `eff [Ffi[Lib]]` compiles
once instead of being inlined (recursion works); the entry check accepts
foreign capabilities; `perform` dispatches foreign effects to the new module.
Without the frontend's plan the backend rejects foreign effects with an error
(`vesper::native::object` on such a World fails). A `.vsys` symbol that names
a function of the object itself (for example an entry exported under the same
name) is an error, never a silent rebinding. The link inputs and isolation
symbols list only the libraries whose calls the entry actually reaches;
unreachable foreign calls are not target-checked.

`ForeignObject` carries the object bytes, CLIF, the link inputs (library,
mode, artifact name, BLAKE3 digest, handler), a textual manifest
(`vesper.bridge-lowering/1`) and `verify_artifact(lib, bytes)`, which checks
the archive against the `.vsys` pin before linking.

### 3. `isolated(mpk)` trampolines

For a library imported with `Handler::IsolatedMpk` each call is wrapped in
generated code: load the `u32` mask from the exported data symbol
`vesper_bridge_pkru_revoke_<Lib>`, trap (user trap 5) if it is zero or would
touch key 0's bits, `RDPKRU`, `WRPKRU(saved | mask)`, call C, `WRPKRU(saved)`.
`RDPKRU`/`WRPKRU` are two generated 6- and 10-byte System V leaf functions
defined with `Module::define_function_bytes` (Cranelift has no PKRU
instructions); there is no new `unsafe` Rust. The host allocates a key,
tags its pages with it and stores `pkru_revoke_mask(key)` (the AD|WD bits)
in the symbol. Only Linux x86-64 targets are accepted; any other target is
`IsolationUnavailable` (AArch64 POE is not generated).

## Acceptance evidence

`tests/ffi_lowering.rs` (6 tests):

- `c_host_calls_vesper_which_calls_c_libraries`: builds `bridge.c` into
  `libvesper_bridge.a` with `cc -O2`, pins its BLAKE3 in `bridge.vsys`,
  compiles `bridge.ves` to an object exporting `bridge_main`, links it with a
  C host (`host.c`) that calls into Vesper for 8 scenarios, and Vesper calls
  out to 24 C functions (reporting what it saw back through C). Checked
  exactly: scalar and widened results (`-17`, `65535`, `-5`), two counter
  calls stay two calls, bool/enum validation (raw 2 and 9 become
  `Err(ForeignInvalid)`), enum arguments, `in`/`inout` `u8` slices of
  different arities, an `inout u32` pointer, an `out` wire struct, a
  big-endian/byte-array/u64 header read and written in place, zero-copy
  `CSlice[Rec]` read and scaled in place, `Option` slices and pointers
  (`NULL` for `None`, copy-back only for `Some`), and recursion of a
  foreign-capability function.
- `isolated_callee_writing_protected_memory_faults`: builds `sandbox.c`, links
  the isolated object with `mpk_host.c`, which `pkey_alloc`s a key, tags a page
  with `pkey_mprotect` and publishes the mask. An isolated callee writing the
  page dies with `SIGSEGV`/`SEGV_PKUERR` (exit 42, "pkey fault"); isolated
  calls that only use their arguments succeed, after which the host writes
  the page and reads `PKRU` with the key's bits clear again (restored); with
  the mask unset the trampoline traps before C runs. **Exercised** in this
  cloud session (Intel VM, kernel 6.18.44, `pku` in /proc/cpuinfo). On CI it
  runs in the Linux Bazel job ("Test all targets", BuildBuddy) if that
  executor has PKU; otherwise the test prints "runtime evidence unavailable"
  and returns. macOS and Windows skip the runtime tests (not Linux x86-64).
- `foreign_calls_require_the_library_capability`,
  `imports_are_resolved_with_typed_errors`, `slice_arity_is_fixed_per_call_site`,
  `objects_check_targets_and_handler_tiers`: platform-neutral frontend and
  object checks (cross-compiled x86-64 Linux objects on every host).

## Checks run

- `cargo fmt --all -- --check`: clean.
- `vesper fmt --check tests/fixtures/ffi_lowering/*.ves`: unchanged.
- `cargo +1.100.0 clippy --locked -p vesper --all-targets -- -D warnings`: clean.
- `cargo test --locked --test ffi_lowering`: 6 passed (PKU exercised).
- `cargo test --workspace --locked --no-fail-fast` (base 1094e64 + this branch):
  4477 passed, 6 failed. All 6 are Lean-toolchain tests outside this lane
  (`lean4lean42_acceptance`, `oracle42_acceptance`, `oracle42_runner`,
  `proof_audit_private`, `replay42_kernel`, `resource_model42`): they need a
  `lean` binary or `VESPER_LEAN4LEAN42_ROOT`, which this cloud container does
  not have.

## Decisions

- The import is out of band (an `Imports` set, the role Loom's manifest plays)
  because `crates/vesper-syntax` is not this lane's to change; source names
  the library only in `Ffi[Lib]` rows and `Lib::f` paths. No `src/check.rs`
  hook: the frontend wraps `check::check` (like `pact_frontend`) and the
  capability rule is the existing effect-row rule.
- Foreign capabilities are granted at the package boundary: the entry may
  carry them and source cannot handle them. The handler tier is part of the
  import, not source syntax.
- Marshalling copies into stack buffers because the native source layout uses
  8-byte slots for every scalar; zero-copy only where that coincides with the
  C layout. All buffers are stack slots: no heap allocation per call (not
  measured with a counting allocator; nothing on this path calls an
  allocator).
- Narrow C parameters (`i8`, `i16`, `i32`, `u16`) are rejected rather than
  silently truncated, since source has no carrier and no checked narrowing.

## Remaining gaps

- No CLI wiring (`vesper build` with `.vsys` imports): `src/main.rs` belongs to
  another lane. No JIT path: Cranelift's JIT resolves imports through the
  process symbol table and panics on a missing symbol, so only objects are
  emitted.
- `linear extern type` handles and `sink` finalizers are not in `vesper-ffi`'s
  `.vsys` grammar yet, so they are not lowered. `Ptr[..]` results, address
  parameters and narrow integer parameters are rejected.
- The trampoline revokes one key around the call; it does not pivot to a
  sandbox stack, give the library its own heap domain, or stop foreign code
  from running `WRPKRU` itself (Omniglot-style binary inspection is not
  done). Arm POE is not generated. Performance was not measured.
- `nocapture` and pointer direction are trusted from the `.vsys` contract;
  nothing observes what C does with a pointer. Rust ownership in this
  compiler is not evidence that the Vesper source typing rules hold; those
  are the checker's rules, and no Lean model covers this lowering.
