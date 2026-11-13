# Verified portable CAS closure transfer (#196–199)

Public claims and frozen file/API contracts preceded all three writers. The
shared wire/resource scaffold was reviewed and published first. A pre-writer
amendment extended the exclusive Store lane to bound occupied-object verification
by the already-hashed staged size plus one probe byte, including the locked
repair recheck. The path-resolution resource exclusion was also published before
the importer depended on it. Integration preserves landed owner main e2b8bba,
including Resonance and SimSoftNIC, without source conflicts.

The actual exporter verifies every reachable object from one explicit root,
strict tree encodings and every file edge size. It emits deduplicated canonical
name-order DFS postorder records, with root last, using existing domain/kind
hash contexts and object/tree schemas. Same-open bounded regular-file reads
hash the bytes returned in the pack. Cached subtree heights enforce the full
depth limit even when a shared subtree is reached later on a deeper path.

The actual importer checks the caller's expected root, complete wire/schema/
tag/length/hash/tree/closure/order/size/EOF constraints and all logical resources
before Store::open or destination writes. It retains validated tree plans and
prepays staging, bounded publication verification, tree encoding and fixed path
envelopes. No later Budget call or arithmetic can return a transfer Limit after
opening the store. Actual inserted/reused counts come from existing verified,
children-first CAS publication. I/O failures may leave verified immutable objects;
invalid or resource-refused packs cannot initialize or change the destination.

Actual coordinator Linux validation of the complete integrated feature:

- `cargo test --locked -p vesper-loom-cas --all-targets`: 85 tests across six
  targets, zero failures or ignores (12 library/shared, 34 existing Store,
  10 export, 14 import, 15 independent acceptance; example harness has no tests).
- The executable Rustdoc passes. Strict all-target crate Clippy, workspace
  formatting and whitespace checks pass.
- The actual transfer example verifies, imports, reexports and materializes
  its 229-byte, two-object source closure.
- Independent acceptance uses manual wire encodings and separately derived
  BLAKE3 contexts across every domain/kind. Actual separate processes transfer
  pack files and stdin bytes, then verify readback/materialization. The built
  example also passes export/import through files and a direct OS pipe with a
  242-byte artifact closure; reexport bytes are identical.
- Hostile cases cover all truncations, hashes, tags, versions, names, ordering,
  duplicate/extra/missing records, file sizes and configured limits, with both
  absent-parent and existing-store/derivation snapshots. Tests cover cached-height
  depth refusal, unsupported filesystem objects, large sparse corruption,
  same-size corruption, concurrent repair/reuse and verified-prefix I/O failure.
- An independent read-only reviewer found no actionable issue in shared resource
  preflight, exporter/Store changes or importer validation/publication ordering.

Owned leaves: 45f34b4/638a789 (export/Store), 5b88772/2d8a4ae (import), and
f8ae5fa/837dde3/1838748/bd3b23d (independent acceptance). Dependency-only
registration/source copies were not reimported as authored work. Coordinator
logs /tmp/caspack41-root-{final,doc,clippy,example}.log record successful execution;
final and doc exit markers are zero. No dependencies, Cargo, BUILD, workflows,
root CLI/compiler/native, Forge or existing hashing schemas change.

Limits count conservative cumulative logical requests, including up to nine
64KiB publication buffer envelopes per record. Individually maximal fields need
not fit together. Caller data, allocator bookkeeping/RSS, OS I/O and lock-wait
duration, resolved canonical destination-root prefixes and their legacy Store
path copies are outside the stated admission caps. No physical-OOM recovery,
filesystem-wide rollback or permanent protection from external mutation is
claimed. Content identity transfers no derivation, mutable root, signature,
trust, Lean validity or execution approval. This is a transport-neutral
distribution prerequisite, not HTTP3/zstd, registry authentication/TUF, signing,
garbage collection or formal proof. Hosted/platform checks remain separate.
