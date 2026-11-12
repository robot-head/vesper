# §38 executable Grammar host foundation — issue #63

Claim: <https://github.com/robot-head/vesper/issues/63>. Branch `tooling41/grammar`,
base `a1a8d938dd8103803219a05c8364cb7a198be8d9`, dedicated new worktree
`/workspace/vesper-worktrees/tooling41-grammar`.

Read the issue, `docs/rfcs/tooling41-contracts.md`, complete §38 and the §38
continuation review. Source snapshot:
`8e0f05ae32a6c87a8ce25fb339f66c760fe5d5f3fd3cb05e58236d291fa62207`, modified
`2026-11-12T09:18:30.204Z`; §38 digest
`e86ef66e6ce08aea8f2ba5f661582a96b0a016371890e67973f73490f3b8b293`.

## Delivered

New safe dependency-free `vesper-grammar` executes three independent slices:

1. Canonical unsigned u64 LEB128 and canonical-length/opaque-byte frames, with
   checked lengths, representability and payload/output/work budgets. Prefix
   operations expose nonclone `Remainder` owners. Exact EOF and next-frame
   operations return the original remainder on failure. All encoders preflight
   before touching caller buffers, preserving the complete output on every
   Result failure and successful suffixes. Exact unsigned/frame representations
   are unique for their supported values; prefix parsing alone makes no
   whole-authenticated-frame non-malleability claim. Each operation has its own
   budget; repeated independent parses are not a globally metered stream.
2. Actual primitive Arrow signed-i64 body/validity views from exact 16-byte
   descriptor/field-node DTOs. Validation rejects negative/unknown counts,
   unrepresentable sizes, checked `rows*8` overflow, truncated ranges/bitmaps,
   conflicting declared null counts and nonempty overlapping buffers. Actual
   LSB-first validity bits determine nullable values and aggregation; padding
   bits/bytes are ignored. `ByteCopy`, `Relative8`, `Relative64` and
   `HostAddress64` profiles separate relative offsets from actual host alignment.
   Host alignment proves no DMA capability. Aggregation uses sequential checked
   arithmetic and returns None for empty/all-null, rejecting intermediate
   overflow even when later values could cancel it.
3. Validated executable acyclic byte-FIRST grammars: empty/byte/literal,
   canonical ULEB, opaque length-delimited blob, nested exact frame, sequence and
   deterministic choice. Branch IDs and references are checked; FIRST/nullable
   sets come from actual rules. Nullable sequence prefixes work; nullable choice
   alternatives explicitly fail rather than inventing FOLLOW/PEG semantics.
   Captures contain actual rule/branch/span/numeric/payload results. Cycles and
   unsupported recursion fail, including in unreachable rules. Shared DAGs are
   supported, but every expanded parse visit consumes work/output budget.

The crate README states exact semantics, error offsets, accepted formats and
ownership/limit boundaries. No source macro, general Protobuf/DER/PEG grammar,
FlatBuffers/IPC envelope, Arrow child/dictionary/compression support, SIMD,
constant-time parsing, mmap, source mode enforcement or model proof is claimed.
No Lean file was added; no theorem/proof badge is supplied. Core schemas,
`vesper.arith.checked/1`, interface v2, ABI v2 and `k_i ⊑ m_i` remain untouched.

## APIs for integration

- `framing::{decode_canonical_u64_leb128, parse_u64, parse_u64_exact, parse_frame,
  parse_frame_exact, encode_u64_into, encode_frame_into, frame_encoded_len,
  encoded_u64_len, Limits, Frame, Remainder, TailError}`.
- `arrow::{BufferDesc, FieldNode, AlignmentProfile, Limits, checked_buffer_range,
  I64Column::validate}`; validated column `get`, `checked_sum`, length/null count
  and borrowed value/validity accessors.
- `grammar::{Rule, Branch, Limits, Grammar::validate}`; validated grammar
  `parse_prefix`, `parse_exact`, `nullable`, `first_contains`; actual `Capture`s.
- Allocation-free `Error { kind: ErrorKind, offset: usize }`. Definition errors
  use rule indices, input errors use byte offsets, preflight errors may use zero.

## Validation evidence

Used private `/tmp/vesper-grammar-target` (about 7 MiB), one build job, no dev/test
debug info and no incremental cache. A temporary leaf `[workspace]` stanza was
used for isolated tests and removed before commit; no leaf lock is committed.

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-grammar-target cargo test --manifest-path crates/vesper-grammar/Cargo.toml --all-targets
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-grammar-target cargo test --manifest-path crates/vesper-grammar/Cargo.toml --doc
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-grammar-target cargo clippy --manifest-path crates/vesper-grammar/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path crates/vesper-grammar/Cargo.toml -- --check
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-grammar-target cargo run --manifest-path crates/vesper-grammar/Cargo.toml --example grammar
```

All pass: **20 integration tests**, one executable Rustdoc, one compile-fail
Rustdoc verifying that Remainder lacks Clone, strict all-target Clippy and format.
Example output:

```text
Canonical frame: 6 bytes; pipeline remainder consumed exactly
Grammar branch 8 decoded nested value 42; Arrow 3 rows, 1 null, sum 6
```

Independent evidence includes fixed LEB128 vectors (`624485 -> e5 8e 26`, u64
maximum), a division/remainder oracle for 20,000 generated values, all 65,536
exact two-byte strings reserialized identically whenever accepted, all 128
2-byte overlong cases, all invalid tenth-byte values, every truncation/small
output of reference values/frames, and explicit unchanged buffer checks.

Arrow tests use real LE i64 bytes and actual bitmaps, all 1,023 nullable bitmap
configurations for lengths 0–9, an independent i128 prefix-sum overflow oracle,
absent/all-null/empty/padded bitmaps, byte-exact descriptor vectors, every body
truncation, wrong counts, negative/range/alias errors and aligned-host versus
shifted-host relative64 validation. A 32-bit-only representability check is
included but was not executed on a 32-bit target in this lane.

Grammar tests execute all 256 disjoint first-byte alternatives, real numeric/
payload captures, nested exact frames and boundary confinement; reject ambiguous
FIRST sets, duplicate IDs, bad refs, empty choice and recursive/unreachable
cycles; enforce nullable-choice scope, every definition/input/capture/work/depth
budget, hard height 64 and memoized DAG heights. Exponential nullable DAG
expansion is stopped by actual work/events rather than only descriptor checks.

## Resource and claim boundaries

Framing allocates no dynamic storage; it reads at most ten prefix bytes and
checks opaque payload bounds/work. Arrow validation/aggregation allocate no
storage and are bounded by body bytes, rows and work. Grammar validation has one
fallible bounded metadata vector, parsing one bounded capture vector; requested
payload byte caps exclude allocator bookkeeping/capacity rounding, caller-owned
rule allocations and RSS. `try_reserve_exact` allocation failures are handled,
but allocator fault injection was not performed. Work units are deterministic
logical charges, not time or instruction counts. Rule recursion depth is capped
by min(caller limit,64); a flat frame has no implicit recursive interpretation.
Rust may drop/forget remainder tokens or independently reparse input. This API
is not a compile-time linear-resource proof. Canonical wire behavior is not
cryptographic authentication or secret constant-time evidence.

Coordinator adds the crate to both root workspace members/default-members and
runs `cargo run -p vesper-grammar --example grammar`; a root dependency is needed
only if facade code calls these APIs. No dependency addition outside this leaf
is required. The exact root registration patch follows.

```diff
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -28,2 +28,3 @@
     "crates/vesper-serde",
+    "crates/vesper-grammar",
     "crates/vesper-quarry",
@@ -68,2 +69,3 @@
     "crates/vesper-serde",
+    "crates/vesper-grammar",
     "crates/vesper-quarry",
```
