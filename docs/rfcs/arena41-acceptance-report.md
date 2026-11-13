# Arena lifecycle acceptance (claim #226)

Added independent public-API lifecycle coverage in `crates/vesper-alloc/tests/arena_region.rs`, the executable `examples/arena_graph.rs`, and the host profile `ARENA.md`. No allocator or collection implementation, public API, dependency, build file, or unrelated test changed in this lane.

The 11 lifecycle tests use a counting/failing backing that records every live chunk address and its requested Layout, rejects unmatched/double deallocation, and requires exact allocation/deallocation balance after region destruction. Drop-bearing payloads own caller-created Box buffers and record finalization independently of arena metadata. Coverage includes:

- Removed values retain their original allocation and remain caller-owned through release, reset, and region drop. Occupied values finalize exactly once; ordinary owner scope exit and explicit owner forgetting leave finalization to Region.
- A live blocking allocation forces actual entry-buffer relocation, checked by different payload addresses. Values and keys survive relocation without premature Drop, and postmark relocated owners become stale after rewind.
- Backing refusal, reservation limits, tracked-allocation limits, alignment refusal, unsupported entry layouts, and checked capacity/reserve overflow preserve RegionStats and live values. Failed insertion returns the original Box allocation; a retry receives the next generation without a failed insertion consuming it.
- Premark removal/free-slot reuse and preallocated append survive scratch rewind, including mutations and newly issued keys. Protected growth preserves all state and returns the original input. Reused slot numbers/new arena identities cannot revive old keys or stale owners.
- Every checked inspection/key operation and reserve0 rejects foreign and stale owners. Consuming release returns its original logical owner on validation failure.
- Zero-sized Drop values remain counted logical occupied values. Reset finalizes remaining occupants while an explicitly removed value stays alive until its caller drops it.

The example creates a real three-node cycle with String-owning affine nodes, traverses it with an explicit finite step bound, mutates two nodes through reversed-order disjoint projections, removes a node, rejects its stale edge, repairs/traverses the two-node cycle, and keeps the removed String alive through arena release. Its self-checked output is:

```text
nodes=2 initial_cycle_sum=12 updated_cycle_sum=46 repaired_cycle_sum=44 stale_edge=rejected
```

`ARENA.md` states the runtime identity/ownership/watermark behavior and distinguishes resident region bytes from caller-owned transitive heap allocations. It makes no source region-branding, Vesper linear-mode, RegionRef/compiler, garbage-collection, Pin, VirtualArena/mmap, SIMD, Lean, throughput, or RSS claim. Ordinary nonpanicking finalization is tested; stronger destructor-panic recovery is not asserted. Two narrowly scoped Clippy expectations allow the explicit `mem::forget` client scenarios even though the present owner handle has no Drop glue; no runtime tests are skipped or weakened.

## Actual validation

Builds began only after coherent source and public exports were published. Dependency commits imported for validation were `e1b0e4c`, `5fcb1b3`, `4e37bbe`, `dde30ee`, and `da02a88`; none is owned by this lane. Final checks used the constructor-frame and checked-atomic fixes, with no deprecation allowance or source adapter.

From `/workspace/vesper-worktrees/collections41-arena-region`:

```sh
export CARGO_TARGET_DIR=/tmp/collections41-arena-region-target
export CARGO_BUILD_JOBS=1
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_INCREMENTAL=0
cargo test -p vesper-alloc --locked --jobs 1
cargo run -p vesper-alloc --locked --jobs 1 --example arena_graph
cargo clippy -p vesper-alloc --locked --jobs 1 --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Results: all 11 owned lifecycle tests passed; the complete available allocator package run passed 96 native tests (including these 11 and existing allocator tests) and 25 Rustdocs, with no ignored tests. The actual example produced the self-checked output above. Strict all-target Clippy, workspace formatting, and whitespace checks passed. This lane used only its private small target cache. Miri, independent model suites, combined integration testing, hosted/platform execution, push, pull request, and merge are coordinator work and are not claimed here.
