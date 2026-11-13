# Region-resident affine slot arena (§15.5)

Claims220(parent),224(source),225(model) and226(lifecycle) were publicly created
before writers. Base main9f0e2f5. Root checked actual open-PR files and claims for
allocator overlap. Parallel flat-table221–223 and reserved tiled206/215, external
Claude121–136, root CLI/core/Cargo, build/workflow and other claimed paths remain
separate. Publish amendments before dependent source changes. Root has reviewed and frozen this contract:
source work is authorized after its publication to the integration branch.

|Claim|Branch|Exclusive writes|
|---|---|---|
|220|collections41/integration|published contract, region.rs/lib.rs registration and actual reexports, integration/review report|
|224|collections41/arena-source|new `crates/vesper-alloc/src/region/arena.rs`, new `crates/vesper-alloc/tests/arena_core.rs`, source report|
|225|collections41/arena-model|new `crates/vesper-alloc/tests/arena_model.rs`, model report|
|226|collections41/arena-lifecycle|new `crates/vesper-alloc/tests/arena_region.rs`, new `crates/vesper-alloc/examples/arena_graph.rs`, new `crates/vesper-alloc/ARENA.md`, acceptance report|

Reports are `docs/rfcs/arena41-{source,model,acceptance}-report.md`; root owns its
integration report. Separate worktrees/private caches, one Cargo job, debug0,
incremental0. Root alone registers the module and reexports actual implemented types after
importing a coherent source commit; independent tests wait for those exports. No Vec/slab/tiled/allocator metadata,
dependency/Cargo, compiler, CLI, BUILD/workflow/wrapper, or unrelated test edits.
Claims stay reserved through reviewed merge. No source builds until publication.

## Frozen public types

```rust
pub const HARD_MAX_ARENA_ENTRY_BYTES: usize = 64 * 1024;
pub struct RegionSlotArena<T: 'static> { /* one private RegionVec<Entry<T>> */ }
pub struct ArenaId(/* private u64 */);
pub struct ArenaKey<T: 'static> {
    // Private ArenaId, u32 slot, u64 generation and invariant PhantomData.
}
pub enum ArenaError {
    Region(RegionError),
    UnsupportedEntryLayout,
    ExhaustedArenaIds,
    ExhaustedGenerations,
    ForeignArena { expected: ArenaId, found: ArenaId },
    StaleKey,
    AliasedKey,
}
pub struct ArenaInsertError<T> {
    pub value: T,
    pub error: ArenaError,
}
```

ArenaId is Copy/Clone/Debug/Eq/PartialEq/Ord/PartialOrd/Hash and has inspection
`bits(self) -> u64`; its integer constructor remains private. ArenaKey manually
implements Copy/Clone/Eq/PartialEq/Hash/Debug without imposing any T trait bound
beyond 'static. An invariant `PhantomData<fn(T)->T>` prevents field-type
coercion; keys contain no T or pointer into allocation memory. Diagnostic-only
inspectors are `arena(self) -> ArenaId`, `slot(self) -> u32`, and
`generation(self) -> u64`. Keys/IDs have no public constructor, Default, wire/
serde parser or conversion from diagnostics. Copying a key creates a checked
weak reference, not an additional value owner.

RegionSlotArena is move-only, not Clone/Copy, invariant through its private
RegionVec, and has Debug without T:Debug plus inspection-only
`region(&self) -> RegionId`. It retains no separate ID/count/free-list metadata
outside region storage. ArenaError implements Debug/Clone/Copy/Eq/PartialEq,
Display and Error with the wrapped RegionError as its source. ArenaInsertError
implements Debug/Display/Error without T:Debug and preserves the original
input allocation/value. Existing error enums are not extended. Allocation/
capacity arithmetic failures use ArenaError::Region(RegionError::Alloc(...)).

T may be affine Rust data: non-Copy, non-Clone, non-Default and Drop-bearing
values are supported. 'static permits owned values and static references but
excludes borrowed values that could expire before region finalization. No
linear constructor or mode-polymorphic policy is added.

## Frozen Region methods

Inherent methods are available on `Region<B>` for every `B: ChunkBacking`.
Repeated generic declarations below elide only `T:'static`.

```rust
arena_new<T>(&mut self) -> Result<RegionSlotArena<T>, ArenaError>;
arena_with_capacity<T>(&mut self, items: usize)
    -> Result<RegionSlotArena<T>, ArenaError>;
arena_id(&self, owner: &RegionSlotArena<T>) -> Result<ArenaId, ArenaError>;
arena_len(&self, owner: &RegionSlotArena<T>) -> Result<usize, ArenaError>;
arena_is_empty(&self, owner: &RegionSlotArena<T>) -> Result<bool, ArenaError>;
arena_capacity(&self, owner: &RegionSlotArena<T>) -> Result<usize, ArenaError>;
arena_reserve(&mut self, owner: &mut RegionSlotArena<T>, additional: usize)
    -> Result<(), ArenaError>;
arena_insert(&mut self, owner: &mut RegionSlotArena<T>, value: T)
    -> Result<ArenaKey<T>, ArenaInsertError<T>>;
arena_get<'a>(&'a self, owner: &RegionSlotArena<T>, key: ArenaKey<T>)
    -> Result<&'a T, ArenaError>;
arena_get_mut<'a>(&'a mut self, owner: &RegionSlotArena<T>, key: ArenaKey<T>)
    -> Result<&'a mut T, ArenaError>;
arena_get_disjoint_mut<'a>(&'a mut self, owner: &RegionSlotArena<T>,
                         first: ArenaKey<T>, second: ArenaKey<T>)
    -> Result<(&'a mut T, &'a mut T), ArenaError>;
arena_remove(&mut self, owner: &mut RegionSlotArena<T>, key: ArenaKey<T>)
    -> Result<T, ArenaError>;
arena_release(&mut self, owner: RegionSlotArena<T>)
    -> Result<(), OwnerError<RegionSlotArena<T>>>;
```

Inspection and all no-op/bounds paths validate the checked RegionVec owner
first, including reserve0. Then keyed access checks exact arena ID, checked
slot-to-vector index conversion/addition, vector bounds and occupied generation.
Foreign keys return ForeignArena, absent/vacant/out-of-range/generation-mismatched
keys return StaleKey. Never turn invalid access into Ok(None) or successful
no-op. A key removed once cannot remove or read a subsequently inserted value.
Opaque keys make malformed indices unavailable in safe client code; private
source tests may exercise checked index boundaries without a public forge API.

Pair access validates BOTH keys fully before alias classification. Two valid
keys for the same index return AliasedKey; invalid keys retain their ordinary
validation error. For distinct indices obtain one checked mutable entry slice,
split it safely with split_at_mut, and return values in requested argument
order, including reversed indices. Returned references borrow Region itself;
they cannot coexist with subsequent mutation, insertion, reserve, rewind/reset
or drop. No raw pointers, retained independently authorized references or new
unsafe blocks are required. The module has `#![forbid(unsafe_code)]`.

Insert failure returns the original T unchanged and preserves all live values,
keys, free-list/count/generation state and owner. Reserve/ordinary allocation
failure preserves logical arena state and RegionStats under existing RegionVec
failure guarantees. A failed consuming release returns the original owner via
OwnerError with existing RegionError; release has no key-validation policy.
Remove moves ownership to the caller without invoking T::drop. Subsequent
drop of that returned T is the caller's operation.

## Private region-resident representation

One private RegionVec<Entry<T>> has a permanently initialized Header at index0.
The other initialized elements are either Vacant{next_free} or
Occupied{generation,value:T}. Header stores ArenaId, live count, free-list head
and next generation. Optional small private Header/slot helper types are owned
entirely by arena.rs; no sibling-private allocator hook is required.

Slot0 in a key addresses entry1, never the header. At most u32::MAX payload
slots are addressable; requested slots, additional-live count, header+1,
slot-index+1 and complete byte size are checked before allocation/indexing.
Capacity is usable payload-slot capacity, excluding the header and limited to
u32::MAX even if ordinary RegionVec amortized allocation over-reserves. Live
count and initialized slot count cannot exceed that logical limit. No allocated
payload lives in a fallback host Vec/Box. Existing Region metadata may retain
its existing bounded host bookkeeping.

Constructor validates the actual complete Entry<T> enum Layout and size<=64KiB
before constructing even its Header variant, since every variant shares the
complete Rust enum stack layout. Checked header+capacity/byte/isize bounds are
preflighted too. Unsupported representable entry layouts return
UnsupportedEntryLayout; Rust may reject unrepresentable types/layouts during
monomorphization instead. There is no new arbitrary64-byte alignment restriction
or ZST ban: actual Layout and RegionConfig::max_align govern alignment, and ZST
values remain logical occupied values with ordinary Drop behavior. An empty
arena reserves one complete header entry even when payload capacity is zero.
The owner is private, so an unsupported entry type cannot later reach insert
with a valid owner. The64KiB policy bounds individual entry values, not compiler
temporary copies or entire process stack/RSS.

Remove links its now-vacant slot at the free-list head and decreases live count
using safe mem::replace/moves, with no user callback between state updates.
Insert preferentially reuses that head, otherwise appends one initialized entry.
It computes a generation prospectively, completes fallible growth first, then
commits state without a user callback. No vacant tail is popped/truncated and no
slot/generation is reset when the arena becomes empty.

Reserve's `additional` means additional successful insertion capacity: existing
vacancies count toward it. Derive vacancies as initialized_payload_slots-live,
request only remaining new tail capacity, and retain all current occupancy.
Delegate growth/fallback to existing RegionVec. Reserve promises memory only;
it consumes/reserves no generations and may succeed despite generation
exhaustion. No no-op may bypass owner validation.

## Identity and generation exhaustion

A checked process-global AtomicU64 mints independent never-reused ArenaIds.
Counters start at1. Pure checked successor helpers refuse current==u64::MAX,
leave it exhausted and never issue MAX itself or wrap through zero. Atomic
identity allocation uses checked fetch_update/compare-exchange, not wrapping
fetch_add. Pure layout/capacity validation happens before minting an identity.
An allocation failure after ID mint may consume that ID permanently; identities
are never rolled back/reused, and gaps have no trust meaning.

Each arena's next generation is stored in its Header and follows the same
checked-successor rule. Every successful insertion, whether new or reused slot,
gets a new arena-local generation. Compute issued/successor before moving the
value/allocating, but commit the counter only after successful insertion. At
exhaustion insertion fails with original T and unchanged arena. Existing get,
remove, pair access, inspection and release remain allowed; reserve retains its
memory-only meaning. No failed operation silently repairs exhaustion or invalid
source keys. Tests use local private counter/header helper state to exercise
MAX-1/MAX without corrupting process-global counters or weakening production
visibility.

## Lifecycle, Drop and scope

RegionVec owns the initialized Entry<T> range and its existing Rust drop glue.
Header/vacant entries own no T. Release, covering rewind, reset and region drop
finalize each still-occupied T exactly once on ordinary nonpanicking paths,
even when the arena owner was dropped/forgotten. Relocation transfers entry
ownership using existing RegionVec machinery, without cloning/finalizing T.
Removed values are finalized only by their callers. Finalization order follows
existing reverse initialized vector entry order, not an insertion-order claim.
T-owned transitive heap allocations are not included in the region byte cap;
this cap bounds resident entry storage. Existing panic/user-callback/physical
OOM/process-abort boundaries are unchanged; no stronger transactional drop,
panic recovery, linear-consumption or destructor-time guarantee is asserted.

A premark arena survives scratch rewind with its existing-storage mutations:
removal, free-slot reuse and appends within preallocated capacity survive too.
A new key minted by such a surviving mutation remains valid after that rewind.
Growing the protected premark buffer is refused unchanged by RegionVec. An
arena allocated after a covering mark becomes stale after rewind even if it
relocated; reset/release likewise invalidate it. Reused allocation bytes/slot
indices or a new arena in the same Region cannot revive old keys/owners.

This implements a real runtime-checked affine arena and cyclic graph handle
foundation. It does not establish source-language region branding, compile-time
cross-arena prevention, Vesper linear Mode policies, RegionRef compiler lowering,
garbage collection, permanent raw references, parallel mutation, Pin guarantees,
VirtualArena/mmap provisioning, Lean verification or performance claims.

## Required actual evidence and independent lanes

Source224: ordinary non-Copy insertion/get/mutation/remove/reuse, value/owner
preservation on failure, pure entry-layout/slot/byte refusal before construction/
allocation, no-op/bounds validation, pair order/alias/stale handling, local pure
ID/generation exhaustion tests including unaffected existing live values. Keys
must be Copy/Hash/Eq without payload trait bounds. Public doctests include
nonstatic payload rejection, nonclonable owner, projection borrowing and true
invariant-wrapper rejection between valid 'static function-pointer field types
with a positive ordinary function-pointer coercion control.

Model225: independent deterministic scalar model of insert/remove/reuse across
many mixed operations, duplicate/foreign/stale keys and arenas within one and
different Regions, generation changes, pair requested order, same-key alias,
invalid-pair priority, capacity/reserve vacancy accounting. Keep production key
constructors private. Use non-Copy payloads and hash keys whose payload type has
no Hash/Eq/Clone implementation. Tests compare actual live identities/values;
they do not clone implementation free-list internals as the oracle.

Lifecycle226: counting/failing backing and Drop counters; finite bounds,
protected growth, both premark reuse/append cases, postmark allocation and
relocation invalidation, serial/arena reuse, release/reset/region-drop/forgotten
owner and exact ordinary finalization. Removed live values must remain caller
owned through subsequent arena cleanup. Include ZST Drop where meaningful,
failure ownership, and actual bounded cyclic graph example: traversal/update
through keys, safe disjoint mutation, then removal producing checked stale
edges. ARENA.md states truthful host-runtime and transitive heap boundaries.

Each lane uses actual public APIs after coordinator reexports, meaningful owned
tests, existing allocator tests where appropriate, Rustdocs, strict package
all-target Clippy, workspace fmt/diff checks and its own cache. Root combines
independent suites and may run Miri separately. Do not ignore tests, relax limits
or claim hosted/platform/Miri success without execution.
