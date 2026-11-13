# Region flat tables and typed slot arenas (claims 220–226)

This implements two actual scalar region-backed Rust host collections from
§15.5: `RegionFlatTable<K,V>` and `RegionSlotArena<T>`. Both support non-Copy,
Drop-bearing affine payloads and use existing checked RegionVec ownership.
Each new implementation module forbids unsafe code. Inline objects and all
persistent collection metadata reside in one region vector per owner.

## Coordination and review

The coordinator checked all open issues and actual external PR file lists
before claiming #220 and its six exclusive children. Reviewed contracts were
pushed at `be2d342` before source/test writers started. Independent test lanes
waited for coherent source and actual public exports. Source, model, and
lifecycle/layout/example writers used six separate worktrees/private caches;
the coordinator alone registered modules and reexports.

The latest fetched design revision adds §42 at modification time
`2026-11-13T04:46:25.948Z`; its normalized snapshot SHA-256 is
`8df79941a11ca2d777d8d5c5e7f4db26ef1a57f06babe2e71f3951a1f676883d`.
Section 15 is byte-identical. Snapshot refresh #227 / PR #228 is separate.

Landings through main `4fcb698` were imported at `79c5d17`, preserving the
owner's Vault, Crucible, TLS and CI work and our merged editor, macro, serde and
tiled-storage changes. The only registration conflict combined the existing
tiled exports with the new flat/arena exports. Existing Vec/slab/tiled
algorithms, allocator hooks, error enums, Cargo/dependencies, BUILD/workflows,
compiler/core AST/native and CLI files are unchanged by this collection diff.
External formatter #217/#218/#219 and integration #136 remain separate.

Independent read-only cross-component reviews found no actionable defect.
The arena review traced owner/key/bounds validation, nonwrapping identity and
generation issuance, failure ownership, free-list/reserve accounting, disjoint
views, lifetimes and inherited reclamation. The flat review traced bounded
probing/EMPTY termination/tombstones, cached-hash displacement termination,
callback boundaries, replacement/clear-panic ownership, disjoint ZST handling
and exact-capacity/layout preflight. These are source reviews, not proofs.

## Behavior

The flat table stores aligned sixteen-byte control groups alongside inline
owned entries and cached full hashes. Probing is bounded even without EMPTY
controls. Growth uses exact power-of-two reservations and a callback-free owned
displacement rehash without a scratch payload collection. Replacement/removal
return actual ownership; clear restores a valid surviving table before each
destructor runs. Read-only controls expose actual layout for inspection.

The arena stores a resident header and vacant/occupied entry enum values.
Opaque invariant typed Copy keys require no payload trait bounds. Checked
process IDs and arena-local generations never wrap or issue MAX. Access checks
owner, arena, slot and generation; removal returns the actual payload and
reuse cannot revive old keys. Reservation counts vacancies. Safe slice splits
provide ordered disjoint mutable projections in both collections.

Complete Group/Entry layouts are checked against 64 KiB before allocation or
construction. Non-inlined helpers keep full storage temporaries out of generic
constructor preflight; actual greater-than-16-MiB representable layout-refusal
regressions pass without constructing their payloads. Existing RegionVec
watermark, relocation, stale-handle and ordinary affine finalization rules
remain authoritative.

## Actual combined validation

Coordinator Linux stable Rust 1.99.1 checks ran after all implementation/test
changes, through `8f25b8c`; subsequent lane commits add evidence reports only.
One build job, debug0, incremental0, coordinator-only target cache:

- `cargo test --locked -p vesper-alloc --all-targets`: **170 passed**, no
  failures or ignored tests, 17 targets including example harnesses.
- `cargo test --locked -p vesper-alloc --doc`: **31 passed**: five runnable
  and 26 compile-fail cases.
- Strict all-target package Clippy, workspace formatting, whitespace checks,
  and `python3 tools/bazel/sync_build_files.py --check`: passed.
- Actual `flat_table` example: independent BTreeMap comparison over 512 mixed
  operations; final rows55, groups8, control_bytes128.
- Actual `arena_graph` example: initial cycle sum12, updated46, repaired44,
  two remaining nodes, stale edge rejected and removed value caller-owned.

Native model tests include 8192 deterministic mixed arena operations and a
2400-operation affine flat-table oracle after 300 initial growth inserts.
Independent backing/layout/drop suites check actual region allocation ranges,
control alignment/stride/tags, exact ownership, returned failures, watermarks,
relocation, stale/foreign/no-op paths and single clear-destructor panic recovery.

Actual nightly Rust 1.102.0 Miri with `-Zmiri-strict-provenance` passed **58
targeted checks**: lib4, flat_core10, arena_core8, flat_model12,
flat_layout3, flat_region9, arena_region11, and the new foreign-arena model
case1. The long flat randomized oracle was filtered only for this targeted
Miri run; it passes in full natively. A separate full eight-case arena model
Miri run was still executing when this report was written; it is not counted
here. Any later result is recorded separately in the PR. No sanitizer result,
unexecuted Miri case, hosted check or other-platform run is inferred.

Local logs use `/tmp/collections41-{native,doc,clippy,fmt,example-flat,example-arena}.log`
and `/tmp/collections41-miri-*.log`. Component contracts and six lane reports
record their own commands/dependencies; those totals are separate intermediate
trees and are not added to the combined totals above.

## Scope and reservation

This is scalar runtime-checked host infrastructure. It does not implement
Vesper source FlatTable/Arena or RegionRef lowering, static region/model brands,
linear-mode storage policy, SIMD, GC, Pin, concurrency, Lean refinement,
performance or RSS guarantees. Caller-supplied String/Box/Vec/Rc payloads may
already own transitive heap allocations outside the region cap. Ordinary Rust
callback/panic/physical-OOM boundaries apply. Public keys have no forge API.

Claims 220–226 remain reserved until reviewed protected merge after the required
checks pass. Do not treat an open PR or local verification as released ownership.
