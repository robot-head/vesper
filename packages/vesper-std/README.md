# @vesper-std

The Vesper standard library, written in Vesper. It is the porting kit's
library half (port wave P0 lane 8, issue #897; survey in
`docs/maintenance/vesper-port-survey.md`). Every package declares
`capability_ceiling = ["Fail", "Alloc"]` and is plain Vesper over the builtin
`ChunkSeq[T]` and `Slice[T]`.

| Package | Directory | Contents |
|---|---|---|
| `@vesper-std/core` | `core/` | `min`, `max`, `clamp`, `swap`, `ordering_sign`, `reverse`, `then_cmp`; module `option` (`is_some`, `is_none`, `unwrap_or`, `unwrap_or_else`, `unwrap`, `map`, `map_or`, `and_then`, `or`, `filter`, `zip`, `ok_or`); module `result` (`is_ok`, `is_err`, `ok`, `err`, `unwrap_or`, `unwrap_or_else`, `map`, `map_err`, `and_then`, `or`) |
| `@vesper-std/collections` | `collections/` | module `btree`: `CacheBTree[K, V]` ordered map (`btree_new`, `insert`, `remove`, `get`, `contains_key`, `len`, `is_empty`, `clear`) with `btree_cursor` (`first`, `last`, `nth`, `lower_bound`, `cursor_first`, `entry`, `advance`, `range_into`); module `flat_table`: `FlatTable[K, V]` hash map; module `sets`: `OrderedSet[K]`, `FlatSet[K]`; module `radix_seq`: `RadixSeq[T]` with O(1) `freeze` snapshots |
| `@vesper-std/fmt` | `fmt/` | `write_u64`, `write_i64`, `write_hex_u64`, `write_u64_radix`, `write_bool`, `write_bytes`, `parse_u64`, `parse_i64`, `parse_u64_radix` (`ParseIntError`), `fnv1a64`; module `text`: `push_u64`, `push_i64`, `push_hex_u64`, `push_u64_radix`, `push_bool` (append to a `String`), `u64_to_string`, `i64_to_string`, `parse_u64_str`, `parse_i64_str`, `parse_u64_radix_str` (from a `Str`) |
| `@vesper-std/bytes` | `bytes/` | `read_u8`, `read_{u16,u32,u64,i64}_{le,be}` (an `Option`), `push_*` (append to a `ChunkSeq[u8]`), `put_*` (overwrite in a `Slice[u8]`, `false` when out of range) |

`tests/` is the `@vesper-std/tests` package: unit tests for every public
function, and Vesper ports of the Strata conformance trace generators.

## Using it

Add a path dependency and import by module:

```toml
[dependencies]
"@vesper-std/collections" = { version = "^0.1", path = "../packages/vesper-std/collections" }
```

```vesper
use @vesper-std/collections::btree::{CacheBTree, btree_new};

fn main() -> u64 {
    let mut m: CacheBTree[u64, u64] = btree_new[u64, u64]();
    m.insert(3, 30);
    match m.get(3) {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
```

Keys need the builtin `Ord` (`CacheBTree`, `OrderedSet`) or `Eq + Hash`
(`FlatTable`, `FlatSet`). The integers, `bool`, `()` and small tuples have
them; a struct opts in with empty impls such as `impl Ord for Point {}`.

## Design notes

- **Determinism.** No public result depends on hash values or capacity.
  `FlatTable` keeps entries densely in insertion order (removal moves the last
  entry into the hole) behind an open-addressing index, so `entry_at(i)` is a
  function of the operations performed. `Hash` is the builtin FNV-style fold.
- **`CacheBTree`.** A B+tree with 16 keys per node. Nodes live in flat arenas
  indexed by `u64`, so the tree holds no pointers. Removal does not merge
  nodes: leaves may become empty and every read skips them. Rebalancing on
  removal, the SIMD rank search and the `B` parameter of the manuscript are
  follow-up work.
- **`RadixSeq`.** The 32-way radix sequence of manuscript chapter 13 over
  `ChunkSeq`. `freeze` is O(1) and starts a new epoch; later edits copy only the
  path they touch, so snapshots never change. Nodes are reclaimed only when the
  sequence is dropped.
- **No ambient allocation effect yet.** `ChunkSeq` allocation is ambient
  (#886), so the packages' rows show only `Fail` where a function can fail.
- **Inside `inout` methods** code uses `try_get`/`try_set`, because an
  abortable effect may not cross a live `inout` loan (E0402).
- **Strings.** `fmt` writes ASCII into `ChunkSeq[u8]`; module `fmt::text`
  wraps the same formatters and parsers for `String` and `Str` (#896). Float
  formatting is follow-up work.
- **Eq, Ord, Hash** are still builtin traits of the compiler.

## Tests

`tests/vesper_std.rs` checks each package on its own, runs the unit tests in
the evaluator and natively, and requires the seven traces in
`crates/vesper-alloc/tests/fixtures/strata/` to regenerate byte for byte from
the Vesper `CacheBTree` and `RadixSeq`. Run a single suite by hand with:

```sh
vesper run --include-tests --no-opt --entry run_all packages/vesper-std/tests
vesper jit --include-tests --no-opt --entry trace_digest packages/vesper-std/tests
```

The self-hosted front end cannot parse `use` yet (#880), so these sources are
skipped by its corpus tests until it can.
