# Strata collection conformance traces

These traces are the executable specification of `CacheBTree[K, V]` and
`ChunkSeq[T]` (manuscript §13.5). Both collections are to be implemented in
Vesper source; a port passes when it replays every trace and produces exactly
the recorded result for every line. The Rust test
`tests/collections_conformance.rs` regenerates each file from its seed with a
standard-library oracle and fails if the checked-in bytes differ
(`VESPER_BLESS=1` rewrites them; review the diff before committing).

Format: one operation per line, `OP ARGS => RESULT`. `#` lines are comments.
All numbers are unsigned decimal `u64`. `none` is an absent value.

## `cache_btree_*.trace` (oracle `BTreeMap<u64, u64>`, canonical order)

| Line | Meaning |
| --- | --- |
| `insert K V => P` | insert or replace; `P` is the previous value or `none` |
| `remove K => P` | remove; `P` is the removed value or `none` |
| `get K => P` | lookup |
| `range A B N => K=V,...` | the first `N` entries with `A <= key < B`, ascending (empty result is an empty string) |
| `bounds => LEN FIRST LAST` | length, smallest and largest entry as `K=V` or `none` |
| `len => LEN` | final length |
| `hash => H` | FNV-1a 64 (hex) of all entries joined as `K=V` with `,`, ascending |

Shapes: `small_churn` (40-key space, mostly a single leaf), `dense_churn`
(700 keys, repeated splits and merges), `wide` (2^40 key space, height 3),
`ascending` and `descending` (sequential inserts followed by removals, which
stress right-edge and left-edge splits and borrows).

## `chunk_seq_*.trace` (oracle `Vec<u64>` plus named snapshots)

| Line | Meaning |
| --- | --- |
| `push V => LEN` | append to the transient sequence |
| `pop => V` | remove the last element (`none` when empty) |
| `set I V => ok/oob` | replace element `I` in place; `oob` when `I >= len` |
| `get I => V` | read element `I` (`none` when out of bounds) |
| `freeze S => LEN` | take an O(1) frozen snapshot named `S` of the current contents |
| `thaw S => LEN/missing` | replace the transient with a mutable copy-on-write view of `S` |
| `snap S => LEN HASH/missing` | length and FNV-1a 64 of `S`'s `{:?}`-formatted contents |
| `all => LEN HASH` | final transient contents |

The semantic core is persistence: edits after `freeze S` (including edits
after `thaw S`) never change what `snap S` reports. The deep trace grows past
32 x 32 elements, so it crosses the second radix level of a 32-way tree.
