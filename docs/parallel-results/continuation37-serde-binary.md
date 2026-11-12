# Continuation §20 binary codec slice — issue #46

Issue: <https://github.com/robot-head/vesper/issues/46>. Worktree
`/workspace/vesper-worktrees/continuation37-serde-binary`, branch
`continuation37/serde-binary`, base `5e699f8`. Read the issue and complete §20,
then checked §20.4 against the refreshed 41-section specification in the root
worktree. Current source snapshot is
`8e0f05ae32a6c87a8ce25fb339f66c760fe5d5f3fd3cb05e58236d291fa62207`,
modified `2026-11-12T09:18:30.204Z`; §20 digest remains
`6fbec230c1d6922fca9fbee9fb1ded5543d7fa8ff2c6d42743e0953ffe0db0d5`.
New §38 Grammar/LEB128/Arrow work and all externally reserved Warp §35 paths are
outside this lane.

## Implemented behavior

- Dependency-free, safe scalar raw StreamVByte u32 encoding/decoding. All control
  bytes precede little-endian data, with 1/2/3/4-byte tags and out-of-band count.
  Minimal encoding, zero unused tail tags; interoperable decoding ignores unused
  tail tags and accepts non-minimal widths. Exact frame lengths are required.
  This is neither LEB128 nor the distinct 0/1/2/4 StreamVByte variant.
- Checked count/control/data arithmetic, exact and maximum size helpers, explicit
  count/byte/work budgets, complete preflight and caller output capacities. Every
  Result failure leaves the entire output unchanged. Successful operations
  preserve suffix bytes/elements. No dynamic storage is allocated by this codec.
- A documented **host-specific** `VSPA` little-endian archive format, version 1,
  schema 1: an ordered rooted tree of u32 values, borrowed UTF-8 labels and child
  RelPtr32 arrays. Signed displacements originate at their own offset fields.
  RelSlice32 includes a checked count. This is not a generic source object-graph
  archive, source ABI or compatibility promise with an external archive format.
- Full actual-byte validation before immutable `Archive`/`NodeView` publication:
  version/schema, exact length/counts, signed ranges, four-byte logical alignment,
  exact canonical node/payload positions, zero reserved/padding bytes, label
  UTF-8, target record boundaries and rooted tree/depth rules. Cycles, sharing,
  backward-index edges, orphans, aliasing payloads and trailing bytes fail.
  Parent records precede children, enabling iterative depth validation. Relative
  child entries in the payload are negative offsets back into the node table.
- Archive preflight and validation use a bounded fallible temporary metadata Vec;
  node ranges must fit actual input bytes **before** allocation, even when caller
  count/allocation limits are raised. `validation_bytes` exposes requested
  metadata payload bytes. Root depth is one; an explicit iterative 200-node chain
  succeeds with depth 200 and fails under default depth 128.
- Archive encoding borrows caller nodes, preflights all schema/size/limit/capacity
  checks and leaves all output unchanged on failure. No caller owners are
  consumed. Validated views retain only the immutable input slice, not the
  temporary metadata. `label_bytes` borrows in constant-size work; safely creating
  `label()` rechecks UTF-8 in O(label bytes), without allocation/copying.
- Existing JSON APIs and error categories remain unchanged. Binary errors live
  separately in `binary::{Error, ErrorKind}`. No dependencies, root manifests,
  lockfiles, CI, CLI or other crates changed.

The crate README specifies every header/record field, canonical payload order,
signed offset origin, empty-slice representation, logical alignment, supported
schema and limits. The self-checking example is `binary_codecs`.

## Evidence

Used private `/tmp/vesper-serde-binary-target` with one build job, zero dev/test
debug information and incremental compilation off. The target is approximately
8 MiB; no temporary workspace stanza or leaf lockfile is needed.

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-serde-binary-target cargo test --manifest-path crates/vesper-serde/Cargo.toml --all-targets
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-serde-binary-target cargo test --manifest-path crates/vesper-serde/Cargo.toml --doc
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-serde-binary-target cargo clippy --manifest-path crates/vesper-serde/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path crates/vesper-serde/Cargo.toml -- --check
```

All pass: **17 binary tests, all 25 existing JSON tests, two executable Rustdoc
examples**, and strict all-target Clippy. The example executes assertions:

```text
StreamVByte: 5 values in 13 bytes; scalar reference
Archive v1/tree1: 2 nodes in 124 bytes; validated borrowed views
```

Tests include hand-calculated fixed StreamVByte and full archive byte vectors;
an independent quotient/remainder StreamVByte oracle across counts 0–512;
all 256 control bytes and every u32 width boundary; every truncation and small
output length of reference frames; non-minimal widths and unused tail tags;
byte-split reconstruction (no streaming-parser claim); signed i32 minimum/maximum
and checked usize multiplication/addition overflow; every resource budget and
exact work/metadata boundaries; malformed header/version/schema/count/range/
alignment/reserved/padding/UTF-8; cycles/sharing/orphans/non-node targets; 99 real
binary trees, a 200-node chain and 864 archive byte mutations with safe view
inspection whenever validation accepts. Actual allocator failure is handled by
`try_reserve_exact`; it was not fault-injected. Allocation-limit failures are
exercised directly.

Independent external interoperability check (not a CI/dependency requirement):
read and compiled the upstream fast-pack/streamvbyte scalar C encode/decode and
endian helpers at commit
[`60286c56cc61f16dfbe003d6e1ac84ad48ad4e8b`](https://github.com/fast-pack/streamvbyte/tree/60286c56cc61f16dfbe003d6e1ac84ad48ad4e8b).
The files were kept only under `/tmp/vesper-streamvbyte-reference`; no third-party
source was vendored. A minimal temporary header disabled ISA macros, so upstream
scalar paths compiled with `cc -std=c11 -O1`. Generated frames for counts 0–512,
using u64 recurrence `state = state * 6364136223846793005 + 1` from seed
`0x43211abc123`, with value `(state as u32) >> ((lane % 4) * 8)`. Upstream
validation/decode succeeded, then a temporary Rust checker decoded the upstream
frames and compared our encoded bytes exactly. Result:

```text
upstream C scalar corpus: 513 frames, 131328 values
Rust/upstream-C scalar byte and value agreement: 513 frames, 131328 values
```

A read-only peer review from the Tensor lane found no correctness blocker.
The hostile-count range-before-allocation regression was added during final
resource review. No peer edits or additional agents were created by this lane.

## Explicit limits and integration

Archive defaults: 4 MiB encoded bytes, 4,096 nodes, 4,095 children per node/total
edges, depth 128, 64 KiB per label, 1 MiB total labels, 128 KiB requested metadata,
16 MiB logical work. Limits may be configured, while the supported V1 wire span
always stays within `i32::MAX`. Counts/lengths must fit wire u32 fields. Temporary
allocation limits describe requested vector payload, not allocator bookkeeping,
capacity rounding, process RSS or caller allocations. Work units are deterministic
resource charges, not exact CPU/time measurements.

StreamVByte defaults: 262,144 values, 2 MiB encoded bytes, 8 MiB logical work.
Malformed/truncated decode never needs external input padding. The caller is
responsible for framing/count transport; raw StreamVByte has no embedded count,
schema ID or authenticity protection.

No source derives, Vesper Region/mode enforcement, linear resource decoding,
SIMD/native acceleration, mmap, device execution, arbitrary object-graph schema,
cryptographic validation or performance claim is provided. No root registration
patch is needed for the already-registered crate. Coordinator CI can add:
`cargo run -p vesper-serde --example binary_codecs`.
