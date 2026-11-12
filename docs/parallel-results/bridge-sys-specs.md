# Bridge declarative sys-specs

Base: `1ffd0aec27dfcc24c5564585ae9068255490bf61`.
Branch: `parallel-next/bridge-sys-specs`.

This completed supported milestone adds a separate `vesper_ffi::sys` API for
bounded `.vsys` contracts. Existing wire-layout APIs and their tests are unchanged.
The accepted grammar and limitations are specified in
[`crates/vesper-ffi/SYS-SPECS.md`](../../crates/vesper-ffi/SYS-SPECS.md).

## Delivered behavior

- One declarative library with explicit target, C calling convention, link mode,
  bare library name, expected BLAKE3 artifact digest and optional normalized
  header/pkg-config constraints. Four supported 64-bit target/convention pairs:
  Linux x86_64 System V, Linux AArch64 AAPCS64, Darwin AArch64, Windows x86_64 MSVC.
  No native host layout is substituted for another target.
- Fixed integer scalars, logical bool/enum refinements on explicitly total-bit C
  carriers, and `nocapture in/inout CSlice[integer]` parameters lowered to pointer
  plus target `size_t` element count. Functions expose read-only C ABI metadata.
  Unsupported ABI/layout/handle/variadic/imperative forms fail closed.
- Parser work/retention limits plus bounded canonical serialization. Versioned,
  length-framed canonical descriptor bytes cover all target/library/signature
  and return-validity inputs. Sorted declaration order/comments do not affect
  identity. `DescriptorId` hashes those bytes using BLAKE3. Artifact bytes can be
  checked separately against the declared content digest.
- `ForeignValue<O>` captures raw bits and a caller owner. Validation checks exact
  integer widths, signed interpretation, 0/1 bool, sparse-enum membership, and
  pointer numerical nullability/alignment. No unchecked bool, enum, reference or
  pointer provenance is constructed. Failure returns the original value/owner;
  optional bounded error-sentinel rejection also retains ownership.
- `prepare_call` checks scalar arguments and numerical pointer/length shape,
  alignment, checked ranges, byte-size bounds and mutable overlap. It returns
  flattened ABI carriers with the owner, or all unchanged original arguments
  and owner on failure. Shared overlap and disjoint adjacent unique ranges work.

The library retains `#![forbid(unsafe_code)]`. A controlled separate integration
fixture host uses explicitly unsafe extern C calls to a tiny compiled C archive.

## Validation

Environment: Linux x86_64 GNU, Rust `1.99.1 (58cf8f9c3 2026-10-13)`, GCC
`14.2.0`, GNU ar `2.44`.

All commands ran from the dedicated worktree with isolated
`CARGO_TARGET_DIR=target-bridge-sys`, `CARGO_BUILD_JOBS=1`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`:

```text
cargo test -p vesper-ffi -- --nocapture
  PASS: 23 tests (13 sys-spec/ABI/ownership, 1 actual C ABI fixture, 9 existing wire).
cargo clippy -p vesper-ffi --all-targets -- -D warnings
  PASS.
cargo fmt --all -- --check
  PASS.
git diff --check
  PASS.
```

The native fixture actually invokes `cc -std=c11 -Wall -Wextra -Werror -O2 -c`,
`ar crs`, and `rustc --edition=2024 -L native=... -l static=bridge_fixture`, then
runs the generated Rust host. It exercises scalar/signed returns, invalid raw
bool/enum results, a numerical pointer return and an actual disjoint
pointer+length byte-copy call. Concrete archive bytes match the descriptor
artifact digest; changed bytes fail verification. Runtime evidence is
specifically Linux x86_64 System V, not Darwin/Windows/AArch64 execution.

Negative tests cover truncation at every manifest prefix, malformed input,
identifier/string/source/token/declaration/parameter/canonical budgets,
unsupported targets/conventions, linker flags/paths, malformed digests,
unsupported layouts/variadics, duplicates, invalid scalar bits, sparse enum tags,
null/misaligned addresses, pointer-range overflow, excessive byte lengths,
mutating aliasing and owner retention.

## Coordinator integration

The existing workspace member needs no registration/module/CLI changes. Its
exclusive crate manifest adds `blake3 = "1.8"`, already resolved in the workspace.
The coordinator-owned root lock change is intentionally uncommitted; apply:

```diff
 [[package]]
 name = "vesper-ffi"
 version = "0.1.0"
+dependencies = [
+ "blake3",
+]
```

No push, PR or main merge was made in this lane.

## Boundaries and remaining work

A descriptor records a contract; artifact content equality does not establish
provenance, safe behavior, actual linker selection or authority. This library
neither proves `nocapture` nor supplies Loom `Ffi[Lib]` capability checks. Numeric
address validation establishes no live allocation, dereference permission or
provenance. Unsafe embeddings still need valid memory and faithful foreign code.
Rust ownership results permit ordinary Drop; source linear exhaustion and
foreign finalizers are not implemented.

ABI metadata records logical carriers and conventions but is not register/stack
classification or a native trampoline. Future codegen must implement each
platform's complete C ABI, including narrow-integer extension rules. Source
parser/checker integration, compiler calls, C header verification, dynamic
loading/linking, opaque linear handles, variadics, aggregates, callbacks,
floating-point signatures and MPK/PKRU/POE isolation remain separate work.
