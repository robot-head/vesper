# Strata collections (design §15.3 and §15.5)

## Wave eight lane 7 (claim #456)

Branch `claude/project-thread-gg7d7m` (the suggested `wave8/strata-collections` was not used; the session is bound to this branch). Base: `origin/main` at `1094e649c95e4d81eef4e705d13882ab1339542d`, a descendant of the required `ea1ed29`. Owned paths: `crates/vesper-alloc/**`, `lean/StrataCollections.lean` and this file. The only root change is `Cargo.lock` (vesper-alloc now lists `libc`, which is already a workspace dependency and already in the lockfile); it is in its own commit.

Much of the assignment already existed on `main` from earlier lanes: `RegionVec`, `RegionString`, `RegionSlice`/`RegionStr` (this file, below), `RegionSlotArena` with generational `ArenaKey`s (`ARENA.md`), the local `ShardSlab` ([strata-slab.md](strata-slab.md)) and two-column `RegionTiled2` tiles (`TILED.md`). This lane adds what was missing and keeps every existing API unchanged; wave-seven lane 29's source regions (`src/regions/**`) still build and pass against it.

### What was built

| Item | Implementation |
| --- | --- |
| §15.3 per-thread shards with remote-free queues | `ShardedSlab` (shareable, `Clone + Send + Sync`) with 1 to 256 shards. A thread claims one shard as a `Shard` (`Send`, not `Sync`, one claimant at a time; dropping it parks the bookkeeping for the next claimant). Each shard owns its 64 KiB slabs and per-class free stacks over the existing 48 size classes. Same-shard `allocate`/`free` pop or push a free stack whose capacity was reserved when the slab was provisioned: no lock, atomic or global-allocator call on that path once the class has a free slot. A `ShardBlock` (`Send + Sync`, unique, zeroed on allocation) freed by another shard joins a per-target outgoing batch owned by the freeing shard; a full batch (`remote_batch`, default 64) is appended to the origin's `Mutex<Vec>` remote queue under one lock. The origin drains its queue on its allocation slow path (before provisioning a new slab) or with `drain_remote`; `free_unbound` serves threads without a shard; `close` drains every queue and refuses while blocks are live. |
| Linear `RawBlock` owners | `ShardBlock` is `#[must_use]`, not `Clone`/`Copy`; its only exits are `Shard::free`, `ShardedSlab::free_unbound` and `Alloc::dealloc_raw`. A dropped block is detected, never silently released: the slot stays allocated, `dropped_shard_blocks()` counts it, and `close` keeps reporting it as `LiveBlocks`. Freeing into the wrong heap returns the unchanged owner (`SlabError::ForeignHeap`). If the heap is dropped while blocks are live, its slabs are deliberately leaked so the owners stay valid. |
| `Alloc` capability API | `trait Alloc { type Block; fn alloc_raw(&mut self, size, align); fn dealloc_raw(&mut self, block) }`, implemented by `Shard` (`RawBlock = ShardBlock`) and the existing local `ShardSlab`. This is a host API convention; Rust always has ambient `GlobalAlloc`, so it is not evidence for the source rule that a row without `Alloc` allocates nothing. |
| §15.3 VirtualArena reservation path | `VirtualArena`: one contiguous range reserved with anonymous private `mmap` (`MAP_NORESERVE` on Linux) on Unix outside Miri, global-allocator `alloc_zeroed` elsewhere (`backing_kind()` reports which). Bump allocation with checked arithmetic and page-bounded alignment, zeroing on hand-out, checked copyable `VirtualBytes` handles (arena id + never-reused serial), linear `VirtualMark` checkpoints with rewind/commit, and `reset`, which returns touched pages with `madvise(MADV_FREE)` (falling back to `MADV_DONTNEED`). The tip allocation grows in place (`extend`), and `VirtualVec<T: Copy>` doubles its capacity in place, so growth never copies; a non-tip vector fails with `NotTip` instead of relocating, and growth of an allocation older than the newest checkpoint fails with `ProtectedByWatermark`, as in `Region`. |
| §15.5 `tiled(N)` AoSoA storage | `Tiled<T, N>` with `N` in {8, 16, 32, 64} (other widths are a compile-time error). A record implements `TiledRecord`, splitting into a hot tuple of 1 to 6 `Copy + Default` columns and a cold remainder. Each tile is a tuple of `Col<A, N>` arrays, each `#[repr(align(64))]`, so every column of every tile starts on a cache line and a column scan reads only that column; cold values live in a separate side array. API: `push` (returns the record on failure), `pop`, `get`, `set`, `hot`, `cold`/`cold_mut`/`cold_slice`, `truncate`, `clear`, `reserve`/`with_capacity` (fallible), `tiles`/`tiles_mut`/`tile`/`tile_mut` exposing only initialized lanes, `TileRef::col`, `TileMut::col_mut` and `cols2_mut` (two disjoint mutable columns; naming one column twice is a borrow error), `column` (one slice per tile) and `iter`. Storage is host-heap `Vec` bounded by `TiledConfig::max_len`; the region-backed variant remains `RegionTiled2`. |
| `RegionRef[r, T]` | `RegionRef<T>`: a `Copy`, invariant, region-branded weak reference minted from a live `RegionBox<T>` (`Region::box_ref`). Reads go through `&Region` (`ref_get`), writes through `&mut Region` (`ref_get_mut`, `ref_get_disjoint_mut`, which rejects two references to one payload with `RefPairError::Aliased`). References can be stored inside region payloads to build shared and cyclic graphs. Taking the box makes every copy `Moved`; rewind, reset and release make them `Stale`, also after slot and address reuse; other regions report `CrossRegion`. |
| Lean model | `lean/StrataCollections.lean` (below). |

### Lean model

`lean/StrataCollections.lean` models a slot arena as a total map from slots to a generation and an optional value; keys carry a slot and the generation they were issued at; lookup requires equal generations; removal clears the slot and bumps its generation. It proves, for this model only: generations never decrease under any operation sequence (`step_gen_mono`, `run_gen_mono`); a key whose value was removed reads nothing under every later sequence of insertions and removals, even when its slot is reused (`stale_key_never_reads`, via `remove_makes_stale` and `get_stale`); a key issued later differs from every stale key (`fresh_key_differs_from_stale`); two keys that both read a value from one slot are equal (`live_keys_unique`). A `#guard` checks a concrete insert/remove/reuse trace. It uses core Lean 4.24.0 only; `#print axioms` reports at most `propext`, `Classical.choice` and `Quot.sound`, and the file has no `sorry`, `admit` or new axioms.

It is not a theorem about the Rust code. The Rust `RegionSlotArena` draws generations from an arena-wide counter rather than a per-slot one, which preserves the same stale-key property. `tests/slot_arena_model.rs` checks the file's hygiene, compiles it and audits the axioms when `VESPER_LEAN` is set, and runs a Rust transliteration of the model in lockstep with the real arena over 4,000 generated insert/remove steps, comparing every issued key (live and stale, including more than 1,000 stale keys whose slot had been reused) after each step.

### "0 Alloc" claims

`tests/zero_alloc.rs` installs a counting global allocator with per-thread counters and measures warmed-up steady states:

- 10,000 same-shard allocate/free pairs across two warmed size classes: 0 allocations.
- 18 rounds of 64 cross-thread frees (worker shard) and the origin's drain plus 64 reallocations: 0 allocations on either thread. (The queue is drained in place, so its capacity is reused; the outgoing batches are reserved at claim time.)
- 1,024 `Tiled` pushes within reserved capacity plus a full column scan: 0 allocations.
- 10 VirtualArena cycles (4,096 in-place `VirtualVec` pushes, checkpoint, allocate, rewind, reset): 0 allocations.
- 1,000 `RegionRef` reads and writes: 0 allocations.

Cold paths (first claim, slab provisioning, queue growth beyond its largest backlog, `Tiled` growth) do allocate. No performance or instruction-count claim is made, and no benchmarks were run. `virtual_arena::a_large_reservation_is_lazily_backed` (Linux, not Miri) measured on the cloud host that reserving 4 GiB raised RSS by less than 64 MiB, and that writing 8 MiB through in-place growth raised it by at least 4 MiB and less than 256 MiB.

### Unsafe code

New `unsafe` is confined to raw allocation and pointer access in two modules: `src/slab/sharded.rs` (slab allocation, slot pointers, block byte views, `Send`/`Sync` for block and bookkeeping owners) and `src/virtual_arena.rs` (`mmap`/`madvise`/`munmap`, byte and element views). The prompt allows unsafe only in the existing arena and slab modules; the sharded heap is a submodule of the slab module, while the virtual arena is a new module because `mmap` needs FFI. `RegionRef` adds three pointer dereferences in `src/region.rs` that follow the existing `RegionBox` accessors. `capability.rs` and `tiled.rs` are `#![forbid(unsafe_code)]`.

### Checks

Cloud host, Linux x86_64, rustc 1.97.0 stable, 1.100.0 for Clippy, Miri on nightly, Lean 4.24.0 from the GitHub release zip. Target dir inside the checkout, `CARGO_BUILD_JOBS=3`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`.

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo +1.100.0 clippy -p vesper-alloc --locked --all-targets -- -D warnings` | clean |
| `VESPER_LEAN=… cargo test -p vesper-alloc --locked` | all pass (new: 13 sharded, 9 virtual arena, 6 tiled, 3 RegionRef, 5 zero-alloc, 3 slot-arena model; 4 new `compile_fail` doctests) |
| `lean lean/StrataCollections.lean` (4.24.0) | compiles; axioms ⊆ {propext, Classical.choice, Quot.sound} |
| `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings` | WS_CLIPPY |
| `cargo test --workspace --locked` | WS_TEST |
| `cargo +nightly miri test -p vesper-alloc --tests` | MIRI |

### Decisions

- **Remote queues use a `Mutex<Vec>` per shard**, not a lock-free MPSC list. Batching (one lock per 64 remote frees) gives the amortization the design asks for without new unsafe intrusive lists; a lock-free queue can replace it behind the same API.
- **Shards are claimed explicitly** (`claim(index)`) rather than bound to OS threads or Pulse workers; the Pulse scheduler can claim one per worker. Moving a `Shard` between threads is allowed because it is `Send`; sharing it is not.
- **Dropped blocks are counted leaks**, not debug panics, so a panic unwinding through owners does not abort.
- **VirtualArena never relocates.** A non-tip vector that needs more room gets `NotTip`; callers reserve first or use `RegionVec`, which relocates. Elements are `Copy` so reset and rewind need no finalizers.
- **`Tiled` hot columns are `Copy + Default`**, which keeps the storage in safe Rust (unused lanes hold defaults and are never exposed). Records are split and joined by a user `TiledRecord` impl until the compiler derives it from `tiled(N) struct`.

### Remaining gaps

- Source-level `tiled(N) struct` declarations, hot/cold derivation, `TiledSeq` lowering, `load_tile_vec` and VVec SIMD loads are compiler work. `Tiled` is host-heap storage; a region-backed generic `Tiled` (beyond `RegionTiled2`'s two columns) and page-aligned cold side arrays are not built.
- `SlotArena[r, V]`/`RegionRef[r, T]` region branding is a runtime check here; the static brand by the capability name `r` is a checker obligation.
- MPK Domain-1 `DmaSlab`, 2 MiB hugepages, `IORING_REGISTER_BUFFERS`, NUMA placement and per-core (rather than per-claim) shards are not implemented. The remote queue is not lock-free.
- VirtualArena does not back `Region` chunks (Region still uses `ChunkBacking`), has no 2 MiB page hint, and on Windows commits its reservation eagerly through the global allocator (no `VirtualAlloc(MEM_RESERVE)`). The pinned-register evidence state and egglog bump coalescing in §15.3 are compiler work.
- `FlatTable` exists (`FLAT.md`); `CacheBTree` and `ChunkSeq` are not built.
- Rust ownership is not evidence that the Vesper source typing rules ([T-RegionAlloc], [T-RegionScope], linear exhaustion) are implemented.

## Earlier slice: bounded collections (next wave, lane 5)

Lane: Strata collections (next wave, lane 5). Branch `claude/project-thread-5ily4n` (the requested name `next/strata-collections` was not used; the session is bound to this branch). Base: `origin/main` at `c2ba0b1` (docs-only successor of the prompt commit `2a8e46e`, which contains the required `0e6caf9` baseline).
Owned paths: `crates/vesper-alloc/**` and this file. No root manifest, lockfile, CLI, CI, compiler, syntax, checker, backend or other lane's file is changed. The crate stays a registered root workspace member and still has no dependencies.

### What is implemented

| Requirement | Implementation |
| --- | --- |
| Contiguous vector | `RegionVec<T>`: construction (`vec_new`, `vec_with_capacity`, `vec_with_capacity_linear`), `len`/`capacity`/`is_empty`, shared and exclusive slice views, `get`/`get_mut`, `push`, `pop`, `extend_from_slice` (`T: Copy`), `reserve`/`reserve_exact`, `truncate`/`clear`, `drain` (move every element out), `release`, `freeze`. |
| Copyable read-only view | `vec_freeze` turns the unique vector into `RegionSlice<T>` (`Copy`), with `slice`, `slice_len`, `slice_release`. No API produces a mutable view from a slice handle. |
| UTF-8 string | `RegionString` wraps a private `RegionVec<u8>`: `string_new`/`with_capacity`/`from_str`/`from_utf8`, `len`/`capacity`, `as_str`, `as_mut_str`, `as_bytes` (shared only), `push`, `push_str`, `push_utf8` (validated), `pop`, `truncate` (char-boundary checked), `clear`, `reserve`, `release`, `freeze` to a `Copy` `RegionStr` (`str_as_str`, `str_len`, `str_release`). |
| Region backing only | Every element buffer is a tracked slot in region chunks obtained from the region's `ChunkBacking`, subject to `max_reserved_bytes`, `max_align` and `max_allocations`. There is no host `Vec`/`String` fallback for elements. Region metadata (slot table, chunk list, watermark stack) remains on the host heap, as before; this is not a zero-heap implementation. |
| Checked arithmetic | Element count × size, `len + additional`, amortized doubling and tip-end offsets are all checked; byte sizes above `isize::MAX` fail with `SizeOverflow`. Amortized growth that does not fit (overflow, reservation limit, backing refusal) retries with the exact requirement before failing. |
| Failure atomicity | A failed construction changes nothing. A failed `push` returns the value in `InsertError { value, error }` and leaves length, capacity, elements, handle and region statistics unchanged. Operations that consume a handle (`vec_release`, `vec_freeze`) return it in `OwnerError { owner, error }`. |
| Growth and metadata | Growth is in place when the buffer is the unprotected tip of the active chunk; otherwise it relocates: bitwise copy of the initialized prefix to a fresh reservation, then transfer of the element metadata (length, capacity, drop function, linear flag) to the new slot, then release of the old slot. No user code runs during the transfer, so a panic cannot observe a half-moved buffer. The handle is updated in place; the old slot's serial is never valid again. |
| Finalization | The region (not the handle) records the initialized length, so rewind, reset, release and region drop finalize exactly the live elements even when handles were dropped or forgotten. |
| Watermarks | A buffer allocated before the newest outstanding watermark cannot gain capacity inside the window (`ProtectedByWatermark`, value returned) because the new bytes would be reclaimed by the rewind while the vector survives. Pushes within existing capacity are allowed and survive the rewind, which makes the §15.5 pattern work when capacity is reserved before `checkpoint`. |

### Supported element types and handle kinds

- **Element types.** Any `T: 'static` (as for `RegionBox`): `Copy`, affine `Drop` types, zero-sized types (capacity `usize::MAX`, no element bytes, destructors still run once per element) and over-aligned types up to `RegionConfig::max_align` (larger alignments fail with `AlignmentTooLarge`). `'static` is required because the region may finalize elements after any borrow they hold would have ended. Linear elements use `vec_with_capacity_linear` (below).
- **`RegionVec<T>` and `RegionString` are unique** (neither `Clone` nor `Copy`). Mutation additionally requires `&mut Region`, so a mutable view excludes every other view of region memory. All handles are invariant in `T` (`PhantomData<fn(T) -> T>`).
- **`RegionSlice<T>` and `RegionStr` are copyable and read-only.** Copies alias one buffer; releasing any copy, or a covering rewind/reset, makes all copies stale. Freezing requires no live linear elements.

### Defined behavior

- **Finalizer order.** Within one buffer, elements drop highest index first (`truncate`, `clear`, `release`, rewind, reset, region drop). Buffers are finalized LIFO in slot order with other region payloads; a relocated buffer takes the position of its newest slot. If an element destructor panics, the remaining lower elements are still dropped while unwinding (a second panic aborts, as for `Vec`); the length is updated before any destructor runs, so nothing is dropped twice.
- **`pop`** moves the last element out (capacity kept). **`truncate(n)`** with `n >= len` is a no-op. **`drain`** moves elements out front to back to a sink and leaves the vector empty with its capacity; if the sink panics, elements not yet passed are dropped once.
- **Partial growth failure.** Growth reserves before writing; there is no partially initialized state. `extend_from_slice` is all-or-nothing.
- **Region destruction.** `close`/drop finalize all live elements and return every chunk. Nonempty linear vectors make `close`/`scoped` report `LinearObligationsLive` but are still finalized, matching `RegionBox` linear payloads.
- **Stale handles.** Rewind, reset and release invalidate every handle and slice copy into the affected buffers, also after the slot index and the bytes are reused by a newer allocation (serials are never reused). Foreign handles yield `CrossRegion`.
- **Linear elements.** A nonempty linear vector counts as one live linear obligation in `RegionStats::live_linear` (moved, not duplicated, on relocation). Rewind over it and reset fail without changes (the watermark is returned); `truncate`/`clear` that would discard elements, `release` and `freeze` fail and return the owner. Elements leave only through `pop` or `drain`.
- **UTF-8.** Strings accept only `&str`, `char`, or bytes validated by `std::str::from_utf8` (validating an appended suffix suffices because concatenating valid UTF-8 is valid). Truncation must land on a `char` boundary (`NotCharBoundary`). The mutable view is `&mut str`, whose safe API preserves UTF-8; there is no `&mut [u8]` view and the byte vector is private.

### Public API added

```rust
pub const REGION_ACCESS_CONTRACT_VERSION: u32 = 1;
pub struct RegionVec<T>;   pub struct RegionSlice<T>;   // + region() accessors
pub struct RegionString;   pub struct RegionStr;
pub struct InsertError<T> { pub value: T, pub error: RegionError }
pub struct OwnerError<H> { pub owner: H, pub error: RegionError }
pub enum StringError { Utf8(Utf8Error), NotCharBoundary { index: usize }, Region(RegionError) }

impl<B: ChunkBacking> Region<B> {
    fn vec_new<T: 'static>(&mut self) -> Result<RegionVec<T>, AllocError>;
    fn vec_with_capacity<T: 'static>(&mut self, cap: usize) -> Result<RegionVec<T>, AllocError>;
    fn vec_with_capacity_linear<T: 'static>(&mut self, cap: usize) -> Result<RegionVec<T>, AllocError>;
    fn vec_len / vec_is_empty / vec_capacity<T>(&self, &RegionVec<T>) -> Result<_, HandleError>;
    fn vec_as_slice<T>(&self, &RegionVec<T>) -> Result<&[T], HandleError>;
    fn vec_as_mut_slice<T>(&mut self, &RegionVec<T>) -> Result<&mut [T], HandleError>;
    fn vec_get<T>(&self, &RegionVec<T>, usize) -> Result<Option<&T>, HandleError>;
    fn vec_get_mut<T>(&mut self, &RegionVec<T>, usize) -> Result<Option<&mut T>, HandleError>;
    fn vec_reserve / vec_reserve_exact<T>(&mut self, &mut RegionVec<T>, usize) -> Result<(), RegionError>;
    fn vec_push<T>(&mut self, &mut RegionVec<T>, T) -> Result<(), InsertError<T>>;
    fn vec_extend_from_slice<T: Copy>(&mut self, &mut RegionVec<T>, &[T]) -> Result<(), RegionError>;
    fn vec_pop<T>(&mut self, &mut RegionVec<T>) -> Result<Option<T>, HandleError>;
    fn vec_truncate<T>(&mut self, &RegionVec<T>, usize) -> Result<(), RegionError>;
    fn vec_clear<T>(&mut self, &RegionVec<T>) -> Result<(), RegionError>;
    fn vec_drain<T>(&mut self, &mut RegionVec<T>, impl FnMut(T)) -> Result<(), HandleError>;
    fn vec_release<T>(&mut self, RegionVec<T>) -> Result<(), OwnerError<RegionVec<T>>>;
    fn vec_freeze<T>(&mut self, RegionVec<T>) -> Result<RegionSlice<T>, OwnerError<RegionVec<T>>>;
    fn slice_len<T>(&self, &RegionSlice<T>) -> Result<usize, HandleError>;
    fn slice<T>(&self, &RegionSlice<T>) -> Result<&[T], HandleError>;
    fn slice_release<T>(&mut self, RegionSlice<T>) -> Result<(), HandleError>;

    fn string_new / string_with_capacity(cap) / string_from_str(&str) -> Result<RegionString, AllocError>;
    fn string_from_utf8(&mut self, &[u8]) -> Result<RegionString, StringError>;
    fn string_len / string_is_empty / string_capacity(&self, &RegionString) -> Result<_, HandleError>;
    fn string_as_str(&self, &RegionString) -> Result<&str, HandleError>;
    fn string_as_mut_str(&mut self, &RegionString) -> Result<&mut str, HandleError>;
    fn string_as_bytes(&self, &RegionString) -> Result<&[u8], HandleError>;
    fn string_reserve / string_reserve_exact(&mut self, &mut RegionString, usize) -> Result<(), RegionError>;
    fn string_push(&mut self, &mut RegionString, char) -> Result<(), RegionError>;
    fn string_push_str(&mut self, &mut RegionString, &str) -> Result<(), RegionError>;
    fn string_push_utf8(&mut self, &mut RegionString, &[u8]) -> Result<(), StringError>;
    fn string_pop(&mut self, &mut RegionString) -> Result<Option<char>, HandleError>;
    fn string_truncate(&mut self, &RegionString, usize) -> Result<(), StringError>;
    fn string_clear(&mut self, &RegionString) -> Result<(), HandleError>;
    fn string_release(&mut self, RegionString) -> Result<(), HandleError>;
    fn string_freeze(&mut self, RegionString) -> Result<RegionStr, HandleError>;
    fn str_len(&self, &RegionStr) -> Result<usize, HandleError>;
    fn str_as_str(&self, &RegionStr) -> Result<&str, HandleError>;
    fn str_release(&mut self, RegionStr) -> Result<(), HandleError>;
}
```

Existing API and behavior are unchanged. Internally, handle resolution now treats a serial whose slot kind differs from the handle's kind as `Stale` (previously a `debug_assert`); this only arises for a frozen buffer and cannot be reached by existing handle kinds.

### Proposed `RegionAccess` contract (v1, for compiler-owner review)

`REGION_ACCESS_CONTRACT_VERSION = 1` (named `RegionAccessV1` in `docs/rfcs/parallel-contracts.md`). It extends the v0 lowering proposal in [strata-regions.md](strata-regions.md). No compiler component consumes it yet, and it does not infer source lifetimes from Rust borrowing.

1. **Allocation origin.** Every region value has an origin `(r, slot, serial)`: the region capability `r` (runtime `RegionId`), and an allocation identity that is never reused within `r`. Origins, not source variable roots, decide aliasing. Two handles alias exactly when their origins are equal; distinct live origins in one region never overlap in memory. Relocating growth produces a new origin for the same logical collection; the source-level value is the unique handle, which the lowering updates in place.
2. **Handle kinds and modes.**
   - Unique owners (`RegionVec[r, T]`, `RegionString[r]`, `RegionBox[r, T]`): affine at least, linear when `T` is linear. They are the only handles through which mutation or growth is possible.
   - Shared read handles (`RegionSlice[r, T]`, `RegionStr[r]`, `RegionBytes` read access): `copy` mode; they never provide mutation. Freezing consumes the unique owner.
   - `RegionBytes` is an existing copyable mutable byte handle; for `RegionAccessV1` the checker treats mutable access through it as requiring exclusive region access plus a runtime distinctness check (`bytes_mut_pair`), as already documented in v0.
3. **Exclusive mutation.** Any mutation, growth, shrink, release, rewind, reset or freeze requires the region capability exclusively (`inout r`, lowered to `&mut Region`). Shared views require `in r` (lowered to `&Region`) and must not be live across any exclusive operation on `r`. The checker must enforce this on the source side; the runtime enforces it for Rust callers through borrowing. Two simultaneous mutable views into one region are permitted only for distinct origins with a runtime check.
4. **Generation and watermark invalidation.** A watermark `w` taken when `slot_len = k` covers every origin with slot index `>= k`. `rewind(w)` invalidates every covered origin (including all copies of shared handles) and every newer watermark; `reset` invalidates all origins and watermarks; release invalidates one origin. Invalidated origins are rejected even after slot index and address reuse. A collection whose origin predates the newest watermark may be mutated within capacity inside the window (the elements survive the rewind), but may not grow; the checker should require `reserve` before `checkpoint` or reject growth in the window. Elements stored into a surviving collection inside a window must not be handles covered by the watermark, or they become stale at the rewind (runtime-checked, but the checker should reject it).
5. **Linear payload obligations.** A nonempty linear collection is one live obligation for its region. It must be emptied by `pop`/`drain` (each element then consumed exactly once) before region exit, any covering rewind, reset, release or freeze. Truncation that would discard linear elements is rejected. The runtime refuses these operations and returns the owner or watermark, and reports abandoned obligations at `close`, but exact-once consumption on every CFG path (including `Fail` and aborts) remains a compiler obligation.
6. **Affine finalization.** Elements with drop glue are finalized exactly once: highest index first within a buffer, LIFO across buffers and boxes, at truncate/clear/release/rewind/reset/close. Element types without drop glue register nothing.
7. **Allocation failure.** Growth and construction are fallible and return the uninserted owned value or the owner; they never abort. A source `push` must surface failure (for example a typed `Fail[RegionExhausted]` row entry, pending language review) and must not discard a linear argument.

### Validation

Run on the cloud host from the repository root (rustc 1.97.0 stable; Miri from nightly `rustc 1.102.0-nightly (3ef0c22cd 2026-11-11)`):

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy -p vesper-alloc --locked --all-targets -- -D warnings` | clean |
| `cargo test -p vesper-alloc --locked` | 36 existing region tests + 23 collection tests + 10 doctests (8 `compile_fail`) pass |
| `cargo +nightly miri test -p vesper-alloc` (Stacked Borrows) | 36 + 23 + 10 pass |
| `MIRIFLAGS="-Zmiri-tree-borrows -Zmiri-strict-provenance" cargo +nightly miri test -p vesper-alloc --tests` | 36 + 23 pass |
| `MIRIFLAGS="-Zmiri-strict-provenance -Zmiri-symbolic-alignment-check" cargo +nightly miri test -p vesper-alloc --tests` | 36 + 23 pass |
| `cargo metadata --no-deps --locked` | root workspace and lockfile unaffected |

The `compile_fail` doctests were each checked to fail for the intended reason (lifetime/invariance, shared view across `vec_push`, two mutable views, no `Clone`, non-`'static` elements, private string bytes) by compiling the same snippets in a scratch crate.

`tests/collections.rs` covers:

- Empty vectors; `usize::MAX`, `isize::MAX/8 + 1` and reservation-exceeding capacities; `len + additional` overflow; an exactly full maximum-size buffer; amortized growth falling back to the exact requirement.
- Zero-sized elements (unbounded capacity, no bytes, exact drop counts) and over-aligned elements (256-byte alignment preserved across relocation; 1024-aligned ZST; 8192 rejected).
- In-place tip growth versus relocation; relocation with interleaved allocations neither drops nor duplicates elements.
- Failed growth from backing refusal, the allocation-table limit and `ProtectedByWatermark`: value returned, elements, capacity and statistics unchanged.
- Drop-counted non-`Copy` payloads across push/pop, growth, failed insertion, truncate, clear, drain, release, rewind, reset and region drop, including LIFO/highest-index-first order; panicking element destructors and a panicking drain sink (no double drop, chunks returned).
- The §15.5 pattern: reserve before the window, push inside per-batch watermarks, scratch vectors stale after each rewind.
- Stale handles after rewind/reset with slot and address reuse; foreign vector, string and slice handles; copied slice aliases sharing one read-only view and going stale together.
- Linear elements: obligation moves on relocation, blocks truncate/reset/rewind/release/freeze (owner or watermark returned), discharged by pop/drain; `close` reports abandoned elements and still finalizes them.
- UTF-8: invalid bytes (0xFF, overlong, surrogate, truncated, stray continuation, above U+10FFFF) rejected without allocating or changing a string; split multibyte appends rejected; non-boundary truncation rejected; multibyte push/pop; `&mut str` edits; growth failure leaves content unchanged; frozen strings.
- A deterministic randomized model check (5,000 steps natively, 200 under Miri) of construct/push/pop/truncate/reserve/checkpoint/rewind/release against a `Vec` model, asserting contents after every step, no double drop, no early drop, and that every value is dropped exactly once by the end.

Not validated: the full root acceptance suite (`cargo test --workspace`, Lean, AArch64, native host scripts), because no other crate depends on `vesper-alloc` and those inputs are unchanged; non-Linux hosts; performance.

### Limitations and decisions

- **Bump-region growth wastes memory.** Relocation leaves the old buffer as a released slot that is reclaimed only at tip release, a covering rewind or reset. Repeated relocation of interleaved vectors therefore consumes reservation and slot-table entries; reserving capacity up front avoids it. Contiguous virtual reservation (§15.3) would allow in-place tip growth across chunk boundaries; it is not implemented.
- **No insert/remove at arbitrary positions, sort, iterator adapters, `Clone`-based extension or shrink-to-fit.** `extend_from_slice` is limited to `T: Copy` so no user code can panic mid-extension.
- **Not implemented:** `FlatTable`, `CacheBTree`, `ChunkSeq`, `TiledSeq`/`tiled(N)`, `SlotArena`, `RegionRef`, a native evidence fast path and source syntax. No zero-heap claim: bookkeeping is host-heap metadata.
- **The runtime does not establish source linearity or escape freedom.** Rust callers may drop or forget handles; the result is reclamation at region teardown or a reported obligation, never undefined behavior. The compiler obligations listed in the contract above remain open.

### Integration steps

1. No manifest or lockfile change is needed; the crate is already a root member with no dependencies.
2. Run the root acceptance commands; optionally add `cargo +nightly miri test -p vesper-alloc` to CI.
3. The compiler owner reviews the `RegionAccess` v1 contract and the allocation-failure decision before source `Region` collections, lifetime/rewind typing or lowering are added.
