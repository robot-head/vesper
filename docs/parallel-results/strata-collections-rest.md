# Strata collections rest (wave nine lane 33, claim #610)

> The [manuscript](../manuscript/README.md) is the sole specification (§13.3 and
> §13.5 in its numbering; §15 in the legacy numbering of the lane prompt).

Branch `claude/project-thread-kg01m1`, base `origin/main` at
`5a018bd`, a descendant of the required `37f7b8b`. Owned paths only:
`crates/vesper-alloc/**` and this file. No root patch, no new dependencies.

## Scope change (2026-11-16)

Midway through the lane the owner approved a new plan: crates made of pure
logic (Strata collections among them) will be ported to Vesper source, so
Rust gets only bug fixes, thin OS/FFI shims, and whatever the tests need.
This lane was narrowed accordingly:

- **Kept and finished:** the OS-level VirtualArena work (Windows reservation,
  2 MiB hint, region chunks over one reservation). It is a thin platform shim
  that a Vesper port would call through an `extern` boundary anyway.
- **Turned into specification:** `CacheBTree` and `ChunkSeq` are specified by
  checked-in conformance traces plus a differential harness. A small Rust
  `CacheBTree` reference (already written when the plan changed) stays as the
  harness's second implementation; `ChunkSeq` has no Rust implementation.
- **Deferred to the Vesper port, specified below:** the lock-free remote-free
  queue, per-core shard selection and NUMA placement. `slab/sharded.rs` is
  unchanged (its remote queue is still the `Mutex<Vec>` of wave eight).

## What was built

| Item | Implementation |
| --- | --- |
| Windows reservations | `VirtualArena` on Windows now reserves with `VirtualAlloc(MEM_RESERVE, PAGE_NOACCESS)`, commits the used prefix with `VirtualAlloc(MEM_COMMIT)` in 64 KiB steps as the cursor or the tip grows, decommits on `reset` (`VirtualFree(MEM_DECOMMIT)`) and releases on drop (`MEM_RELEASE`). New `VirtualBackingKind::VirtualAlloc`; a refused commit is `VirtualArenaError::CommitFailed`. kernel32 is declared directly (`extern "system"`), so there is no `windows-sys` dependency. Before this, Windows used an eager `alloc_zeroed`. |
| 2 MiB page hint | `VirtualArenaConfig::huge_page_hint()` (field `huge_pages`). On Unix the mapping is over-reserved by 2 MiB and trimmed so the base is 2 MiB aligned; Linux then calls `madvise(MADV_HUGEPAGE)`. `HugePageStatus` reports `Advised`, `Refused { errno }`, `AlignedOnly` (macOS: no advisory call) or `Unsupported` (Windows: large pages need `SeLockMemoryPrivilege` and eager commit, which defeats lazy reservation; global-allocator path). `resident_huge_bytes()` reads `AnonHugePages` from `/proc/self/smaps`. |
| Region chunks over a reservation | `VirtualChunkBacking` implements `ChunkBacking` over one reservation (same platform layer): chunks are bump-allocated in address order, so a growing `Region`'s chunks are adjacent; a chunk returned at the tip lowers the cursor (absorbing holes) and its pages go back to the OS (`MADV_FREE` / `MEM_DECOMMIT`); a chunk returned below the tip becomes a coalesced hole reused first-fit. `Region::new_virtual(config, huge_pages)` sizes the reservation with `reservation_for` (2 x `max_reserved_bytes`, because a region obtains a replacement chunk before it releases its spares, plus alignment slack). The region's own budget and errors are unchanged. |
| Shared platform layer | `src/virtual_arena/os.rs` (`Reservation`): the old inline `mmap` code plus the Windows and huge-page paths; `VirtualArenaStats` gained `committed_bytes` and `huge_pages`. |
| CacheBTree reference | `CacheBTree<K: Copy + Default, V, O: KeyOrder<K> = Canonical>`, safe Rust (`#![forbid(unsafe_code)]`): B = 16 keys per `#[repr(align(64))]` node, leaves and inner nodes in two slabs addressed by `u32` indices (relocatable by move or clone, nothing pinned), branchless 16-lane rank count for search, leaf chain for `iter`/`range`, split on insert and borrow/merge on remove, intrusive free lists so churn reuses nodes, `max_len` bound with typed `CacheBTreeInsertError` that returns the entry, and nodes provisioned before a split cascade so an allocation failure leaves the tree intact. The order is a zero-sized brand: `CacheBTree<K, V>` and `CacheBTree<K, V, ReverseOrder<Canonical>>` are distinct types (compile-fail doctest). The rank loop is written for auto-vectorization; there are no explicit SIMD intrinsics. |
| Conformance traces | `tests/fixtures/strata/*.trace` (7 files, about 56,000 lines) with `README.md` describing the format: five `CacheBTree` traces (oracle `BTreeMap`) and two `ChunkSeq` traces (oracle `Vec` plus named frozen snapshots). `tests/collections_conformance.rs` regenerates each trace from its seed and requires a byte-identical file (`VESPER_BLESS=1` rewrites), then replays the map traces against the Rust reference with `check_invariants` after every mutation. |

## Checks

All on Linux x86_64 in the cloud session (4 vCPUs, one NUMA node, THP
`enabled=[madvise]`, `defrag=[madvise]`):

- `cargo test -q -p vesper-alloc`: every target passed (23 result lines, 0 failures).
- `cargo +1.100.0 clippy -p vesper-alloc --all-targets --locked -- -D warnings`: clean.
- `cargo clippy -p vesper-alloc --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings` (1.97): clean; the Windows path is compiled here and run only by Windows CI.
- `cargo fmt -p vesper-alloc`, `python3 tools/bazel/sync_build_files.py`: no BUILD change needed.
- Huge pages were actually exercised: after touching 8 MiB of a hinted 16 MiB arena, `/proc/self/smaps` reported `AnonHugePages` 8,388,608 bytes. The test only requires the status and the smaps read, since THP use depends on host settings.
- `tests/cache_btree.rs`: 200,000 random operations against `BTreeMap` reaching height 4 and draining to empty; ranges, bounds, reverse brand, limits, relocation by move; **0 allocations** over 20,000 warmed remove+insert pairs and 2,048 lookups, measured with a counting global allocator (per-thread counter).

## Port specification (deferred work)

The Vesper port of these collections must pass the traces above and the
existing `vesper-alloc` tests for the behaviour it replaces. In addition:

**ChunkSeq[T].** A 32-way strictly radix-balanced tree with a tail chunk.
Uniquely owned chunks are mutated in place (`set`, `push`, `pop` allocate
nothing except a new chunk every 32 pushes and path nodes when the height
grows); `freeze` is O(1) and yields a value that is safe to share across
`Pulse::scope` fibers; `thaw` is O(1) and copies a path only on the first
write. Acceptance: both `chunk_seq_*` traces; a counting-allocator test that
in-place `set` on a unique sequence allocates 0; a test that a frozen
snapshot read concurrently from several fibers is unaffected by writes to a
thawed copy. Relaxed (RRB) concatenation is not specified.

**Lock-free remote-free queue (§13.3 Tier 1).** Replace the per-shard
`Mutex<Vec>` with an intrusive multi-producer stack of 64-slot batch nodes
(push by CAS with Release, consume by one `swap(null, Acquire)`, so there is
no ABA), with consumed batch nodes returned to their sender through a second
such stack, so that steady-state cross-shard frees allocate nothing.
Acceptance: the existing `tests/sharded.rs` and the remote-free case of
`tests/zero_alloc.rs` unchanged, plus a many-thread stress test under Miri or
loom-style interleavings.

**Per-core shards and NUMA.** A claim helper that picks shard
`current_cpu % shards` (`sched_getcpu` on Linux, `GetCurrentProcessorNumber`
on Windows, none on macOS) and falls back to the next free shard; slab memory
placed on the shard's node with `mbind(MPOL_PREFERRED)` where the host exposes
more than one node. This host has one node (`/sys/devices/system/node/node0`
only), so multi-node placement could not be exercised here; the report of the
port must say so unless it runs on a multi-node host.

## Remaining gaps and decisions

- No Lean change; `lean/StrataCollections.lean` is untouched.
- `ChunkSeq`, the lock-free queue, per-core claiming and NUMA have no
  implementation (by the 2026-11-16 decision); only their specification above.
- `VirtualChunkBacking` does not release pages of holes below the tip (only
  the tail), because the Windows commit model is a prefix; holes are reused.
- Windows large pages and macOS superpages are not attempted.
- Rust ownership is not evidence that the Vesper source typing rules for these
  collections are implemented.
