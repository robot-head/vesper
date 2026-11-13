# Bounded two-column region tiles

`RegionTiled2<A, B, N>` stores two actual contiguous field columns per tile in
region chunks. Width `N` is 8, 16, 32 or 64; both fields are nonzero-sized
`Copy + 'static` types aligned at most 64 bytes. The complete tile, including
padding and its initialized-row header, is bounded by `HARD_MAX_TILE_BYTES`.
Each tile and field column begins at a 64-byte-aligned address. Column views
expose only initialized lanes, including a partially filled final tile.

Run `cargo run --locked -p vesper-alloc --example tiled_columns` for a real
73-row, five-tile scan and mutation checked against independently calculated
logical rows. This host runtime supplies two hot columns. It does not supply
source-language schema derivation, parsing, SIMD, cold arrays or a Lean proof.

```rust
use vesper_alloc::{Region, RegionConfig};
let mut region = Region::new(RegionConfig::bounded(8192)).unwrap();
let mut rows = region.tiled2_with_capacity::<u32, i64, 8>(16).unwrap();
region.tiled2_push(&mut rows, 7, -11).unwrap();
let (a, b) = region.tiled2_columns_mut(&rows, 0).unwrap().unwrap();
a[0] += 2;
b[0] *= 3;
assert_eq!(region.tiled2_get(&rows, 0), Ok(Some((9, -33))));
region.tiled2_release(rows).unwrap();
```

Capacity rounds to whole tiles and can grow within the region's finite byte,
alignment and allocation bounds. Constructors report unsupported shapes as
`TiledError`; reserve and insertion report the existing region errors. Failed
insertion returns its original field pair. A failed consuming release returns
its owner, allowing a foreign-region mistake to be retried in its home region.
Every checked method validates the owner, even an out-of-range lookup or
`reserve(0)`. Reset and covering rewind invalidate owners; reusing storage does
not revive an old serial.

Watermarks record allocation positions, not snapshots of initialized rows.
An owner allocated before a watermark survives rewind. Mutation of its old
rows, append into a partial tile, and publishing new live tiles within its
previously reserved capacity all survive scratch rewind. Growth beyond that
capacity is refused with `ProtectedByWatermark` while protected. Allocate
capacity before the checkpoint or commit the checkpoint before growing.
An owner allocated after the checkpoint becomes stale on covering rewind,
including when its buffer has relocated. Dropping or forgetting an owner
does not free its region storage; release, rewind, reset or region destruction
perform the relevant reclamation.

Owners move and cannot be cloned:

```compile_fail
use vesper_alloc::{Region, RegionConfig};
let mut region = Region::new(RegionConfig::bounded(8192)).unwrap();
let rows = region.tiled2_new::<u32, i64, 8>().unwrap();
let duplicate = rows.clone();
```

Field values must be `Copy`:

```compile_fail
use vesper_alloc::{Region, RegionConfig};
let mut region = Region::new(RegionConfig::bounded(8192)).unwrap();
let rows = region.tiled2_new::<String, u32, 8>().unwrap();
```

Borrowed fields cannot outlive their referents:

```compile_fail
use vesper_alloc::{Region, RegionConfig};
let mut region = Region::new(RegionConfig::bounded(8192)).unwrap();
let local = String::from("temporary");
let mut rows = region.tiled2_new::<&str, u32, 8>().unwrap();
region.tiled2_push(&mut rows, local.as_str(), 1).unwrap();
```

Column views borrow the region through their last use. Holding a view across
insertion or rewind is rejected, even when reserved capacity would suffice:

```compile_fail
use vesper_alloc::{Region, RegionConfig};
let mut region = Region::new(RegionConfig::bounded(8192)).unwrap();
let mut rows = region.tiled2_with_capacity::<u32, i64, 8>(16).unwrap();
region.tiled2_push(&mut rows, 1, 2).unwrap();
let (first, _) = region.tiled2_columns(&rows, 0).unwrap().unwrap();
region.tiled2_push(&mut rows, 3, 4).unwrap();
assert_eq!(first[0], 1);
```

```compile_fail
use vesper_alloc::{Region, RegionConfig};
let mut region = Region::new(RegionConfig::bounded(8192)).unwrap();
let mut rows = region.tiled2_with_capacity::<u32, i64, 8>(8).unwrap();
region.tiled2_push(&mut rows, 1, 2).unwrap();
let mark = region.checkpoint().unwrap();
let (first, second) = region.tiled2_columns_mut(&rows, 0).unwrap().unwrap();
region.rewind(mark).unwrap();
first[0] = second[0] as u32;
```

The owner type cannot be weakened to accept shorter borrowed fields. Its
`'static` field bounds already reject such a target type; the underlying
collection also preserves invariant payload identity:

```compile_fail
use vesper_alloc::RegionTiled2;
fn shorten<'a>(owner: RegionTiled2<&'static str, u32, 8>, local: &'a str) {
    fn accept<'b>(_: RegionTiled2<&'b str, u32, 8>, _: &'b str) {}
    accept(owner, local);
}
```

The lifecycle acceptance suite uses a counting/failing `ChunkBacking` that
records each actual pointer and exact layout, checks deallocation against the
live allocation map, and requires every successful allocation to be freed once.
These are native Rust runtime checks; they do not establish performance,
source-language linear exhaustion or external platform behavior.
