# Actual region column tile integration (#206–209)

Public claims, fresh issue/actual-PR file checks and reviewed contract37f9097
preceded the three exclusive writers. Base main2d58c85 includes merged grammar
work194. Coordinator registered the development module first, then exposed the
actual types after coherent source. No existing Vec/allocator metadata algorithm,
Cargo/dependency, BUILD/workflow, compiler, core AST or CLI changes are introduced.

RegionTiled2 owns an existing checked RegionVec of actual repr(C) column tiles:
two separately 64-byte-aligned field arrays and a region-resident initialized-row
counter. Valid widths are8/16/32/64. Copy fields need not implement Default;
zero-sized or over-aligned fields and complete tiles larger than64KiB refuse
before allocation/full tile construction. Rust may reject unrepresentable types
at compile time. Only initialized prefixes are exposed through shared or paired
disjoint mutable projections, including identical field types.

Every access validates the underlying region/slot/serial, even out-of-bounds and
no-op requests. Push errors preserve the original pair; consuming release errors
preserve the owner. Counts live in region storage. Premark mutations of reserved
storage survive scratch rewind, protected growth fails unchanged, and postmark
allocations become stale even after relocation or slot reuse. No payload Vec
fallback, independent outer row counter, user Default callback or field finalizer
is added. The two narrowly documented unsafe blocks expose initialized slices
from one checked tile borrow; they create no retained raw-pointer capability.

Actual coordinator Linux validation of the complete integrated feature:

- `cargo test --locked -p vesper-alloc --all-targets`:104tests across8targets,
  zero failures or ignores (72 existing,9 core,9 independent layout,14 lifecycle).
- All18 crate doctests pass, including meaningful function-pointer invariance
  rejection and its positive coercion control. Strict all-target crate Clippy,
  workspace formatting and whitespace checks pass.
- The actual tiled_columns example checks73 logical rows in5 actual tiles and
  reports initial column sums18615 and38836 against its scalar row oracle.
- Independent physical-layout tests cover actual column addresses, contiguous
  scalar stride, column offsets and tile stride across20type/width profiles,
  initialized tails and exact complete-tile size boundaries. Lifecycle tests
  cover failed backing, finite reservation/alignment/allocation bounds, foreign
  and stale handles, all watermark cases, returned ownership and exactly-once
  backing reclamation. The separate TILED.md examples/compile-fail checks passed
  in the acceptance lane.
- Actual nightly1.102.0 Miri with `-Zmiri-strict-provenance` passes all9 layout
  tests (131.52s) and all14 lifecycle tests (2.74s), zero failures or ignores.
  These runs exercise initialized-prefix and disjointness behavior plus rewind,
  relocation, staleness and reclamation; this is tested evidence, not a universal
  proof for every Copy type or a claim about another platform.
- An independent read-only reviewer found no runtime invariant or unsafe-code
  issue. Strict lint and an initially vacuous reference-lifetime doctest were
  corrected without suppressing source lints or weakening runtime assertions.

Owned leaves:9919666/6c03c6d/1a10dfb(source/core),0b5acd2/bf46395(layout),
7538159(lifecycle/example). Dependency-only registration/source copies were not
reimported as authored work. Coordinator logs are /tmp/tiled41-root-{final,doc,
clippy,example}.log and /tmp/tiled41-miri-{layout,region}.log; exit markers are
zero. Stable remains the default toolchain; the separate nightly/Miri install
and sysroot are validation tools only, with no repository dependency changes.

This is two hot Copy host columns, not cold/page-aligned side arrays, general
source TiledSeq/schema lowering, field-tree compiler proofs, SIMD instructions,
Lean Array refinement or completed NDJSON ingestion. Existing RegionConfig
reservation/metadata/alignment bounds and checked arithmetic remain authoritative;
there is no zero-whole-process-allocation, speed or RSS guarantee. Hosted and
other-platform checks remain separate from the actual local results above.
