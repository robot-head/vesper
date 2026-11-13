# Region-backed scalar flat table foundation (§15.5)

This contract is the coordinator-reviewed interface for claims 220–223. The
public claims precede source writers. It is frozen on base `9f0e2f5` after coordinator review; source
implementation is authorized after this contract is pushed. Interface
amendments must be published before dependent implementation changes.

| Claim | Branch/lane | Exclusive writes |
|---|---|---|
| 220 | collections integration | this contract, module registration/reexports in `region.rs`/`lib.rs`, integration report |
| 221 | flat source | new `crates/vesper-alloc/src/region/flat.rs`, new `tests/flat_core.rs`, source report |
| 222 | flat model | new `crates/vesper-alloc/tests/flat_model.rs`, model report |
| 223 | flat lifecycle/layout | new `crates/vesper-alloc/tests/flat_region.rs`, new `tests/flat_layout.rs`, new `examples/flat_table.rs`, new `FLAT.md`, acceptance report |

Tests, examples and `FLAT.md` in the table are under `crates/vesper-alloc/`.
Reports are `docs/rfcs/flat41-{source,model,acceptance}-report.md`; coordinator
owns the parent collections integration report. Claims 224–226 cover the
separate arena work and grant no permission to edit these flat-table paths.
Existing `vec.rs`, slab/tiled code, allocator algorithms, Cargo/dependencies,
BUILD/workflows, root CLI/core/compiler and external-agent paths are excluded.
Each lane uses a separate worktree and private small Cargo cache, one job,
debug information disabled and incremental compilation off. Dependency copies
are not reported as owned implementation commits. No push/PR/merge is delegated.

## Actual profile and public types

This is a real region-backed open-addressing table with 16-byte control groups
and inline entry objects. Control matching and probing are scalar Rust. It is
a host foundation for the design's FlatTable, not the completed language-level
Safe SIMD SwissTable.

```rust
pub const FLAT_GROUP_WIDTH: usize = 16;
pub const HARD_MAX_FLAT_GROUPS: usize = 4096;
pub const HARD_MAX_FLAT_GROUP_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatConfig {
    pub max_groups: usize,
}

// Default: max_groups == 1024.
impl Default for FlatConfig { /* ... */ }

pub struct RegionFlatTable<K: Hash + Eq + 'static, V: 'static> {
    // One private RegionVec<Group<K,V>>; no other owner fields.
}

pub struct FlatIter<'r, K: 'static, V: 'static> {
    // Private borrowed traversal state only; no owned payload or allocation.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatError {
    InvalidMaxGroups { max_groups: usize },
    UnsupportedEntryLayout,
    CapacityLimit { requested: usize, max: usize },
    Region(RegionError),
}

pub struct FlatInsertError<K, V> {
    pub value: (K, V),
    pub error: FlatError,
}
```

`FlatConfig.max_groups` must be a positive power of two no greater than
HARD_MAX_FLAT_GROUPS. The maximum accepted live entry count is
`14 * max_groups`. A checked constructor rejects invalid configuration before
checking entry layout, requested capacity or allocating. The complete actual
Group layout must be nonzero, valid under `Layout`, fit `isize` byte arithmetic,
and be no larger than HARD_MAX_FLAT_GROUP_BYTES. Layout validation precedes
creating any complete Group stack value or allocating its region buffer.
Representable unsupported layouts return UnsupportedEntryLayout. Rust may
reject an unrepresentable type/array layout at monomorphization; the runtime
does not promise to instantiate impossible Rust types to return an error.

Zero-sized K and/or V are supported. The cached hash and control/header data
make Group nonzero. Group alignment is at least 16; greater field alignment
is permitted when the complete layout is supported, subject to the existing
RegionConfig alignment limit at allocation. K/V need neither Copy, Clone,
Default nor Debug. No user constructor/default callback initializes empty slots.

RegionFlatTable is move-only, neither Clone nor Copy, invariant in its payload
types through RegionVec, and has Debug without requiring K/V:Debug. It exposes
only `region(&self) -> RegionId` outside the region methods below. No raw handle
constructor, field reinterpretation, serialization reconstruction or frozen
mutable alias is provided. All persistent table state is region resident.

FlatError implements Display and std::error::Error. Its Region variant exposes
the wrapped RegionError as its source. It has From<RegionError> and
From<AllocError>, with allocation errors represented through RegionError::Alloc.
FlatInsertError has Debug/Display/Error without requiring K/V:Debug and exposes
its FlatError as the error source. Existing public error enums are unchanged.

## Frozen public methods and lifetimes

Methods are inherent on Region<R> for every R:ChunkBacking. Every method below
has generic bounds K:Hash+Eq+'static and V:'static; repeated declarations are
elided only for readability. Actual definitions use the explicit returned
reference lifetimes shown here. The owner borrow does not constrain `'r`.

```rust
flat_new<K, V>(&mut self, config: FlatConfig)
    -> Result<RegionFlatTable<K,V>, FlatError>;
flat_with_capacity<K, V>(&mut self, entries: usize, config: FlatConfig)
    -> Result<RegionFlatTable<K,V>, FlatError>;

flat_len(&self, owner: &RegionFlatTable<K,V>)
    -> Result<usize, HandleError>;
flat_is_empty(&self, owner: &RegionFlatTable<K,V>)
    -> Result<bool, HandleError>;
flat_capacity(&self, owner: &RegionFlatTable<K,V>)
    -> Result<usize, HandleError>;
flat_group_count(&self, owner: &RegionFlatTable<K,V>)
    -> Result<usize, HandleError>;

flat_get<'r>(&'r self, owner: &RegionFlatTable<K,V>, key: &K)
    -> Result<Option<&'r V>, HandleError>;
flat_get_mut<'r>(&'r mut self, owner: &RegionFlatTable<K,V>, key: &K)
    -> Result<Option<&'r mut V>, HandleError>;
flat_get_disjoint_mut<'r>(
    &'r mut self, owner: &RegionFlatTable<K,V>, keys: [&K; 2],
) -> Result<[Option<&'r mut V>; 2], HandleError>;

flat_iter<'r>(&'r self, owner: &RegionFlatTable<K,V>)
    -> Result<FlatIter<'r,K,V>, HandleError>;
flat_control_group<'r>(
    &'r self, owner: &RegionFlatTable<K,V>, group: usize,
) -> Result<Option<&'r [u8; 16]>, HandleError>;

flat_insert(&mut self, owner: &mut RegionFlatTable<K,V>, key: K, value: V)
    -> Result<Option<(K,V)>, FlatInsertError<K,V>>;
flat_remove(&mut self, owner: &RegionFlatTable<K,V>, key: &K)
    -> Result<Option<(K,V)>, HandleError>;
flat_reserve(&mut self, owner: &mut RegionFlatTable<K,V>, additional: usize)
    -> Result<(), FlatError>;
flat_clear(&mut self, owner: &RegionFlatTable<K,V>)
    -> Result<(), HandleError>;
flat_release(&mut self, owner: RegionFlatTable<K,V>)
    -> Result<(), OwnerError<RegionFlatTable<K,V>>>;
```

An empty table contains one initialized group; group zero is necessary for its
resident header. `flat_new(config)` is `flat_with_capacity(0, config)` and has
logical capacity 14, not zero bytes. Capacity is the actual live-entry threshold
`14 * initialized_groups`, not the number of physically available entry lanes.
The constructor chooses the smallest supported power-of-two group count with
enough live capacity. Requested capacity is an initial reservation, not a
permanent table limit. `flat_group_count` returns the initialized group count.
`flat_reserve(additional)` reserves for resident len + additional, with checked
addition. Exceeding the configured live limit returns CapacityLimit, recording
the requested total entry count and the configured maximum. Addition overflow
returns Region(Alloc(SizeOverflow)). Shape validation still precedes capacity
rejection in constructors. Region allocation failures retain their usual error.

All checked operations validate the owner, including reserve zero, empty/missing
lookup, out-of-range control inspection, iteration and clear. Invalid owners
fail before Hash/Eq callbacks. Valid out-of-range control inspection is Ok(None).
The control view is inspection-only and borrows Region; callers cannot mutate
controls or hold a view across mutation, growth, reset or rewind.

Insertion of a new key returns None. Replacement installs the incoming K and V,
keeps length unchanged, and returns the actual previous `(K,V)` without dropping
either object. Replacement does not request growth even at the live threshold
or under a watermark. Any returned insertion error preserves all existing
entries/owner identity and returns the incoming tuple unchanged. Resource and
arithmetic refusal happens before entry/control mutation; failed reserve keeps
rows, owner and RegionStats unchanged as guaranteed by RegionVec.

Remove returns the actual owned key/value pair, reducing length once. A miss
changes nothing. Shared/mutable lookups expose values only; stored keys are
immutable. Disjoint lookup preserves input order and resolves both keys before
producing mutable references. If both resolve to the same occupied slot, it
returns HandleError::Aliased without mutation. Missing keys yield None; two
equal missing keys yield `[None,None]`. Aliasing is decided by occupied slot
identity, not pointer addresses or query-object identity. This matters for ZST
values. Returned mutable values are obtained from safe slice/field splits.

FlatIter implements Iterator<Item=(&'r K,&'r V)>, ExactSizeIterator and
std::iter::FusedIterator. It visits each occupied entry once in ascending
group/lane order and borrows Region, not the owner. No stable insertion order or
iteration order across rehash is promised. Constructing or consuming it does
not hash, compare keys, move payloads or allocate. size_hint/len reflect remaining
entries. FlatIter need not implement Clone, DoubleEndedIterator or Debug.

Release delegates to the ordinary affine RegionVec release and drops remaining
entries. Validation failure returns the original owner through OwnerError.owner.
The table does not add a source-language linear obligation.

## Storage and scalar probing

The new source module declares `#![forbid(unsafe_code)]`. It uses existing
checked RegionVec operations, standard Option ownership, and safe slice splits.
It adds no sibling-private region/allocator hook and no separate host payload
buffer, index map, scratch Vec, HashMap or per-entry allocation.

The representation is one private `#[repr(C, align(16))] Group<K,V>` with fields
in this order: `control: [u8;16]`, `entries: [Option<Entry<K,V>>;16]`, then a
private header containing `len:usize` and `max_groups:usize`. Entry owns cached
`hash:u64`, K and V. Header order is len then max_groups. Only group zero's
header carries live values; other group headers are zero. Rust's internal
Option/entry layout is not a wire or cross-toolchain ABI. Both control groups
and the encompassing groups begin at addresses aligned to at least 16 bytes;
consecutive control addresses differ by the actual complete Group stride.

RegionVec's initialized element count is the logical group count, always a
positive power of two no greater than max_groups. Its reserved group capacity
equals that count. Construction reserves the exact group count. Growth uses
vec_reserve_exact and initializes every newly reserved group before returning;
hidden amortized overcapacity is forbidden. The ordinary RegionVec bookkeeping
may use host metadata as before; actual controls and inline entry objects reside
in region chunks. A caller's String/Vec/Box payload may already own indirect
heap allocations; arbitrary Rust payloads are not transitively region-resident.

Stable states satisfy: an occupied control has one Some entry; EMPTY/DELETED
have None; resident length equals the number of occupied controls. No tombstone
counter or outer duplicate length is necessary. The maximum load is 14 live
entries per allocated group in aggregate, not a per-group restriction.

Hash each queried/incoming key with a fresh `std::collections::hash_map::DefaultHasher::new()`
and its ordinary Hash implementation. Cache the complete finish() value. The
h2 tag is the upper seven hash bits, `(hash >> 57) as u8`; starting group is the
low hash bits masked by group_count-1. Probe consecutive groups with wraparound.
Within each group, check all occupied matching h2 candidates, then full cached
hash, then Eq. Only after that group's candidates are checked may an EMPTY
control terminate an unsuccessful search. Remember an earlier DELETED vacancy
for insertion without skipping a possible later equal key. At most group_count
groups and 16 lanes per group are examined by one search. A group may contain
more than 14 entries if global load allows it.

Removal marks DELETED, not EMPTY, so surviving probe chains remain searchable.
Insertion may reuse a remembered DELETED slot. Every algorithm must also work
when accumulated tombstones leave no EMPTY byte: bounded full-table traversal
returns a miss or a reusable vacancy, rather than looping indefinitely. The
global live threshold guarantees a vacancy for a permitted new insertion.
Reserve need not compact tombstones when existing capacity suffices.

K must obey the ordinary stable Eq/Hash contract: equal keys hash equally, Eq is
an equivalence relation, and equality/hash behavior is not changed while stored,
including by interior mutation or external state. Violating this is a table
logic error; it may spoil lookup/replacement semantics but cannot justify memory
unsafety, unchecked indexing, unbounded probing or aliasing mutable references.
DefaultHasher output is neither persisted nor advertised as stable across Rust
versions/processes. No randomized collision security guarantee is made.

## Growth, ownership and callback boundary

Insertion validates and hashes/searches first. User Hash/Eq callbacks run only
before that operation's allocation, relocation or entry/control mutation. A
replacement takes the previous entry and installs the new one without running
Drop. A new insertion performs all fallible reserve work before placing the
incoming entry. Lookup/removal likewise finish Hash/Eq before any mutation.
Region backing allocation and explicit destruction are separate callbacks;
they retain the ordinary backing/Drop contracts, not an arbitrary callback
exclusion claim. Hash/Eq may be expensive or panic, so there is no user-code CPU
time bound and no strong rollback promise for arbitrary Rust panics.

Growing to the new power-of-two count follows this exact ownership plan:

1. Validate live owner, checked requested count/bytes/configured group cap, and
   reserve the complete exact RegionVec capacity. Every returned resource error
   occurs here, before any table entry or control changes.
2. Initialize additional empty groups within that pre-reserved capacity, using
   only private code and None slots. These pushes cannot request allocation or
   invoke user code. The live owner remains checked; impossible internal errors
   must not be exposed as a resource Err after successful capacity mutation.
3. Borrow the complete initialized group slice once. Retain length/configuration
   in header zero. Mark controls EMPTY while retaining the owned entries as
   unplaced Some values. No external view, callback or fallible work occurs in
   this transient representation.
4. Take an unplaced entry into one pending stack variable. Use its cached hash
   to find a vacancy under the new group mask. An EMPTY destination may hold an
   unplaced entry: swap that entry into the pending variable, install the prior
   pending entry and publish its h2. An EMPTY destination containing None ends
   this chain. Occupied controls represent placed entries and are skipped.
5. Each placement consumes one unplaced entry. At most the original live count
   placements are needed across all chains, each with a bounded probe. Do not
   rehash or compare keys, clone/drop payloads, allocate a payload temporary
   collection, or return a fallible error after the first ownership move.
6. Restore the stable control/Option invariant with the original resident
   length. Subsequent new-key placement consumes one vacancy and increments
   resident length once.

Group arrays, Option moves/swaps and cached hash arithmetic invoke no user
Hash/Eq/Default/Drop callback during rehash. All region operations that can
allocate precede rehash. Safe RegionVec relocation transfers owned Group bytes
and finalizer metadata using the existing allocator implementation.

Clear visits occupied entries and takes each Option before destruction. It
marks that slot DELETED and decrements resident length before dropping the
owned Entry. If K/V Drop panics, remaining entries still satisfy the stable
invariant and their probe chains remain intact. Only after successful complete
clear are all controls reset to EMPTY. Capacity and allocation identity remain.
At most ordinary Rust panic/drop behavior is promised: a second panic while
unwinding can abort. Region release/rewind/reset/drop retain existing finalizer
guards; no entry is duplicated, dropped twice, or silently leaked by table code.

## Watermarks and reclamation

The underlying affine RegionVec's existing semantics are authoritative:

* Mutations, replacements, removals and new entries within capacity reserved
  before a watermark survive scratch rewind. This is storage lifetime, not a
  snapshot rollback transaction.
* A premark table cannot grow while the watermark is live. An insertion needing
  growth returns ProtectedByWatermark with the original incoming tuple and all
  entries/stats unchanged. Replacement and insertion within capacity remain
  valid. Reserve zero still validates the handle.
* A table allocated after a covering watermark becomes stale on rewind,
  including after relocation. Reset/release invalidate it; region/slot/serial
  validation prevents revival through reused slots or bytes.
* Dropping or forgetting the move-only owner does not release region memory or
  discard entries. Region rewind/reset/drop still finalizes each remaining
  affine entry exactly once. Explicit remove/replacement transfers owned pairs
  to the caller; table finalization does not drop those transferred objects.
* Safe views/iteration borrow Region, preventing mutation, growth, rewind/reset
  or region destruction while used. Keys are never exposed mutably.

There is no new freeze/clone/drain API, raw index handle, hasher/model branding,
Borrow<Q> heterogeneous lookup, retain callback, source linearity promise or
concurrent mutation facility in this first profile.

## Required actual evidence and acceptance

Source 221 implements all frozen methods and local core cases: configuration,
minimum/rounded capacity, complete-layout preflight, insertion/replacement,
get/get_mut/remove, reserve/grow, initialized groups, exact live thresholds,
bounded wraparound/tombstone search, both disjoint projections, iterator and
control bounds, original input tuple on returned failure. Include real static
function-pointer variance witnesses and compile-fail examples for owner clone,
nonstatic payload, mutable key exposure through the API's immutable iterator,
and views preventing mutation/rewind. Positive controls must establish the
compile-fail example's intended coercion rather than relying on a vacuous
static bound or missing import.

Model 222 independently compares actual operations with a scalar ownership
model, including affine non-Copy/non-Default keys and values, replacement of
equal but observably distinct keys, full hash collisions, same h2/different full
hash, start-group wraparound, tombstone saturation/reuse and repeated growth.
Counters/panic injection verify no Hash/Eq callbacks during reserve/rehash and
lookup Hash/Eq panics before mutation. Test returned/transferred/dropped objects,
two-key alias/miss combinations, iterator completeness and mutable value oracle.

Lifecycle/layout 223 uses counting/failing backing and finite RegionConfig to
check failed allocation/growth stats and tuple preservation, configured hard
limits/layout rejection before allocation, premark within-capacity changes,
protected growth, postmark relocated-table invalidation, cross-region/stale
checks on no-op/missing/bounds APIs, slot/serial reuse, forgotten owners, release
failure owner recovery, reset and exactly-once affine drops/backing deallocation.
Catch a single injected destructor panic during clear and verify the surviving
table remains searchable and safely reusable. Physical tests inspect actual
public control addresses, exact 16-byte control length/alignment, consecutive
group stride and tags using varied native K/V layouts; they do not claim a wire
ABI or pointer inequality for ZST values. The executable example demonstrates
real insertion/replacement/removal, independent model comparison and disjoint
value mutation using the actual public API.

Every lane runs meaningful owned tests, appropriate existing allocator tests,
strict package all-target Clippy, formatting and diff checks in its private
cache. Coordinator runs combined actual tests/docs/example and independent
Miri where available, recording executed evidence and limits honestly. No
ignored tests, weakened caps, fabricated source stubs or hosted/platform claims
stand in for actual execution. Runtime/test work starts after coordinator publication of this frozen
contract and remains inside the exclusive paths above.

This establishes scalar 16-lane control-group shape, bounded runtime probing,
actual inline region storage and ordinary Rust affine ownership. It does not
establish Isa[Baseline128]/SIMD instruction counts, language model brands,
Vesper source FlatTable lowering, Lean refinement, source linear exhaustion,
zero transitive heap allocation, whole-process RSS bounds, adversarial callback
termination, throughput, or performance superiority.
