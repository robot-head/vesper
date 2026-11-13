# Region tiled columns: independent layout evidence (#208)

## Ownership and basis

This lane owns only `crates/vesper-alloc/tests/tiled_layout.rs` and this report.
The tests were drafted against the reviewed frozen contract `37f9097`, before
the actual source/reexport dependencies were imported. They use public checked
region methods and the addresses of actual returned slices, without accessing
private tile types or copying the implementation's internal layout helpers.

## Physical and semantic checks

Nine tests cover the following independent observations:

* Twenty physical profiles combine every width (8, 16, 32, 64) with `u8/u16`,
  `u32/u64`, `u128/u8`, `[u8; 17]/[u32; 3]`, and a 64-byte aligned Copy field.
  Four consecutive live tiles include a partial final tile. Actual addresses
  establish both 64-byte column alignments, scalar contiguity, the rounded
  second-column offset, and the complete tile stride including the count header.
  The profiles include both sub-64-byte and larger column payloads.
* Equal `u64/u64` columns expose simultaneous mutable slices with disjoint byte
  ranges. Interleaved writes and reads keep both projections live. A separate
  scalar row calculation checks the result through both logical row reads and
  shared projections, for every width. Repeated pop/refill/drain operations cross
  tile boundaries and check every initialized prefix after each operation.
* Copy fields without `Default` are inserted, removed, projected, mutated, and
  refilled. Every slice length matches the exact initialized prefix; removed
  bytes and unused capacity are never included in a public view.
* Both constructors reject representable unsupported widths, zero-sized fields,
  over-aligned fields, and oversized complete tiles without changing region
  statistics. Huge requested row capacity does not bypass shape validation.
* The exact 65,536-byte complete tile boundary is exercised with real values in
  two adjacent tiles. A distinct rejected shape has 65,536 field bytes before
  its count header, ensuring the bound includes the full padded tile.

All address arithmetic is inspection of safe returned references; the tests
contain no unsafe code. Borrow exclusion compile-fail documentation belongs to
the source/lifecycle lanes and is not duplicated in this test file.

## Validation

Actual source dependency `9919666` and public registration `3529ef5` were imported
as local dependency-only copies `58e0c54` and `438fd76`. These are not owned
layout commits. Source follow-up `6c03c6d` is dependency-only local `01268eb`.
Owned tests are committed as `0b5acd2`.

The stable leaf cache was `/tmp/tiled41-layout-target`, with one Cargo job,
development/test debug information disabled, and incremental compilation off.

* `cargo test --locked -p vesper-alloc --all-targets`: **81 passed**, zero failed
  or ignored (1 existing library, 23 existing collections, 36 existing region,
  12 existing slab, and 9 new independent layout tests).
* `cargo fmt --all -- --check` and `git diff --check`: passed.
* `cargo clippy --locked -p vesper-alloc --all-targets -- -D warnings`: passed.
* `cargo test --locked -p vesper-alloc --doc`: **18 passed**, including 15
  compile-fail examples and three runnable examples.

The initial source dependency exposed three strict Clippy lints and a vacuous
invariance compile-fail example. These were reported to the source owner and
fixed in `6c03c6d`; the final actual checks above pass with that dependency.
The replacement invariance witness uses static function-pointer field types
plus a positive ordinary function-pointer coercion example. No layout
assertion was weakened or removed to pass verification.

Coordinator-owned Miri uses a separate cache and is not claimed as an executed
layout-lane result.
No SIMD behavior, throughput, platform-wide success, or compiler lowering is
established by these bounded host-layout checks.
