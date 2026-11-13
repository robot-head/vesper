# Region-backed scalar flat tables

`RegionFlatTable<K,V>` stores its actual control groups and inline key/value
entry objects in one region vector. It supports ordinary affine Rust payloads;
keys and values need not be Copy, Clone, Default or Debug. Caller payloads may
already own indirect heap allocations, such as a String buffer. Those indirect
allocations are not moved into the region automatically.

```rust
use vesper_alloc::{FlatConfig, Region, RegionConfig};

let mut region = Region::new(RegionConfig::bounded(1024 * 1024)).unwrap();
let mut table = region.flat_new::<u64, String>(FlatConfig::default()).unwrap();
assert!(region.flat_insert(&mut table, 7, "old".into()).unwrap().is_none());
let previous = region.flat_insert(&mut table, 7, "new".into()).unwrap().unwrap();
assert_eq!(previous, (7, "old".to_owned()));
assert_eq!(region.flat_get(&table, &7).unwrap().unwrap(), "new");
region.flat_release(table).unwrap();
```

Each group starts with exactly 16 control bytes and is aligned to at least 16.
Entry alignment can increase the complete native group alignment/stride. This
is a scalar host implementation: tag matching and bounded probing use ordinary
Rust, not SIMD. The internal Option/entry layout is not a wire or portable ABI.
The complete group must fit 64 KiB; unsupported representable layouts are
rejected before allocation and group initialization. Rust may reject impossible
type/array layouts during compilation.

`FlatConfig.max_groups` is a positive power of two, at most 4,096, with default
1,024. A new table initializes one group and has live capacity 14. Reservations
round up to a power-of-two group count, with no hidden reserved group capacity.
The live threshold is 14 times the initialized group count in aggregate; it is
not a limit of 14 occupied lanes in every individual group. Construction still
uses existing finite region alignment, reservation and allocation limits.

Keys use a fresh standard DefaultHasher and must obey stable Eq/Hash behavior
while stored. Full hashes are cached; reserve/rehash moves owned entries using
those hashes without cloning payloads or calling Hash/Eq. Probe loops are bounded
by group count and 16 lanes per group, including tables with accumulated
tombstones. Hash output is not persistent or promised stable across Rust
versions. This is not an adversarial collision-security guarantee.

Insertion returns the previous actual owned key/value pair on replacement and
does not drop either object. Removal similarly transfers the owned pair.
Returned insertion errors return the incoming pair unchanged; resource/arithmetic
refusals preserve existing rows, owner identity and region stats. Replacement
does not grow at the live threshold. Clear drops live entries; if one destructor
panics, already-removed entries stay removed and the surviving table remains
searchable. Ordinary Rust panic/unwind scope applies, including abort on a second
panic during unwinding. No strong rollback guarantee covers arbitrary user
Hash/Eq/Drop panics or physical OOM aborts.

Lookups expose shared or mutable values, never mutable stored keys. Disjoint
lookup returns references in query order and refuses two queries resolving to
the same occupied slot, including ZST values. Equal absent queries may both
return None. Iteration visits occupied entries in group/lane order and is exact
size and fused; rehash may change that order. Controls, iteration and values
borrow the region, so they cannot remain in use across mutation, growth, rewind
or reset. Even missing lookups, zero reserve and out-of-range control inspection
validate their owner first.

Watermarks protect storage lifetime rather than snapshotting mutations:

- Changes within capacity allocated before a watermark survive scratch rewind.
- A premark table cannot grow until the watermark is consumed. A growth refusal
  preserves rows/stats and returns incoming ownership; replacement remains valid.
- A table allocated after a covering watermark becomes stale after rewind, even
  after relocation. Reset and released allocations likewise cannot be revived
  through reused slots or bytes.
- Dropping or forgetting the move-only table handle does not release payloads.
  The region still finalizes remaining entries exactly once on release, rewind,
  reset or destruction. Failed by-value release returns the owner for recovery.

The self-checking example compares actual insert/replace/remove/lookups with an
independent ordered-map oracle and demonstrates disjoint mutation:

```sh
cargo run -p vesper-alloc --example flat_table
cargo test -p vesper-alloc --test flat_region --test flat_layout
```

This provides runtime region ownership, scalar control groups and inline entry
storage. Source FlatTable lowering, static region/model brands, language linear
exhaustion, Isa/SIMD execution, Lean refinement, zero transitive heap allocation,
whole-process RSS bounds and throughput/speedup guarantees remain outside this
profile. The example's reference map is an external oracle, not table storage.
