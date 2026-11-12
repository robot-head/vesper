# Strata region allocator core (design §15)

Lane: Strata region allocator. Initial core merged as PR #1 (`c09c014`); review follow-up on branch `claude/project-thread-d6page`, based on `origin/work` at `72493b8`.
Owned paths: `crates/vesper-alloc/**` and this file. No root workspace, lockfile, CLI, CI, syntax, checker, IR, or codegen file is changed.

## What is implemented

`crates/vesper-alloc` is a standalone host-runtime library (temporary `[workspace]` table, no dependencies) implementing the VirtualArena half of §15.3 as a checked region core for `eff [r: Region]` (§15.2):

| Requirement | Implementation |
| --- | --- |
| Bounded region | `RegionConfig` bounds total reserved bytes, chunk growth, maximum alignment, tracked allocations, and outstanding watermarks. Exceeding any bound is an error; nothing silently falls back to the global heap. |
| Aligned allocation | Alignment is computed on absolute addresses, so any power of two up to `max_align` (default 4096) is honored inside a chunk. A new chunk is laid out with `max(16, align)` so a fresh chunk never needs padding. |
| Checked size arithmetic | Every cursor, padding, length, growth, and reservation computation is checked; sizes above `isize::MAX` and overflowing extensions return `SizeOverflow`. Padding is part of the fit computation. |
| Growth | Chunks start at `initial_chunk_bytes`, double up to `max_chunk_bytes`, and an oversized request gets a dedicated chunk if the reservation allows it. Chunks never move, so addresses are stable. |
| Owned identity/generation | Each region has a process-unique `RegionId`. Each tracked allocation and each watermark has a serial that the region never reuses. `RegionStats::epoch` counts rewinds and resets. |
| Reset/reclamation | `checkpoint`/`rewind` give LIFO O(1) bump reset (plus finalizer work); spare chunks obtained after the watermark are retained for reuse. A request that fits no spare replaces the unusable spares (their capacity counts toward the replacement), but they are returned only after the backing supplies the replacement, so a failed request keeps them. `reset` keeps up to `retained_bytes` of chunks and returns the rest to the backing. Releasing or taking the newest allocation reclaims its bytes (and padding) immediately, cascading through earlier released slots and across emptied chunks, stopping at the newest watermark and at untracked raw allocations. |
| Stale/cross-region rejection | Handles carry `(RegionId, slot, serial)`. Another region's handle yields `CrossRegion` even when its slot index and serial coincide with a local allocation. A rewound, reset, released, or relocated allocation yields `Stale` even after its slot index and bytes are reused. Watermarks invalidated by an outer rewind or reset are also `Stale`. |
| Byte buffers | `RegionBytes` (`RegionSlice[r, u8]`): zero-initialized or copied allocations, `bytes`/`bytes_mut`, disjoint `bytes_mut_pair`, and `extend_bytes`, which grows the unprotected tip in place or relocates (old handle becomes stale; on failure the handle and contents are unchanged). |
| Region boxes | `RegionBox<T>` (`RegionBox[r, T]`, invariant in `T`): `alloc_value` (copy/affine) and `alloc_linear`; `get`, `get_mut`, `take`. A failed allocation returns the value (`ValueAllocError { value, error }`). Values needing drop go on the region's LIFO finalizer list; `take` unregisters them. |
| Linear payloads | A live linear payload makes `rewind` over it fail (the watermark is returned so the caller can consume the payload and retry), makes `reset` fail, and makes `close`/`scoped` report `LinearObligationsLive`. |
| Cleanup on error | `Region::scoped`/`scoped_in` close the region on return; unwinding drops it. Drop finalizes remaining payloads in LIFO order, continues after a panicking finalizer (a second panic aborts, as for `Vec`), and always returns every chunk. Finalizer slots are removed before their finalizer runs, so a panic cannot double-drop. |
| Explicit ownership, no GC/ARC | A region is uniquely owned, has no `Clone`, and is `!Send`/`!Sync` (shard-confined). Payloads are not reference-counted. The only atomic is the process-wide `RegionId` counter. Region metadata (chunk list, slot table, watermark stack) uses ordinary `Vec`s on the host heap. |

## Public API

```rust
pub const CHUNK_ALIGN: usize = 16;
pub const REGION_LOWERING_CONTRACT_VERSION: u32 = 0;

pub unsafe trait ChunkBacking {
    fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>>;
    unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout);
}
pub struct SystemBacking;                       // std::alloc global allocator

pub struct RegionConfig { initial_chunk_bytes, max_chunk_bytes, max_reserved_bytes,
                          max_align, max_allocations, max_checkpoints, retained_bytes }
impl RegionConfig { fn bounded(max_reserved_bytes) -> Self; fn validate(&self) -> Result<(), ConfigError> }

pub struct Region<B: ChunkBacking = SystemBacking>;
impl Region {
    fn new(RegionConfig) -> Result<Self, ConfigError>;
    fn scoped<R>(RegionConfig, impl FnOnce(&mut Region) -> R) -> Result<R, RegionError>;
}
impl<B: ChunkBacking> Region<B> {
    fn with_backing(RegionConfig, B) -> Result<Self, ConfigError>;
    fn scoped_in<R>(RegionConfig, B, impl FnOnce(&mut Region<B>) -> R) -> Result<R, RegionError>;
    fn id(&self) -> RegionId; fn config(&self) -> &RegionConfig; fn backing(&self) -> &B;
    fn stats(&self) -> RegionStats;
    // bytes
    fn alloc_bytes(&mut self, len, align) -> Result<RegionBytes, AllocError>;
    fn alloc_bytes_copy(&mut self, &[u8], align) -> Result<RegionBytes, AllocError>;
    fn len(&self, &RegionBytes) -> Result<usize, HandleError>;
    fn bytes(&self, &RegionBytes) -> Result<&[u8], HandleError>;
    fn bytes_mut(&mut self, &RegionBytes) -> Result<&mut [u8], HandleError>;
    fn bytes_mut_pair(&mut self, &RegionBytes, &RegionBytes) -> Result<(&mut [u8], &mut [u8]), HandleError>;
    fn extend_bytes(&mut self, &mut RegionBytes, &[u8]) -> Result<(), RegionError>;
    fn release_bytes(&mut self, RegionBytes) -> Result<(), HandleError>;
    fn bytes_ptr(&self, &RegionBytes) -> Result<NonNull<u8>, HandleError>;
    fn alloc_raw(&mut self, size, align) -> Result<NonNull<u8>, AllocError>;   // untracked
    // typed
    fn alloc_value<T: 'static>(&mut self, T) -> Result<RegionBox<T>, ValueAllocError<T>>;
    fn alloc_linear<T: 'static>(&mut self, T) -> Result<RegionBox<T>, ValueAllocError<T>>;
    fn get<T>(&self, &RegionBox<T>) -> Result<&T, HandleError>;
    fn get_mut<T>(&mut self, &RegionBox<T>) -> Result<&mut T, HandleError>;
    fn take<T>(&mut self, RegionBox<T>) -> Result<T, HandleError>;
    // lifetime management
    fn checkpoint(&mut self) -> Result<Watermark, RegionError>;
    fn rewind(&mut self, Watermark) -> Result<(), RewindError>;
    fn commit(&mut self, Watermark) -> Result<(), HandleError>;
    fn reset(&mut self) -> Result<(), RegionError>;
    fn close(self) -> Result<RegionStats, RegionError>;
}
// Handles: RegionBytes (Copy), RegionBox<T> (move-only), Watermark (move-only, #[must_use]), RegionId.
// Errors: ConfigError, AllocError, HandleError, RegionError, RewindError, ValueAllocError<T>.
```

## Validation

All run from `crates/vesper-alloc` on the cloud host (rustc 1.97.0 stable; Miri from nightly 2026-11-10):

| Command | Result |
| --- | --- |
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo test` | 36 integration tests + 2 doctests (one `compile_fail`) pass |
| `cargo +nightly miri test` (Stacked Borrows, leak check on) | 36 + 2 pass |
| `MIRIFLAGS="-Zmiri-tree-borrows -Zmiri-strict-provenance" cargo +nightly miri test` | 36 + 2 pass |
| `MIRIFLAGS="-Zmiri-strict-provenance -Zmiri-symbolic-alignment-check" cargo +nightly miri test` | 36 + 2 pass |
| `cargo metadata --no-deps --locked` at the repository root | root workspace unaffected |

Tests (`tests/region.rs`) use a counting backing with failure injection, so chunk reclamation is observed rather than assumed:

- Alignment: every power of two from 1 to 4096 with zero, small, and boundary lengths after a perturbed cursor; over-aligned typed values; invalid (0, 3, 24) and oversized alignments.
- Checked arithmetic and bounds: `usize::MAX`, `isize::MAX + 1`, padding-induced exhaustion, reservation limits with an unchanged region after failure, allocation/watermark limits, config validation.
- Growth: geometric chunk growth to the cap, dedicated oversized chunks, in-place tip growth versus relocation, growth failure preserving the buffer, and spare chunks too small for a request being returned.
- Overlap: a deterministic randomized model check (4,000 steps natively, 150 under Miri) of allocate/extend/release/checkpoint/rewind/commit, asserting after every step that live allocations are pairwise disjoint, aligned, hold their model contents, that every stale handle is rejected, and that every operation path was exercised. Plus `bytes_mut_pair` aliasing rejection.
- Reuse: rewind reuses the same address with zeroed bytes and reuses spare chunks without new backing allocations; reset retains only `retained_bytes`; tip release and `take` reclaim bytes and padding; tail reclamation does not cross a watermark or an untracked raw allocation.
- Stale/cross-region: identical slot/serial pairs in two regions, handles and watermarks across regions, outer rewind invalidating inner watermarks, commit semantics, stale typed handles after their memory is reused.
- Finalizers and linear payloads: LIFO order across rewind/reset/drop, taken values not finalized, linear payloads blocking rewind/reset with the watermark returned, abandoned linear payloads reported by `close`/`scoped`.
- Cleanup on error: `scoped` with an `Err` body, with an abandoned linear payload, with invalid config, a panicking body, and a panicking finalizer (no double drop, remaining payloads finalized, all chunks returned).

Not validated here: the root workspace acceptance commands (`cargo test --workspace --locked` etc.), because the crate is not yet registered; behavior on AArch64 or non-Linux hosts; any performance property.

## Review follow-up (Codex review of PR #1)

Four findings were verified and fixed, each with a regression test that failed before the fix:

1. **Soundness (P1):** `RegionBox<T>` used `PhantomData<fn() -> T>` and was therefore covariant. Because `get_mut` writes through `&RegionBox<T>`, safe code could view a `RegionBox<&'static str>` as `RegionBox<&'a str>`, store a local borrow, and read it back as `'static`. The marker is now `PhantomData<fn(T) -> T>` (invariant). A `compile_fail` doctest checks that the coercion is rejected; the same snippet was confirmed to compile with the old marker.
2. **Soundness (P1):** `SystemBacking::allocate` is a safe method that any caller can use, and it guarded the nonzero-size precondition of `std::alloc::alloc` only with `debug_assert!`. It now returns `None` for zero-sized layouts.
3. **Failure atomicity (P2):** a request that fit no spare chunk released the spares before asking for a replacement, so a failed request could still discard retained capacity. Spares are now released only after the replacement chunk is obtained.
4. **Reclamation (P2):** tail reclamation stopped at a chunk boundary because the active chunk index stayed on the emptied chunk, so released slots in the earlier chunk stayed tracked and counted against `max_allocations`. The active chunk now steps back over emptied chunks, never below the newest watermark's chunk. A follow-up review found that a zero-sized allocation can open a chunk without advancing its cursor; a chunk that still holds any slot is now never treated as empty (regression test with a 4096-aligned zero-sized byte allocation and `RegionBox`).

## Proposed Alloc/Region lowering contract (v0, for compiler-owner review)

`REGION_LOWERING_CONTRACT_VERSION = 0`. This is a proposal; no compiler component consumes it yet.

1. **Scope.** `Region::scoped(n, λr. body)` lowers to `Region::with_backing(cfg(n), backing)` (or `scoped_in`), with the region object as the evidence for capability `r`. The compiler emits exactly one `close` on every exit edge of the body: normal return, `Fail` propagation, handler abort, and cancellation. `close` returning `LinearObligationsLive` is an internal compiler error in checked code, not a user-visible failure.
2. **Allocation.** `r.box(e)` lowers by the payload's kind: `copy`/`affine` to `alloc_value`, `linear` to `alloc_linear`. Byte slices and `ByteSlice`-style buffers lower to `alloc_bytes`/`alloc_bytes_copy`/`extend_bytes`. Allocation failure needs a source-level answer (see the ambiguities below); until then the proposal maps every `AllocError` to an abort with the error code, never to silent fallback onto `eff [Alloc]`.
3. **Handles.** A lowered `RegionBox[r, T]` is `{slot: u32, serial: u64}` plus the static capability `r`; the region id need not be stored when `r` is statically known, because the compiler resolves it through the evidence. Checked accessors are the reference semantics. A backend may replace a checked access with a raw pointer (`bytes_ptr`, or `alloc_raw` for untracked scratch) only for accesses the checker proves live (no intervening rewind/reset/release of that allocation).
4. **Watermarks.** `checkpoint` produces a linear `Watermark`; the checker must ensure each is consumed exactly once by `rewind` or `commit`, in LIFO order relative to nested watermarks, and that no `RegionBox`/`RegionBytes` allocated after the watermark is live after the rewind. Runtime `Stale`/`LinearObligationsLive` results are defense in depth.
5. **Finalizers.** Affine payloads needing drop are finalized LIFO by rewind/reset/close. The compiler may elide registration when the payload owns only memory in the same region (the runtime already registers nothing for types without drop glue).
6. **Fast path (not implemented).** A future native fast path would pin the active chunk's `(cursor, limit)` in the evidence slot and inline the bump; the slow path is `alloc_raw`/`alloc_*`. Bump coalescing in egglog is not part of this milestone.

### Compiler obligations versus runtime checks

Rust ownership is not a proof of Vesper's linear discipline. A Rust caller can drop a `RegionBox` or `Watermark`, `mem::forget` it, copy a `RegionBytes`, or return a handle from `scoped`. The runtime makes each of those safe (no undefined behavior): forgotten handles leak nothing past region drop, escaped handles cannot be resolved without the region, abandoned linear payloads are reported and finalized, and stale handles are rejected. It does not make them *correct*. The following stay compiler obligations from [T-RegionAlloc]/[T-RegionScope]:

- Escape freedom: `r ∉ free_caps(τ_out)` for every scope.
- Exactly-once consumption of each linear payload and each watermark on every CFG path, including `Fail` and aborts, using the existing exact-once analysis.
- No region exit, rewind, or reset while a linear payload in its span is live.
- No use of a handle after a covering rewind/reset/release.
- Aliasing is decided by allocation origin, not by source variable. Copies of a `RegionBytes` (and any future copyable `RegionRef`) name the same cell even when they flow from distinct source roots, so place-disjointness for two mutable region accesses needs either evidence that the handles have distinct `(region, allocation)` origins or exclusive (`inout r`) access to the whole region with a runtime distinctness check, as `bytes_mut_pair` does.

## Ambiguities and proposed decisions

1. **Region exhaustion semantics.** §15.2 types `bump_alloc` as infallible, but a bounded `Region::scoped(256.kib(), …)` can be exhausted. Proposal: exhaustion aborts the enclosing scope with a typed `RegionExhausted` code in v0; a later `Fail[AllocError]` row entry needs language review.
2. **Growing pre-watermark collections.** The §15.5 example pushes into `entries` (allocated before `checkpoint`) between `checkpoint` and `rewind`. In a bump region, growth after the watermark lands above it and would be discarded by the rewind while the collection survives. The runtime rejects that growth with `ProtectedByWatermark` instead of losing data. Proposal: the checker rejects growth of pre-watermark region collections inside a watermark window unless capacity was reserved before the checkpoint (a `reserve` API is future work), or the collection lives in an outer region.
3. **Linear payloads at forced teardown.** On drop or unwinding, abandoned linear payloads are finalized rather than leaked, and `close` reports the violation. Leaking was rejected because linear payloads typically own OS resources.
4. **Allocation-table bound.** Released and moved slots that are not at the tip (or are protected by a watermark) count against `max_allocations` until a covering rewind or reset. This keeps handle checks O(1) and memory proportional to tracked allocations.
5. **Chunk provisioning.** The design specifies `mmap(MAP_NORESERVE)` reservations with `MADV_FREE`. This milestone uses `ChunkBacking` (global allocator by default) and chunked growth. Contiguous virtual reservation is a later backing, not a change to the handle semantics.
6. **Mode crossing.** `RegionBytes` is `Copy`; `RegionBox<T>` is move-only even for `T: Copy`, since the runtime cannot see Vesper modes. The checker decides crossing.

## Not implemented or not claimed

ShardSlab (§15.3 tier 1) and `eff [Alloc]`, remote-free queues, MPK DmaSlab, hugepages, and `IORING_REGISTER_BUFFERS`; `mmap` virtual reservation; the pinned-register evidence ABI and egglog bump coalescing; `@fip` reuse; cyclic-region typing, `SlotArena`, `RegionRef`; `tiled(N)`/AoSoA and the collection suite. No performance, zero-allocation, or formal-soundness claim is made: there are no benchmarks, the metadata uses the host heap, and there is no Lean model of this allocator. Source syntax, checker, and codegen are untouched.

## Integration requirements

1. Remove the temporary `[workspace]` table from `crates/vesper-alloc/Cargo.toml` (and its local `.gitignore`), add `crates/vesper-alloc` to root `members` and `default-members`, and regenerate the root `Cargo.lock`. The crate has no dependencies.
2. Run the root acceptance commands; add `cargo +nightly miri test -p vesper-alloc` to CI if Miri is available there.
3. The compiler owner reviews the v0 lowering contract and the ambiguity decisions above before any source-level `Region` API is added.
