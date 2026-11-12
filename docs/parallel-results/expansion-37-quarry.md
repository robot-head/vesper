# Expansion 37: Quarry host SQL core

Implemented a dependency-free bounded SQL host library in
`crates/vesper-quarry`, based on refreshed specification §24. The supported slice
replays deterministic CREATE/DROP TABLE migrations, parses SELECT/WHERE and
inner/left/right/full joins, checks typed named parameters and alias-qualified
column identities, infers output type/nullability across nested outer joins,
optimizes typed relational predicates, and executes actual rows.

Prepared queries and data are bound to exact revision-plus-schema snapshots;
stale plans and foreign same-revision catalogs are rejected. Schema/data updates
are atomic. `DataSet::set_rows(table, &rows, limits)` borrows supplied rows and
clones only after complete validation, preserving both stored state and incoming
rows on every failure. A regression test exercises type/table/name/configuration/
row/byte failures and retries the same retained input successfully. Duplicate
tables/columns/aliases/output names, ambiguous references,
type mismatches, malformed/unsupported syntax, non-finite floats, invalid data,
and resource exhaustion produce explicit errors. Expression dependencies come
from checked AST column nodes. There are no raw predicate text placeholders or
caller-supplied dependency assertions. WHERE pushdown crosses only preserved
outer sides. SQL NULL and bag-join behavior are exercised against expected rows.

## Verification

Run with one build job, zero debug information, disabled incremental compilation,
and a private target (`/tmp/vesper-quarry-target`):

```sh
cargo fmt --manifest-path crates/vesper-quarry/Cargo.toml --check
cargo test --manifest-path crates/vesper-quarry/Cargo.toml --all-targets
cargo clippy --manifest-path crates/vesper-quarry/Cargo.toml --all-targets -- -D warnings
cargo run --manifest-path crates/vesper-quarry/Cargo.toml --example quarry
```

All passed: one internal overflow/atomicity test plus 16 integration tests.
The differential test compares 3,584 query/input configurations across all join
kinds, NULL-containing relations, nullable predicates, AND, OR, and NOT. Separate
tests assert actual expected query rows, all nine Boolean/NULL combinations,
bag duplicates, multiple outer joins, typed scalar/bind values, catalog/data
invalidation, and byte/node/nesting/work/row/output failures. The self-checking
example prints two exact order rows with a nullable unmatched customer and
logical work `674 -> 577`. This is execution evidence, not a performance claim.

The complete dialect, unsupported syntax, hard ceilings, work/byte accounting,
nullability policy, and integration boundaries are documented in the crate
README. Limits are bounded above, including zero limits; arithmetic and catalog
revision overflow fail explicitly. The executor is a host row/nested-loop
reference, with bounded cumulative candidate materialization. Its temporary-row
and allocator-overhead accounting limitations are explicit.

## Exact coordinator registration proposal

1. Add `"crates/vesper-quarry"` to root `[workspace].members`.
2. Remove only the leaf empty `[workspace]` and leaf `[profile.dev]` /
   `[profile.test]` sections when integrating with the root workspace. Standalone
   package metadata is complete, and there are no dependencies to register.
3. Regenerate the coordinator-owned root lockfile. No leaf Cargo.lock is part of
   this handoff.
4. Workspace validation commands become `cargo test -p vesper-quarry
   --all-targets`, `cargo clippy -p vesper-quarry --all-targets -- -D warnings`,
   and `cargo run -p vesper-quarry --example quarry`.
5. A CLI adapter may call `HermeticCatalog::{apply_ddl,prepare}`,
   `DataSet::{new,set_rows}`, `PreparedQuery::{optimized,execute}` directly;
   source syntax/code generation is unnecessary. No CLI changes are in this lane.

## Refreshed design review: §§12–19,24–25

All assigned sections were reread from the exact refreshed per-section files.
Their illustrative code does not by itself establish native runtime, compile-time
linearity, or formal proof claims. Existing implementations are read-only here.

- §12 describes stack-switching ABI, exact-once linear CQE handling, portable vs.
  local fibers, simulation, and telemetry. Its queue skeleton cannot establish
  kernel I/O, stack preservation, native performance, or deterministic protocol
  execution on its own.
- §13 wire layouts require verified padding/bit validity and foreign-library
  isolation requires actual native validation. A plain Rust layout record does
  not demonstrate MPK/POE or nocapture behavior.
- §14 requests fixed/scalable ISA-specific execution and SIMD equivalence; its
  `bv_decide` examples conflict with this batch's explicit kernel-only proof
  constraint. No new kernel-only proof here uses SAT/native shortcuts.
- §15's `compact_log_segment` holds a linear Watermark over fallible `?` before
  rewind, which must obey the resumption-frame rule. The allocator example also
  needs checked alignment arithmetic before claims of a general safe allocator.
- §16 requires canonical/model coherence and two-way macro hygiene, with
  post-expansion confinement of actual resolved effects. Rust skeleton metadata
  checks cannot substitute for compiler-enforced source semantics.
- §17's WebSocket loop holds a linear WsSession across a Cancel-bearing race;
  cancellation needs an explicit safe protocol consistent with [T-Let]. A Rust
  route registration allocation flag cannot establish allocation-free handlers.
- §18 is cross-platform WinUI3/GTK4/SwiftUI plus SimAura. Its slot example indexes
  out of range on invalid slot IDs and assumes a functioning platform bridge;
  native rendering, accessibility, and frame-rate claims need platform evidence.
- §19 is now explicitly **tri-backend** DirectX12/Vulkan1.4/Metal4, with appended
  §§19.8–19.11 adding Slang, portable GPU backend abstractions, and target barrier
  emission. Earlier Windows-only/DXC/IOCP examples are not the complete current
  contract. The new sample emitter truncates u64 fence IDs to u32 Vulkan event
  IDs, hardcodes RenderTarget wait layouts, and assumes ComputeShading producer
  stages for Metal waits; those are not general hazard-equivalence evidence.
- §24's sample catalog overwrites duplicate table definitions and conflates
  self-join identities; its raw-SQL/caller-dependency optimizer cannot justify
  pushdown, particularly through nullable outer sides. The sample revenue
  `wrapping_add` conflicts with fixed checked-arithmetic semantics. This host
  slice corrects these issues for its declared grammar without implementing the
  source macro, WASI, SIMD/AoSoA, storage, page pool, WAL, or egglog integration.
- §25's endpoint projection/merge skeleton is a useful model but does not itself
  implement RDMA lifetime safety, a Raft service, partition/crash simulation, or
  compiler-enforced session exhaustion. The session/consensus proof excerpts
  require actual defined/extracted semantics before their broad claims apply.

Only the new Quarry crate and this lane report are changed. No compiler, syntax,
Forge, Pulse/SimPulse, Prism, FFI, allocator, global docs, CI, CLI, root manifest,
root lock, or unrelated lane files were modified.
