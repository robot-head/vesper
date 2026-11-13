# Region collection implementation claims (§15.5)

Public claims220–226 precede all runtime/test writers. Open issues and actual
external PR files were checked for allocator overlap. Claude formatter217/218
and PR219 touch separate formatter/CLI paths; root CLI/native/Cargo136 remains
reserved. Tiled206–209 stays reserved in its own pending PR215. This independent
branch starts from landed main9f0e2f5, including ownerTLS173, ownerCI193, grammar194
and formatter proof142. It does not edit the tiled implementation.

| Claim | Branch | Exclusive implementation writes |
| --- | --- | --- |
|220|collections41/integration|contracts, narrow region.rs/lib.rs registrations/reexports, integration report/review|
|221|collections41/flat-source|region/flat.rs, tests/flat_core.rs, docs/rfcs/flat41-source-report.md|
|222|collections41/flat-model|tests/flat_model.rs, docs/rfcs/flat41-model-report.md|
|223|collections41/flat-region|tests/flat_region.rs, tests/flat_layout.rs, examples/flat_table.rs, FLAT.md, docs/rfcs/flat41-acceptance-report.md|
|224|collections41/arena-source|region/arena.rs, tests/arena_core.rs, docs/rfcs/arena41-source-report.md|
|225|collections41/arena-model|tests/arena_model.rs, docs/rfcs/arena41-model-report.md|
|226|collections41/arena-region|tests/arena_region.rs, examples/arena_graph.rs, ARENA.md, docs/rfcs/arena41-acceptance-report.md|

Runtime/test/example/profile paths are under crates/vesper-alloc/src or
crates/vesper-alloc as appropriate; report/contract paths are repository relative.
Per-component exact APIs and policies are frozen in flat41-contracts.md and
arena41-contracts.md after coordinator review, before dependent writers start.
Contract drafts are documentation only; no runtime feature is implied by them.

Each writer uses a separate worktree and private small target cache, one Cargo
job, debug0 and incremental0. The shared root target and coordinator Miri target
are coordinator-only. No existing Vec/slab algorithms, allocator metadata hooks,
tiled.rs, error.rs, Cargo/dependencies, BUILD/workflows, wrappers, compiler/core
AST or CLI edits. The coordinator alone registers empty development modules,
then actual public types after coherent source. New implementation modules
forbid unsafe code and use existing checked RegionVec APIs and safe slice splits.

These collections own ordinary affine Rust payloads, including non-Copy values
with Drop. Element objects live in region storage; caller-supplied payloads may
already own external heap allocations. No implementation-created host payload
fallback, static source-region brand, Vesper linear-mode storage guarantee,
RegionRef compiler lowering, SIMD execution, formal proof, throughput or RSS
claim is introduced. Panic/user callback and physical-OOM scope stays explicit;
no unchecked public key or approval constructor is added.

All claims stay reserved through reviewed protected merge. Source/test/interface
amendments are published before dependent edits. Actual native, ownership,
model/layout/lifecycle, strict Clippy, formatting and relevant strict-provenance
Miri results are required before completion; hosted/platform success remains
separate until executed.
