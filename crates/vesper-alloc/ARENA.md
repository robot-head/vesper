# Checked affine slot arenas

`RegionSlotArena<T>` stores affine Rust values in one region-backed entry vector.
Its resident header records arena identity, live count, free slots, and the next
generation. `T` needs only `'static`: it need not implement Copy, Clone, Default,
Hash, or Eq. Ordinary Drop-bearing values and zero-sized values are supported.

Construct an owner with `Region::arena_new` or `arena_with_capacity`, insert to
receive an `ArenaKey<T>`, and access values through the same region and owner.
Keys are copyable weak references with runtime arena, slot, and generation
checks. Copying a key does not own its value. Removing a value transfers it to
the caller and makes its old key stale; slot reuse issues a new generation.
`arena_get_disjoint_mut` checks both keys, rejects a valid repeated key, and
returns distinct mutable values in the requested argument order.

The owner is move-only. Dropping or forgetting it does not release storage.
Release, covering rewind, reset, and region drop finalize still-occupied values
on ordinary nonpanicking paths. Removed values remain caller-owned through that
cleanup. Views borrow Region, so safe Rust excludes mutation, growth, rewind,
reset, and destruction while a view is live. Keys do not pin addresses; growth
can relocate entries while preserving their checked identities.

Capacity counts usable payload slots and excludes the resident header. Vacant
slots count toward `arena_reserve`'s additional-insertion capacity. The usable
slot limit is `u32::MAX`; checked byte/isize arithmetic, the complete 64 KiB entry
layout ceiling, and RegionConfig's finite memory/allocation/alignment caps also
apply. Empty arenas still reserve a header entry. There is no separate 64-byte
field-alignment or zero-sized-value prohibition. Unsupported representable entry
layouts fail before entry construction/allocation; Rust may reject impossible
type layouts during compilation instead.

Failed insertion returns the original value and leaves existing keys and values
unchanged. Ordinary allocation, overflow, and protected-growth failures retain
the underlying RegionVec failure guarantees. A failed consuming release returns
its original owner. Arena IDs and generations never wrap or revive stale keys;
diagnostic inspectors do not provide a public key constructor.

Watermarks are allocation boundaries, not collection snapshots. Removing,
reusing free slots, and appending within preallocated capacity in a premark arena
survive scratch rewind. Growing that protected buffer is refused unchanged. A
postmark arena becomes stale after rewind, including after relocation; a new
arena cannot revive its old owner or keys.

The region byte cap bounds resident entry storage. A caller-supplied String,
Box, Rc, or other payload can already own transitive heap allocations, which
remain outside that cap. Existing bounded region bookkeeping also remains.
Physical allocation failure, destructor panics/user callbacks, and process abort
retain the existing RegionVec scope; this profile adds no transactional panic
recovery or destructor-time guarantee.

Run the self-checking cyclic graph example with:

```sh
cargo run -p vesper-alloc --example arena_graph
```

It traverses a three-node cycle with a finite step bound, mutates two nodes
through disjoint checked projections, removes a node, rejects its stale edge,
repairs the remaining graph, and keeps the removed value alive through release.

This is a host-runtime affine arena and graph-handle foundation. It does not add
source-language region branding, static cross-arena checks, linear-mode policy,
RegionRef lowering, garbage collection, permanent raw references, parallel
mutation, Pin, VirtualArena/mmap provisioning, SIMD, Lean proofs, throughput, or
RSS guarantees. Native tests, hosted/platform testing, and Miri are distinct
evidence and should be reported only when executed.
