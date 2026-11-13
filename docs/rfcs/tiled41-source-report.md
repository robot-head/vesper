# Two-column region tiles: source claim207

Frozen contract37f9097 preceded implementation. Owned source commit9919666 adds
only `crates/vesper-alloc/src/region/tiled.rs`; follow-up6c03c6d fixes the source
doctest and strict-lint spellings. Actual public registration3529ef5 was imported
as dependency2e28a59 and belongs to coordinator206. This lane additionally owns
`tests/tiled_core.rs` and this report. No existing allocation algorithm,
RegionVec/slot metadata, Cargo/dependency, compiler, CLI, build or workflow path
changed. No push, PR or merge performed.

`RegionTiled2<A,B,N>` owns one checked RegionVec of actual repr(C) column tiles.
Each tile contains separately64-byte-aligned contiguous MaybeUninit field arrays
and its common initialized-row count; the outer owner has no second length.
Only initialized prefixes become slices. The two documented unsafe blocks
convert those actual aligned initialized arrays to shared/exclusive field
slices. Mutable projection splits distinct fields from one region borrow,
including when A==B. All other logic uses safe checked RegionVec operations.
No Default callbacks or element destructors are introduced.

Constructors check widths8/16/32/64, nonzero fields/alignment<=64, checked actual
layout and complete64KiB tile cap before allocation or constructing a full tile
value. Row-to-tile rounding, addition, tile bytes and isize bounds are checked.
Unsupported owners cannot be minted. Existing finite region bounds, serial
validation, failure atomicity and watermark protection remain authoritative.
Failures preserve original insertion pairs and consuming-release owners.
Premark mutations inside reserved capacity survive scratch rewind; this API
does not claim transactional snapshots of existing collections.

Actual Linux validation in exclusive `/tmp/tiled41-source-target`, one job,
debug0/incremental0:

- `cargo test -p vesper-alloc --all-targets`:81passed, zero ignored;
  72existing allocator tests plus9new source integration tests.
- `cargo test -p vesper-alloc --doc`:16passed, zero ignored, including owner
  cloning, non-Copy/nonstatic fields, projection borrow safety and actual
  invariant-wrapper rejection. A separate positive normal function-pointer
  coercion demonstrates the subtyping used by that compile-fail regression.
- `cargo clippy -p vesper-alloc --all-targets -- -D warnings`:passed with no
  lint suppression. Private tuple-result spelling aliases preserve the frozen
  underlying public slice-pair types.
- Workspace fmt check and diff whitespace check:passed.

The9new tests exercise all widths against independent logical row vectors,
growth/pop/reuse and initialized tails, actual capacity rounding and partial
tail reservation, Copy values without Default/Debug, a refusing backing proving
shape rejections make zero allocation requests, every unsupported shape category,
row/addition overflow with unchanged stats, actual bounded-region insertion
failure with intact values/rows, protected growth and retry, consuming owner
recovery after cross-region rejection, and stale out-of-range/no-op validation.
Independent layout/lifecycle claims208/209 provide separate evidence.

Logs: `/tmp/tiled41-source-tests.log`, `/tmp/tiled41-source-doc.log` and
`/tmp/tiled41-source-clippy-final.log`. This lane makes no hosted CI,
Windows/macOS runtime or Miri claim. It implements two hot host columns, not a
cold side array, source-language TiledSeq/field-tree lowering, NDJSON ingestion,
SIMD throughput or Lean refinement. Those remain separate integrations.
