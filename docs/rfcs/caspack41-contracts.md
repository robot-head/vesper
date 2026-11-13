# Verified portable CAS closure packs (§8.2.6)

Claims196–199 precede writers. Base main5d0922e. Fresh open issues and actual
external PR files139–147,149–151,163,165,173,193 show no CAS/serde overlap.
Claude121–136/main/native and193BUILD/Bazel files remain reserved, as do separate
Codex source/CI batches. Publish amendments before dependent source changes.

|Claim|Branch|Exclusive writes|
|---|---|---|
|196|caspack41/integration|shared transfer/mod.rs, module scaffolding/lib export, contract/integration report/review|
|197|caspack41/export|transfer/export.rs, narrow store.rs same-open bounded verified read, tests/transfer_export.rs, report|
|198|caspack41/import|transfer/import.rs, tests/transfer_import.rs, report|
|199|caspack41/acceptance|tests/transfer_acceptance.rs, examples/transfer.rs, docs/cas-transfer.md, report|

Separate worktrees/private small caches, one Cargo job/debug0/incremental0; root
cache coordinator-only. No Cargo/dependency/root main/native/compiler/Forge,
BUILD/workflow/wrapper changes. No existing unrelated test rewrites. Claims stay
reserved through reviewed merge. Coordinator's empty development modules are not
implemented transfer features; public reexports follow actual implementations.

## Frozen API and records

`vesper_loom_cas::transfer` provides:

```
export_pack(store: &Store, root: ObjectId, limits: TransferLimits)
    -> Result<Vec<u8>, TransferError>;
import_pack(store_path: impl AsRef<Path>, expected_root: ObjectId,
            bytes: &[u8], limits: TransferLimits)
    -> Result<TransferReport, TransferError>;
TransferReport { root: ObjectId, objects: usize, pack_bytes: usize,
                 payload_bytes: usize, inserted: usize, reused: usize }
```

TransferLimits public fields/default/HARD: max_pack_bytes64MiB/256MiB,
max_objects4096/16384, max_depth64/128, max_tree_entries16384/65536,
max_metadata_bytes16MiB/64MiB, max_work512Mi/2Gi. Positive settings only; validate
before allocations/I/O. Checked arithmetic and fallible reservations throughout.
Existing object/tree schemas and domain-separated BLAKE3 contexts do not change.

Shared TransferError implements Debug/Display/Error and wraps existing CasError;
variants InvalidLimits(&'static str), Limit(&'static str), InvalidPack(&'static
str), UnexpectedRoot{expected:ObjectId,found:ObjectId}, DigestMismatch{expected:
ObjectId,actual:ObjectId}, AllocationFailed, Cas(CasError). No trust/approval bit.

Frozen wire format, all integers little endian:

```
"VSPACK01" (8 bytes)
u32 pack version = 1
u32 object schema = existing OBJECT_SCHEMA_VERSION (1)
root ObjectId (existing34bytes: domain tag, kind tag,32digest bytes)
u32 record count
record count * (ObjectId34 | u64 payload length | exact payload bytes)
```

Header54bytes, record overhead42. Unknown tags/versions reject. Records are
deduplicated deterministic DFS postorder of the root, traversing each canonical
Tree.entries() name order. A repeated ID appears only at its first completed
visit; root is last (one record for Blob root). No missing, duplicate, extra or
reordered records, trailing bytes, foreign domain/kind interpretation or injected
derivations. All domains and both kinds are supported. Canonical pack equality
is scoped to this version and exact immutable closure, not authenticity.

## Resource/shared helper boundary

Coordinator supplies shared wire constants/DTOs and finite Budget helpers before
writers. Budget tracks work, cumulative metadata requests, cumulative owned
payload-capacity requests and aggregate tree entries. Payload buffers and one
final pack buffer each have max_pack_bytes bounds; they are not unmetered extra
metadata. Temporary allocation/copy envelopes are prepaid. These logical caps
exclude allocator bookkeeping, caller-owned input/path and process RSS.

Exporter may add only `Store::read_verified_bounded(id,max_bytes,&mut Budget)` as
a private transfer helper. Open actual regular object once using existing
nofollow helpers; inspect length, reserve bounded storage, read/hash those SAME
bytes and require actual expected domain/kind digest. Check/charge every explicit
capacity growth before reserve; avoid unbounded read_to_end and allocating the
full64MiB for every tiny object. Growing/corrupt files cannot bypass byte/work
bounds. Existing reads/ingestion/materialization/derivation behavior stays intact.

Before Tree::decode, preflight its fixed header/count and remaining aggregate
entry/metadata/work allowance, including strings/case-fold maps, BTree nodes,
canonical reencoding and eventual publication encode/copy envelope. Preserve
existing strict portable names, sorting/case collisions, kind/domain/size policy.
All graph lookups, edges, depth and shared DAG visits receive finite charges.

## Export and import semantics

Export verifies every object, canonical tree and recorded file size in the actual
reachable closure. contains() and file metadata alone are insufficient. Missing,
corrupt, wrong-size, unsupported filesystem or resource states return no pack.
No build/network execution or derivation record is exported.

Import validates the ENTIRE bounded byte pack and complete closure/order/root,
hashes, canonical tree encodings and file sizes BEFORE Store::open, destination
initialization or any write. It must prepay all later publication resources
before opening the store; a validation/resource error must not arrive after
publishing some objects. Retain validated decoded trees/plans as necessary.
Expected root is caller-provided, never accepted merely from the pack header.

After complete validation, initialize/open the explicit destination and publish
children before parents through existing verified put_blob_expected/put_tree.
Existing valid objects may be reused, corrupt objects repaired under existing
CAS publication discipline; report actual new/reused counts. I/O/store failures
may leave verified immutable objects but never unverified bytes or a transferred
derivation/trust/execution approval. No promise of filesystem-wide rollback or
permanent immunity to out-of-band corruption. No new mutable root/reference file.

## Actual acceptance and scope

Independent golden pack bytes, every domain/kind, shared DAG and deterministic
ordering; actual separate-process/file/pipe transfer to a second store followed
by verified readback/materialization; executable flags and opaque artifact/
certificate-shaped bytes unchanged. All truncations/tampered hashes, wrong roots,
missing/extra/duplicates/order/trailing, malformed/case-colliding/path trees,
file-size and every configured bound must reject before destination creation or
change existing store/derivation contents. Cover corrupt source, concurrent
imports, reuse/repair and independent materialized copies. No weakened caps.

This is a real transport-neutral prerequisite for distribution, not HTTP3/zstd,
registry authentication/TUF, signing/quorum admission, garbage collection,
source-language effect confinement or Lean certificate validity. Payload identity
does not authorize execution. Actual local tests/examples/strict Clippy required;
hosted/platform/RBE claims await execution.
