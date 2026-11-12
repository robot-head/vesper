# Expansion 37: bounded JSON host codec

Review source: specification snapshot `04c1d5975c031a1dd9be11c3fbfb0c70e973d1c36fefa662ce775cd39dca61c3`,
modified `2026-11-12T08:13:57.039Z`. Read complete sections 1–7 and 20, source
metadata, and `docs/rfcs/expansion-37-contracts.md` before implementation.

## Review findings and scope

- Sections 2–6 require exact linear obligations, second-class projections,
  one-shot continuations, explicit handler state and the Fail resumption-frame
  check. Section 4 now explicitly requires the user-approved `k_i ⊑ m_i`
  premise. This independent Rust library neither changes nor claims to implement
  those source rules; borrowed operations leave input owners with callers.
- Section 7's compiler pipeline, source derives/Pact/Forge, native runtime and
  Lean extraction remain outside this lane. No interface, arithmetic contract,
  fiber ABI or compiler schema changes are made.
- Section 20's O(1) unknown-subtree skip is sound only after *complete* primitive
  and Unicode validation. The implementation fully validates before exposing
  raw slices and precomputes subtree ends; it does not skip malformed unknowns.
- The illustrative `compile_key` accepts non-ASCII, controls, quotes and
  backslashes despite its direct short-ASCII-key premise. The implemented helper
  rejects these spellings, checks short windows safely, and has semantic matching
  for escaped-equivalent validated keys.
- The source byte-bound calculator adds unchecked `usize`s. Here all schema and
  runtime size accumulation returns checked errors, including string-bound
  multiplication. A read-only peer review also found the mixed-bound pitfall:
  adding bounded-field *upper* ceilings cannot yield a dynamic total-size lower
  bound. `Dynamic { static_ceiling }` now explicitly describes only an upper
  ceiling of the statically bounded contribution, excluding dynamic payloads;
  it makes no total-size upper/lower claim. The concrete `{a:0,b:""}` regression,
  all four bound combinations and dynamic overflow are tested. The source's `F64` ceiling is not exposed because this slice
  has no float encoder.
- The source leaves duplicate-key behavior unspecified. This slice rejects all
  decoded-equivalent keys, including unknown nested objects and `"id"` versus
  `"\u0069d"`; no Unicode normalization/case folding is applied.
- Multi-GB/s, one-cycle SWAR dispatch, Eisel-Lemire timing, zero-global-allocation,
  native SIMD, source lifetime freedom and source linear rollback are not
  established by a host scalar library. The design's `bv_decide` example is not
  a kernel-only proof artifact here. No such performance/formal claims are made.

## Implemented

New dependency-free `crates/vesper-serde` contains:

- A strict UTF-8/Unicode/number/object/array parser with scalar structural tape,
  validated immutable `Node`s, bounded schema lookup, object iteration and scoped
  transactional cursors. Unknown malformed trees are rejected before any view.
- Zero-copy unescaped UTF-8 and explicit caller-scratch escape decoding,
  including all JSON escapes and paired UTF-16 surrogates. Insufficient scratch
  changes neither bytes nor cursor state.
- Explicit input, depth, token, per-string, aggregate-string, work and requested
  index-allocation limits. One fallible bounded vector reservation; no DOM or
  heap strings. Hard recursion ceiling 128. Work-bounded quadratic canonical
  duplicate detection. Allocation bounds describe payload, not allocator/RSS.
- Safe short-key/SWAR helpers and a scalar cross-block backslash-carry reference.
- Checked schema bounds plus exact preflight/transactional slice encoding of
  null/bool/u64/i64/UTF-8 strings/arrays/objects/validated raw subtrees. Raw
  embeddings respect destination depth/token/string limits. Failed encodes
  preserve the complete output buffer; borrowed input ownership is retained.
- A self-checking `bounded_json` executable example and crate-level API/limits
  documentation. No source macros, Region typing, linear deserialization,
  compiler/Forge/native/Prism/SIMD/allocator integration, floating conversion,
  binary archives or NDJSON/AoSoA integration is included.

## Evidence

Private target `/tmp/vesper-serde-target`, `CARGO_BUILD_JOBS=1`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`.

- `cargo fmt --manifest-path crates/vesper-serde/Cargo.toml -- --check` passes.
- `cargo test --manifest-path crates/vesper-serde/Cargo.toml --all-targets`
  passes 25 integration tests; example target compiles. Tests include every
  truncation/byte split of a nested Unicode document (split reconstruction, no
  streaming parser claim), 19,600 long backslash-run strings across 64-byte
  boundaries, 131,072 carry cases, invalid controls/UTF-8/surrogates, canonical
  duplicates, grammar/range errors, checked bound overflow, resource limits,
  10,000 integer encodings versus Rust decimal output, deterministic every-byte
  mutation of a nested document, and 20,000 generated Unicode scalar attempts.
- `cargo clippy --manifest-path crates/vesper-serde/Cargo.toml --all-targets -- -D warnings`
  passes.
- `cargo run --manifest-path crates/vesper-serde/Cargo.toml --example bounded_json`
  executes assertions and prints:

  ```text
  {"id":42,"topic":"build complete","raw_meta":{"ok":true,"n":[1,2,3]}}
  validated 14 tokens; scalar host codec
  ```

These are host behavior checks, not a proof, benchmark, SIMD validation, source
linearity check or native runtime result. One executable Rustdoc example also passes.

## Coordinator registration

The leaf `[workspace]` stanza was used only for isolated checks and is absent
from the committed crate. On integration, add `"crates/vesper-serde",` to **both** root workspace
`members` and `default-members`, add root dependency
`vesper-serde = { path = "crates/vesper-serde" }` if the CLI uses the host API,
and let Cargo update the root lockfile. No third-party dependency is required.
Do not commit the generated leaf `Cargo.lock`.

Stable CLI integration surface:

```rust
use vesper_serde::{IndexedJson, Limits};
let doc = IndexedJson::parse(bytes, Limits::default())?;
let kind = doc.root().kind(); // Kind::{Null,Bool,Number,String,Array,Object}
let tokens = doc.token_count();
```

`Error` has public `kind: ErrorKind` and `offset: usize` fields and implements
Display/Error. Parse and encoding default byte ceilings are 1 MiB; default depth
is 64. The `bounded_json` example is suitable for coordinator CI registration.

Root manifest registration patch (apply in coordinator worktree):

```diff
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -32,2 +32,3 @@
     "crates/vesper-ignis",
+    "crates/vesper-serde",
     "experiments/native-pulse",
@@ -63,2 +64,3 @@
     "crates/vesper-ignis",
+    "crates/vesper-serde",
     "experiments/native-pulse",
@@ -95,2 +97,3 @@
 vesper-loom-cas = { path = "crates/vesper-loom-cas" }
+vesper-serde = { path = "crates/vesper-serde" }
 ariadne = "0.5"
```
