# CAS closure-pack import (#198)

The published contract and shared scaffold `aff02c4` preceded implementation.
The later path-scope clarification `804801a` was read before final validation.
Owned changes are limited to `transfer/import.rs`, `tests/transfer_import.rs`
and this report. Existing CAS schemas, hashing contexts, derivations, ingestion,
materialization, dependency/build files and shared transfer APIs are unchanged.

The initial owned source is `5b88772`. Actual tests use approved dependencies:
export/store original `45f34b4` copied as `87f720b`, export registration
`bae12a9` copied as `70461b3`, and import registration `6cd513f` copied as
`2c43240`. These three copies are not import-owned changes and must not be
integrated a second time. An initial wrong registration order caused a local
cherry-pick conflict; that attempt was aborted and the dependencies reapplied
in their proper order before the successful runtime checks below.

## Validation before destination initialization

The importer checks positive/HARD limits, complete fixed header/version/schema,
the caller-provided expected root, record count/lengths/tags and exact outer EOF.
It borrows payloads from the caller pack, hashes those same bytes with their
actual domain/kind context, rejects duplicate IDs and retains strict decoded
Trees. Shared preflight pays tree names/maps/error/encoding resources before
Tree::decode; canonical reencoding is checked before any destination access.

The graph pass resolves actual full IDs, checks every recorded file size against
the verified blob payload, and traverses canonical name order. First completed
visits must match the next wire record index, proving the exact deduplicated DFS
postorder and rejecting missing, injected, duplicate and reordered closures.
The root must be last; a blob root therefore has exactly one record. Gray states
reject cycles, and cached subtree heights enforce the full depth bound when a
shared subtree is reached again through a deeper path. Lookups, edges, visits,
state vectors and all checked arithmetic receive finite prepaid charges.

Before Store::open, the importer also admits the complete publication envelope:
nine cumulative64KiB Store buffers per record (one staging buffer plus up to
eight occupied-object checks), bounded staged-size+1 read/hash probes, staging
copy/hash/write work, fixed relative path/ID/retry storage, tree-child path slots,
and a conservative32-times caller-input-path envelope. Shared tree preflight
already pays eventual Tree::encode buffers/copies. The filesystem-resolved root
prefix and its legacy copies are expressly excluded with caller-path resolution.

After that boundary the code performs only existing Store::open,
put_blob_expected and put_tree operations, children before parents, and infallible
bounded report accounting. There is no Budget call, pack validation or arithmetic
capable of returning a TransferError::Limit after opening the destination.
Inserted/reused counts come from actual Store publication outcomes. Existing
valid objects are reverified/reused and corrupt ones repaired through the bounded
publication discipline supplied by the export/store lane.

## Actual local checks

Used exclusive `/tmp/caspack41-import-target`, one Cargo job, debug0 and
incremental0. Actual final results:

- `cargo test --locked -p vesper-loom-cas --all-targets`:60tests across three
  suites, zero failures/ignores:12library/shared tests, all34existing Store tests,
  and14new import tests. Separately owned export/independent-acceptance tests
  were not copied into this lane.
- The existing executable Rustdoc passes.
- Strict `cargo clippy --locked -p vesper-loom-cas --all-targets -- -D warnings`,
  formatting and whitespace checks pass.

The new runtime tests import real blob roots and shared Merkle trees in every
domain, materialize actual files, preserve executable metadata, and check actual
insert/reuse reporting. Every truncation is rejected both without creating an
absent destination and without changing an existing store's files, directories,
readonly metadata or actual derivation record. Valid hashes do not authorize
bad names, case collisions, unsorted/duplicate entries, malformed tree schemas,
wrong kinds, directory sizes or unsupported UTF8. Other cases cover root/tag/
version/count/length/hash/trailing errors, missing/extra/duplicate/reordered valid
objects, recorded file-size mismatch, all zero/above-HARD settings, every finite
limit and publication-only prepayment failures before initialization.

Additional actual tests cover a deeper shared path exceeding a cached subtree's
height, longer corrupt occupied objects, same-size corrupt children and parents
repaired before parent publication, eight concurrent imports with exact total
unique insertions, and a deliberate late Store I/O error. That I/O test leaves a
verified immutable child but returns no successful import report or root claim.

## Boundaries

All malformed/resource refusals happen before destination initialization. Store
I/O failures after admission may leave verified immutable objects; this is not
filesystem-wide rollback or immunity to out-of-band corruption. Logical caps
exclude allocator bookkeeping/RSS, caller data, canonical path resolution and
OS I/O/lock-wait duration. Existing safe Tree/Store allocations are prepaid by
their conservative logical envelopes; no physical-OOM recovery/fault injection
is claimed. These cumulative envelopes intentionally may refuse combinations
below individual object/byte/count caps rather than silently increasing limits.

This imports content identity only. It transfers no mutable root, derivation,
signature, admission, Lean-certificate validity or execution approval and runs
no build, network or user program. Hosted/platform/RBE and independent whole-pack
process/pipe acceptance remain separate coordinator evidence.
