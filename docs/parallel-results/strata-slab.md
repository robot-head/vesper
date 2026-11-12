# Bounded local ShardSlab byte allocator

Base: `1ffd0aec27dfcc24c5564585ae9068255490bf61`. Branch:
`parallel-next/strata-slab`. Owned paths: `crates/vesper-alloc/**` and this report.
No manifest, lockfile, compiler, scheduler, ABI or existing Region behavior changes.

## Delivered behavior

`ShardSlab<B = SystemBacking>` implements a host-runtime slice of §15.3:

- Fixed 64 KiB slabs with 16 KiB base alignment, and 48 explicit size classes
  from 8 bytes through 16 KiB. Each slab serves one class. Class selection checks
  both requested size and divisibility by alignment. Unsupported sizes/alignment
  return errors; there is no large-object/global-heap fallback for payloads.
- `SlabConfig` bounds physical slabs and concurrent live blocks. Configuration
  permits at most 4096 slabs, checks the possible slot budget, and reserves the
  bounded deferred-free queue fallibly before exposing a pool. Metadata has at
  most 8192 slots per slab and 48 class lists. Metadata uses host `Vec` storage;
  this is not a zero-host-heap allocator.
- Unique `SlabBlock<B>` owners expose only initialized byte slices. New
  allocations are zeroed, `allocate_copy` copies borrowed bytes, and resized
  extensions are zeroed even after shrinking. Zero-length owners occupy the
  smallest compatible slot while exposing an empty slice.
- Pool IDs never repeat process-wide; successful allocations receive a
  monotonically increasing generation. Copyable `SlabKey` snapshots validate
  origin/liveness but cannot produce a reference, manufacture an owner or free
  an allocation. Released keys stay stale even when a slot/address is reused.
- `free`, `resize` and local `transfer_from` return the complete original block
  on failure. Growing replacement allocation and transfer copy before releasing
  the source. Shrinking retains the original class; free slabs retain their
  original class until pool destruction. Counts distinguish requested bytes,
  occupied class bytes and full reserved slabs (including unusable slab tails).
- Blocks hold local non-atomic `Rc` shares of their pool state. A block remains
  readable/mutable after the pool handle is dropped. The last owner returns all
  slabs to their originating `ChunkBacking` with the exact original layout.
  `close` instead reports live blocks and returns the pool for a retry.
- Custom backing callbacks cannot reenter mutable pool bookkeeping: operations
  return `Busy`, preserving consuming owners. A block dropped during provisioning
  defers its free into a pre-reserved queue. The next bookkeeping operation
  drains it without a second allocation or a `RefCell` panic.

The warm local slot pop/push, generation/count bookkeeping, `Rc` updates, and
initialized-byte access contain no atomic operations in this implementation.
Only construction uses the process-wide atomic pool-ID counter. Cold slab
provisioning invokes the backing/global allocator and allocates metadata; those
providers can use locks/atomics. No assembly instruction count, lock-free system
allocation, latency or performance claim is made. The freelist links are checked
host metadata, rather than pointers written over user payload bytes.

## API

```rust
ShardSlab::new(SlabConfig) -> Result<ShardSlab, SlabError>
ShardSlab::with_backing(SlabConfig, B) -> Result<ShardSlab<B>, SlabError>
pool.allocate(size, align) -> Result<SlabBlock<B>, SlabError>
pool.allocate_copy(&[u8], align) -> Result<SlabBlock<B>, SlabError>
pool.free(block) -> Result<(), SlabOwnerError<SlabBlock<B>>>
pool.resize(block, size, align) -> Result<SlabBlock<B>, SlabOwnerError<SlabBlock<B>>>
pool.transfer_from(source_block) -> Result<SlabBlock<B>, SlabOwnerError<SlabBlock<C>>>
pool.validate(SlabKey) -> Result<(), SlabError>
pool.stats() -> Result<SlabStats, SlabError>
pool.close() -> Result<SlabStats, SlabCloseError<B>> // error returns pool
block.key(); block.capacity(); block.alignment(); block.as_ptr();
// Deref<Target=[u8]> / DerefMut, with unique Rust borrows.
```

There is no `Clone`/`Copy` block, public raw-owner constructor or ownership
conversion from a key. `as_ptr` only inspects an address; dereferencing a retained
raw pointer is an unsafe caller operation. Both pools and blocks are `!Send` and
`!Sync`; cross-thread frees are rejected by the Rust type boundary and covered
by compile-fail examples. `transfer_from` is a local copy-transfer, not worker
migration or a remote-free queue.

## Validation

Rust `1.99.1`, Linux x86_64. Each command used isolated `CARGO_TARGET_DIR=target-slab`,
`CARGO_BUILD_JOBS=1`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`,
`CARGO_INCREMENTAL=0`.

- `cargo test -p vesper-alloc --locked`: 84 tests/doctests pass (1 slab exhaustion
  unit, 12 new slab integration, 36 existing region, 23 existing collection,
  12 doctests including two new confinement examples).
- `cargo test -p vesper-alloc --locked --release --test slab`: all 12 pass,
  including the actual deferred-release path with assertions disabled.
- `cargo clippy -p vesper-alloc --locked --all-targets -- -D warnings`: passes.
- `rustfmt --edition 2024 --check` on changed Rust files and `git diff --check`:
  pass.
- `cargo miri --version`: unavailable on installed stable toolchain; no Miri
  execution is claimed.

Tests perform actual aligned allocations across every size class and all
compatible alignments, check initialized bytes/disjoint live payloads, exhaust
slab and owner budgets, observe pointer reuse with stale-key rejection, inject
backing refusal, retain owners on foreign/busy free and failed resize/transfer,
observe backing deallocation after pool-handle destruction, retry close, force
ID/generation exhaustion without wrap, and compare 2000 deterministic reuse
operations against live payload identities. Counting backing fixtures verify
all normally released slabs are returned exactly once.

## Explicit limits

No remote free batching, worker migration, typed slab boxes/finalizers, source
Alloc typing/lowering, intrusive payload freelists, DMA pinning/registration,
hugepages, MPK, GC, allocator benchmarking or VirtualArena changes. No unsafe
`Send` implementation is introduced. User code can still `mem::forget` a block,
leaking its local reference and retaining a live slot; ordinary Rust `Drop`/`Rc`
is not proof of Vesper source-level linear consumption. Existing Region linear
obligation and collection contracts remain separate and unchanged.

No coordinator registration patch is needed: the allocator crate already
belongs to the workspace and has no added dependencies.
