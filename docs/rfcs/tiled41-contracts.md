# Region-backed two-column AoSoA host foundation (§15.5, §20.5)

Claims206–209 were created before source writers. Base main2d58c85 includes
merged194. Coordinator checked open claims and actual external PR files for
allocator overlap. Claude121–136/root CLI/core/Cargo and all other externally
owned paths remain reserved. Publish interface amendments before dependent
source changes. This reviewed contract is frozen before runtime implementation;
source writers start only after its publication.

|Claim|Branch|Exclusive writes|
|---|---|---|
|206|tiled41/integration|this contract, region.rs/lib.rs module registration and actual reexports, integration/review report|
|207|tiled41/source|new `crates/vesper-alloc/src/region/tiled.rs`, new `tests/tiled_core.rs`, source report|
|208|tiled41/layout|new `crates/vesper-alloc/tests/tiled_layout.rs`, layout report|
|209|tiled41/lifecycle|new `crates/vesper-alloc/tests/tiled_region.rs`, new `examples/tiled_columns.rs`, new `crates/vesper-alloc/TILED.md`, lifecycle report|

All test/example paths in the table are under `crates/vesper-alloc/`; reports
are `docs/rfcs/tiled41-{source,layout,acceptance,integration}-report.md`.
Separate worktrees and private small Cargo caches, one job, debug0 and
incremental0. No Cargo/dependency, BUILD, workflow, wrapper, root CLI/core AST,
compiler, existing allocator algorithms, or unrelated test rewrites. Root alone
registers an empty development module before source; actual public reexports
follow coherent implementation. Claims remain reserved through reviewed merge.

## Actual profile and public types

This is an actual array of two-column tiles in bounded region storage, not an
AoS `Vec<Record>` or two unrelated global column vectors. The public move-only
owner is:

```rust
pub struct RegionTiled2<A: Copy + 'static, B: Copy + 'static, const N: usize> {
    // Private RegionVec<Tile2<A, B, N>> only; no independent length counter.
}
pub const HARD_MAX_TILE_BYTES: usize = 64 * 1024;

pub enum TiledError {
    UnsupportedTileWidth { width: usize },
    UnsupportedFieldLayout,
    Region(RegionError),
}
```

`RegionTiled2` has Debug without requiring A/B:Debug and an inspection-only
`region(&self) -> RegionId`. It is neither Clone nor Copy. Existing RegionVec
type invariance and never-reused region/slot/serial identity are retained by
the private owner. No public raw handle constructor, byte reinterpretation,
nonstatic borrowed field types, mutable frozen alias, serde reconstruction or trust token
is added. TiledError implements Debug, Display and std::error::Error, with the
wrapped RegionError exposed as its error source. Existing public error enums
are not extended. Allocation errors convert through RegionError::Alloc.

N must be8,16,32 or64. Both fields must have nonzero size and alignment at most64.
The complete actual tile layout, including both rounded column wrappers, the
initialized-count header and final padding, must be at most HARD_MAX_TILE_BYTES.
Unsupported N returns UnsupportedTileWidth; unsupported fields or oversized
tile return UnsupportedFieldLayout. Pure layout validation precedes region
allocation and the creation of any complete Tile2 stack value. Constructors
use only type/layout information and delegate allocation to RegionVec. Checked
layout arithmetic, Layout validity and isize size limits apply before requests.
An unsupported shape cannot mint an owner, so later operations may rely on its
immutable N/field-layout validation without constructing oversized tile values.
These runtime errors apply to representable Rust types/array layouts. Rust may
reject an unrepresentable array/type layout during monomorphization; this API
does not promise to instantiate impossible Rust types merely to return an
error. That is compile-time rejection, not an accepted unsupported runtime owner.

Copy fields need not implement Default. No field default constructor, user
closure or other user callback runs during insertion/removal. Drop-bearing and
linear field types are outside this profile and fail the Copy bound.

## Frozen public methods

These methods are inherent on `Region<R>` for every `R: ChunkBacking`. Each
method's generic A/B bounds are Copy+'static and its const N matches the owner.
The notation below elides those repeated method generic declarations only.

```rust
tiled2_new<A, B, const N: usize>(&mut self)
    -> Result<RegionTiled2<A, B, N>, TiledError>;
tiled2_with_capacity<A, B, const N: usize>(&mut self, rows: usize)
    -> Result<RegionTiled2<A, B, N>, TiledError>;

tiled2_len(&self, owner: &RegionTiled2<A, B, N>)
    -> Result<usize, HandleError>;
tiled2_is_empty(&self, owner: &RegionTiled2<A, B, N>)
    -> Result<bool, HandleError>;
tiled2_capacity(&self, owner: &RegionTiled2<A, B, N>)
    -> Result<usize, HandleError>;
tiled2_tile_count(&self, owner: &RegionTiled2<A, B, N>)
    -> Result<usize, HandleError>;
tiled2_get(&self, owner: &RegionTiled2<A, B, N>, row: usize)
    -> Result<Option<(A, B)>, HandleError>;

tiled2_columns<'a>(&'a self, owner: &RegionTiled2<A, B, N>, tile: usize)
    -> Result<Option<(&'a [A], &'a [B])>, HandleError>;
tiled2_columns_mut<'a>(&'a mut self, owner: &RegionTiled2<A, B, N>, tile: usize)
    -> Result<Option<(&'a mut [A], &'a mut [B])>, HandleError>;

tiled2_reserve(&mut self, owner: &mut RegionTiled2<A, B, N>, additional_rows: usize)
    -> Result<(), RegionError>;
tiled2_push(&mut self, owner: &mut RegionTiled2<A, B, N>, first: A, second: B)
    -> Result<(), InsertError<(A, B)>>;
tiled2_pop(&mut self, owner: &mut RegionTiled2<A, B, N>)
    -> Result<Option<(A, B)>, HandleError>;
tiled2_release(&mut self, owner: RegionTiled2<A, B, N>)
    -> Result<(), OwnerError<RegionTiled2<A, B, N>>>;
```

`rows` is initial capacity, not a permanent length/cap ceiling. Capacity rounds
up to whole tiles and reports actual allocated row capacity. Zero capacity is
supported and consumes the existing tracked zero-element RegionVec allocation;
it contains no logical tiles or initialized rows. `reserve` follows existing
RegionVec amortized growth/fallback policy. There is no additional arbitrary
global row limit: RegionConfig's finite reservation/allocation/alignment limits,
checked row addition, row-to-tile rounding, tile-byte multiplication and isize
limits govern requests. Avoid overflow-prone `(rows+N-1)/N`; quotient/remainder
rounding is checked before allocation. Live tile-capacity times N fits because
the nonzero actual tile is larger than N bytes and its allocated byte count
fits isize; inspection may rely on that sealed invariant.

Handle validation occurs on every checked operation, including out-of-range
row/tile lookup and reserve0. A foreign/stale owner must not become Ok(None)
or a successful no-op. Out-of-range lookup on a valid owner is Ok(None).

Insertion failure preserves all existing rows, owner identity and returns the
original `(first, second)` tuple in InsertError.value. Reserve/overflow/protected
growth failures preserve rows and RegionStats as guaranteed by RegionVec. The
consuming release returns the original logical owner through OwnerError.owner
on validation failure; no ownership is lost. Successful release invalidates
the owner allocation under the ordinary checked RegionVec lifecycle.

## Private representation and safety boundary

Source207 owns all private types/helpers in tiled.rs; no new sibling-private
allocator hook is required. It uses existing checked public RegionVec APIs.
Only coordinator changes module registration/reexports in region.rs/lib.rs.

Each private column is `#[repr(C, align(64))]` with exactly
`[MaybeUninit<A>; N]` or `[MaybeUninit<B>; N]`. Private Tile2 is repr(C) and
contains first-column wrapper, second-column wrapper, then initialized:usize.
Both columns and each tile begin at64-byte-aligned addresses. Because field
alignment is at most64, tile/column alignment is exactly64. If
`column_bytes(T)=round_up(N*size_of::<T>(),64)`, the second column offset equals
column_bytes(A), and the tile stride is
`round_up(column_bytes(A)+column_bytes(B)+size_of::<usize>(),64)`.
The finite tile-size check covers that complete stride, not just field bytes.
The initialized counter is separate from field arrays; no per-row header or
interleaving interrupts either contiguous field column.

The underlying RegionVec's initialized element count is the number of live
tiles. Every nonlast tile has initialized==N; the last has initialized in1..=N;
empty collections have no live tiles. Total row length is derived from those
region-resident facts. No independent outer handle/global table count can
disagree after rewind, relocation, owner drop or forget.

To append within a tile, write the actual first/second values to the same next
lane, then publish the initialized count after both writes. There is no
allocation or user callback between the two writes/count update. Appending a
new tile delegates growth first or passes a fully valid one-row tile into
RegionVec push, preserving original row ownership on allocation failure.
Pop returns the last actual pair, lowers the initialized count and removes a
tile from RegionVec when its final row is removed. Hidden old Copy bytes are
not exposed after removal. All returned column slices have length exactly the
initialized prefix; capacity padding and inactive lanes never become `&A/&B`.

Minimize new unsafe to documented initialized-prefix casts/copy reads needed
for MaybeUninit. The safety argument must cite checked live owner access,
actual aligned field arrays, initialized count, distinct nonzero field ranges,
and borrow lifetime. `tiled2_columns_mut` obtains one mutable RegionVec tile
borrow, splits distinct struct fields, and returns the two projections from
that single borrow. Equal A/B types do not make the columns alias. Returned
views borrow Region itself; mutation, growth, rewind/reset and drop are excluded
while a view is live. No retained raw pointer or separately reopened region
borrow is used to manufacture projections.

## Watermarks, reclamation and exact scope

Existing RegionVec semantics are authoritative, not snapshot rollback:

* A collection allocated before a watermark survives scratch rewind. Mutations
  of its existing storage survive too, including appending rows to a partial
  tile and adding new tiles within capacity preallocated before the watermark.
* A premark buffer cannot grow beyond capacity while the watermark is live.
  An insertion needing growth returns ProtectedByWatermark unchanged with its
  original pair. Overflow errors likewise happen before mutation/allocation.
* A collection allocated after a covering watermark becomes stale after rewind,
  even if it relocated meanwhile. Reset and release also invalidate handles.
  Reusing slot indices/bytes does not revive old serials.
* Dropping/forgetting the move-only owner does not release region memory. Region
  rewind/reset/drop performs normal storage reclamation. Copy fields have no
  destructor glue; there is no field finalizer/linear-obligation claim.

This is two hot host columns only: no cold/page-aligned side array, complete
source-language TiledSeq/schema derivation, field-tree compiler projection,
NDJSON parser/ingestion, ISA vectorization/gather claim, Lean Array refinement,
VirtualArena/mmap provisioning or throughput claim. It is a real bounded host
layout and projection seam for later actual columnar ingestion. Safe caller
code may use copied row pairs as transient values; payload storage remains
columnar rather than an intermediate AoS collection.

## Required actual evidence

Source207: actual initialized-row/partial-tile append/pop, all widths,
no-Default Copy values, row/tile bounds and handle validation, constructor
unsupported width/ZST/overalignment/tile-size refusal, checked capacity/reserve
overflow, no mutation/allocation on shape rejection and failed insertion tuple.

Layout208: actual addresses of both columns, contiguous scalar element strides,
second-column offsets and consecutive tile stride for multiple field sizes and
widths (including columns shorter/longer than64bytes); exactly initialized tail
lengths; equal-type disjoint mutable projections and mixed shared/mutable use;
independent scalar row oracle after mutations. Use public projections, not a
private representation echo. Check pointer differences/alignment rather than
timing or an unsupported SIMD assertion.

Lifecycle209: counting/failing backing, finite region/allocation/alignment
bounds, owner movement/release failure, cross-region/stale checks on all
no-op/bounds APIs, both premark append cases, protected growth with original
rows/tuple/stats, postmark allocation/relocation invalidation, serial reuse,
reset, release, forgotten owner and exactly-once backing deallocation. The
actual example scans/mutates tile columns and compares logical rows with a
scalar oracle without claiming a parser or compiler path. Compile-fail doctests
cover owner cloning, non-Copy/nonstatic payloads, type invariance and views
surviving region mutation/rewind (source owns public API doctests; lifecycle
may add independent compile-fail crate documentation).

Each lane runs meaningful owned tests, all existing allocator tests where
appropriate, strict package all-target Clippy, fmt and diff checks in its own
cache. No hosted/platform success until executed. Existing allocator behavior
and tests must remain intact; do not relax caps or ignore tests for acceptance.
