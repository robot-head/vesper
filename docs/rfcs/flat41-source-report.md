# Region flat table source evidence (#221)

## Ownership and implementation

This lane owns only `crates/vesper-alloc/src/region/flat.rs`,
`crates/vesper-alloc/tests/flat_core.rs`, and this report. The frozen contracts
were read before implementation; publication `be2d342` authorized source work.
Source commit `cdf60cb` was supplied before coordinator registration. Follow-up
`da02a88` isolates complete-group stack initialization and adds actual core tests.
No existing allocator/Vec/tiled algorithm, error enum, dependency, build file or
shared registration was edited by this lane.

`flat.rs` forbids unsafe code. The move-only owner contains one RegionVec of
actual aligned groups. Each group contains a 16-byte control array, 16 owned
Option entries with cached full hashes, and the region-resident header; only
group zero carries live length/configuration. Empty tables initialize one group.
Growth reserves the exact power-of-two group count, initializes all groups and
then rehashes using cached hashes and ownership-preserving Option displacement.
Every placement consumes an unplaced entry. This needs no payload scratch
collection, Hash/Eq callback, clone or destructor during rehash.

Constructor configuration/layout/count/byte preflight precedes allocation and
full Group values. Both empty-group construction and initialization are kept in
separate non-inlined functions so unsupported large generic layouts do not put
complete Group stack frames in the constructor's preflight. A representable
group larger than 16MiB is refused on an ordinary native test-thread stack.

Hash/Eq callbacks run only after checked handle validation and before the
operation's allocation or mutation. Searches test all matching candidates in a
group before EMPTY termination and are bounded by actual group count. The
separate insertion vacancy pass uses only cached hash/control bytes. DELETED
slots preserve surviving search chains, including when no EMPTY remains.
Replacement returns the actual old key/value; insertion failures return the
actual incoming tuple. Values are mutable but keys are shared only.

Two-value projection resolves both slots before safe same-group/different-group
slice splits. Actual occupied-slot identity detects aliasing, including ZST
values. Returned references, control views and the named exact/fused iterator
borrow Region rather than the owner. The iterator visits physical group/lane
order and allocates nothing.

Clear takes each entry, marks DELETED and decrements resident length before
Drop, preserving surviving table invariants if a destructor panics. Complete
clear resets controls to EMPTY. RegionVec retains responsibility for allocation
identity, relocation, watermark protection and ordinary affine finalization.

## Actual native evidence

Ten core tests execute the following public behavior:

* Default/invalid configuration, exact initial power-of-two capacities and live
  limits, initialized EMPTY groups, and owner region identity.
* Actual owned String insertion, replacement without growth, growing insertion,
  readback, removal and clear with retained capacity.
* Reserve uses resident length plus additional, with unchanged stats/entries on
  configured-cap and arithmetic refusal; full-cap insertion returns its tuple.
* Mutable values and ordered disjoint projections, aliases, each missing/present
  combination, duplicate misses, and ZST key/value slot identity.
* Exact/fused iterator progression, completeness after tombstones, and a returned
  iterator that remains valid after its owner handle goes out of scope.
* Actual DefaultHasher-derived h2 controls, EMPTY/DELETED tags and bounded control
  inspection; affine fields requiring neither Copy, Default nor Debug; error
  source conversion and stale input ownership.
* Unsupported complete entry layout before capacity/allocation, invalid-config
  precedence, and refusal of an oversized representable group above native
  test-thread stack size.

Six new compile-fail Rustdocs check owner cloning, nonstatic payload insertion,
real static function-pointer value-type invariance, immutable iterator keys,
views excluding growth and controls excluding rewind. A positive function
pointer coercion verifies that the invariance witness tests the owner rather
than relying on a vacuous static bound.

Final stable leaf commands use `/tmp/collections41-flat-source-target`, one job,
dev/test debug0 and incremental0. Coordinator registration and arena source are
dependency-only: `5fcb1b3` copied as `a06b4e4`, `4e37bbe` as `568c972`, and arena
fix `dde30ee` as `b3c5771`; none is an owned flat implementation commit.

* `cargo test --locked -p vesper-alloc --all-targets`: **85 passed**, zero failed
  or ignored: 72 existing allocator tests, three arena dependency unit tests,
  and ten new flat core tests.
* Strict all-target package Clippy with `-D warnings`: passed.
* Formatting and diff checks: passed.
* `cargo test --locked -p vesper-alloc --doc`: **25 passed**, comprising four
  runnable examples and 21 compile-fail examples across the actual package.

An initial no-Debug test fixture used Result::unwrap_err, whose success-type
Debug bound was inappropriate; the fixture now explicitly matches Err without
adding Debug to the payload. An initial strict run was blocked only by the arena
dependency's deprecated atomic API; its owner supplied `dde30ee`, and the final
strict run passes without suppressions. No runtime assertion was weakened.

## Scope

Independent model/lifecycle/layout suites and coordinator Miri are separate
lanes and are not claimed as executed here. Controls/probing are scalar; this
does not establish SIMD execution, static source-region/model brands, Vesper
linear storage, source-language lowering, proof, performance or process RSS.
Inline K/V objects live in the region; caller-owned String/Vec/Box fields can
already own indirect heap allocations. Hash/Eq must remain stable, and arbitrary
user callbacks/Drop and OS allocation have ordinary Rust panic/physical-OOM
scope rather than a CPU-time or universal rollback guarantee.
