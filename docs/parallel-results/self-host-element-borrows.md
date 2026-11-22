# Element borrows and collections that own sequences (SH2 lane 1, #1173)

Base: origin/main c9f228b. Branch `claude/project-thread-69175v`.

## Result

An element of a `ChunkSeq[T]` or `Slice[T]` whose type is not copy can now be
borrowed in place, `in` or `inout`, without copying it out. That closes the
census gap C5 (`E1522 cannot copy 'State' out of a sequence`). On top of
it, `CacheBTree`, `FlatTable`, `Arena` and `IdxMap` hold values that are not
copy, which closes C6. Keys stay copy, and every existing API is kept.

The census state pool (blocks of dataflow states that own sorted rows) is
now an ordinary `ChunkSeq[State]`. Against Rust `Vec<State>` it takes 1.62x
Rust's wall time (1.45x callgrind Ir). The census workaround (one flat
append-only pool, loaded by copying) takes 1.58x (1.40x Ir). So views cost
about the same as the workaround while removing its hand-written pool and
keeping only live rows. See "Numbers".

Rust stays the default everywhere. No Rust code was deleted.

## What changed

### The language (manuscript §13, flagged in the PR)

A new list item, *Element views*, after the place primitives:

- `f(in s[i])` and `f(inout s[i])` lend an element to a call. `s[i].m()`
  lends it to a method. `swap(inout s[i], ..)` and `replace(inout s[i], v)`
  take it. Like `s[i]`, these raise `Fail[RuntimeError]`
  `IndexOutOfBounds` when `i` is out of range.
- `match s.try_get(i) { Some(x) => .., None => .. }` binds `x` as an `in`
  view of a non-copy element. `match s.try_get_mut(i)` binds an `inout` view
  of an element of any type. Both are total, so they work under a live
  `inout` loan.
- The arms of a view match are only `Some(x)`, `Some(_)`, `None` and `_`.
  `try_get_mut` is written only as a match scrutinee (`E1522`).
- A view borrows the whole sequence for the call or the match: shared for
  `in`, exclusive for `inout`. The existing call-argument and match-borrow
  rules check it (`E0300`, `E0301`). So the sequence cannot be pushed,
  popped or moved while a view is live, and two views of one sequence in one
  call are refused unless both are `in`.
- A view is never dropped. The element stays owned by its sequence.

The std catalog items for `@vesper-std/collections` and `@vesper-std/arena`
describe the owning-value forms.

### Checker (src/check.rs, src/seq/check.rs, src/seq.rs)

- `element_view_scrutinee` recognises `match s.try_get(i)` and
  `match s.try_get_mut(i)`, checks the arm shapes, and records the view's pass
  in `World::element_views` (keyed by the scrutinee span).
- Match borrows carry a pass. An `in` view conflicts only with writes; an
  `inout` view conflicts with any use of the sequence.
- `lend_boxed` and the place primitives (`swap`, `replace`) accept element
  places of non-copy elements. The whole sequence is the loaned place.
- The E1522 message now names the borrowing forms.
- `try_get_mut` resolves to `SeqOp::TryGet` on sequences and slices.

### Place IR check (src/ir_check.rs)

A view's `Seq` result is not owned, and its binding is a borrow with the
view's pass, so a linear or affine element is not counted as abandoned or
moved.

### Evaluator (src/runtime.rs)

Slots support element segments (`#i`), so `inout` arguments and view
bindings write back into the sequence's cell. `element_ref` finds the
defining `Get`/`SliceGet` of an argument, and `element_view_bind` binds a
view match arm.

### Native code (src/native.rs, src/seq/native.rs, src/native_aggregates.rs)

- `seq_element_address` gives a bounds-checked element address for call
  arguments. Elements that are aggregates or 8 bytes wide are passed by that
  address.
- Narrow elements (`u8`, `bool`, `u32`) go through a word of scratch, with a
  write-back after the call (`element_writebacks`). A narrow view binding is
  a variable homed at the element (`narrow_homes`).
- The fused `try_get` read takes the view flag, and an unfused view binds
  the element's address (`seq_bind_view`).
- View bindings are never split into registers (`split_locals`). A split
  binding lost writes with the optimizer on.

Nothing under src/verified_codegen/**, src/ir.rs, `ir_opt::optimize`,
lean/**, bootstrap/** or packages/vesper-compiler/** changed.

### Collections (packages/vesper-std/collections, packages/vesper-std/arena)

- `CacheBTree[K, V]`: values now live densely in `vals`, and each leaf entry
  holds a value position (`lvids`). `vkeys` keeps each value's key, so
  removal moves the last value into the hole and renames it in its leaf.
  `insert(k, sink v) -> Option[V]` returns the replaced value. `remove`
  moves the value out. `position(k)` names a value's slot (`u64::MAX` when
  absent). The cursor has `entry_position`. `get`, `entry`, `first`, `last`,
  `nth` and `range_into` keep their signatures under a copy bound.
- `FlatTable[K, V]`: `insert` replaces in place, `remove` moves the value
  out, and `position`/`key_at` are new. `get` and `entry_at` need copy.
- `Arena[T]`: `alloc(sink v)`, `len`, `contains`, `truncate` and `clear` work
  for any `T`. `get`, `get_or`, `set` and `at` need copy.
- `IdxMap[K, V]`: `idx_map_owning(sink dflt)` and
  `put(i, sink v) -> Option[V]` for values that are not copy. `put` cannot
  fill a gap with defaults, so writing past `len` hands `v` back.

A non-copy value is read with an element view:
`match t.vals.try_get(t.position(k)) { Some(v) => .., None => .. }`.

## Tests

- `tests/element_borrows.rs` (8 tests). It covers the census repro, call
  arguments and receivers, total match views, `swap`/`replace`, narrow
  elements, owning elements with leak counters, linear elements, and the
  refused cases (alias xor mutability). Each program runs in the evaluator,
  the JIT with the optimizer on and off, and an executable where `cc`
  links. The Place IR check runs on each, and the JIT must free every
  sequence exactly once (`teardown_frees`, `stray_frees`).
- `tests/owning_collections.rs`: `FlatTable`, `CacheBTree` (with a cursor
  walk), `IdxMap` and `Arena` of `State { slots: ChunkSeq[u32], n }`
  through inserts, replacements, removals and in-place updates. The results
  are checked against a Rust `BTreeMap`/`Vec` model, in the evaluator, both
  JIT configurations with leak counters, and an executable.
- `tests/owning_collections_pool.rs`: the census state pool in both shapes
  against Rust `Vec<State>`. The ignored tests are the ABBA benchmark and a
  callgrind entry.
- Existing suites still pass: vesper_std (the Strata traces regenerate byte
  for byte), std_arena, std_graph, std_intern, self_host_ast,
  self_host_check, and every sequence, borrow and drop suite (borrow_paths,
  borrowed_box_match, bound_views, dyn_box, dyn_fn, exit_drops,
  native_copy_within, native_drop_glue, native_empty_seq,
  native_fail_exit_drops, native_seq_access, native_seq_drop,
  native_seq_runtime, seq, seq_bulk, two_phase_arguments,
  two_phase_borrows).

## Stage 1 guard

The bootstrap-stage1 workflow passed on the branch: a fresh
`bootstrap/run.sh stage1 --reproduce --outcome` matches bootstrap/stage1.lock
([run 44796027644](https://github.com/robot-head/vesper/actions/runs/44796027644)
on 2756d8c). It runs again on the final head before merge. This lane does
not touch src/verified_codegen/**, src/ir.rs, `ir_opt::optimize`, lean/**,
bootstrap/** or packages/vesper-compiler/**.

## Numbers

The state pool has 256 blocks and runs 8 rounds. Block `b` takes the sorted
union of its predecessors `b - 1` and `(7b + 3) mod 256`, adds one row and
kills one (rows drawn from 0..512). Vesper is the optimized JIT
(`WordFunction`). Rust is a release build of the same algorithm over
`Vec<State { rows: Vec<u32> }>`. All three compute the same checksum.

Wall time is the median of 18 ABBA samples (`abba_against_vec_state`).
Ir is one run of each in `once_for_callgrind`, inclusive.

| Shape | Wall | vs Rust | Ir | vs Rust |
|---|---|---|---|---|
| Rust `Vec<State>` | 3.06 ms | 1.00x | 25.7 M | 1.00x |
| Vesper element views (`pool_views`, after) | 4.96 ms | 1.62x | 37.4 M | 1.45x |
| Vesper flat pool (`pool_flat`, before) | 4.83 ms | 1.58x | 35.9 M | 1.40x |

Before this lane the views shape did not compile (`E1522`). The only way to
write it was the flat pool.

- The views shape is about 4% more Ir than the flat pool. Both allocate a
  new row sequence per transfer. The views shape also frees the replaced
  one, as Rust does, while the flat pool never frees until the end, so its
  pool holds every stored version (8 rounds here, so 8x the live rows).
- Most of the remaining gap to Rust is the merge loop in `transfer`
  (`try_get` per element with a sentinel), not the views. Element views
  themselves add one bounds check and an address per access.
- Lines: the views shape of the pool is about 40 lines in this test. The
  census sample's `State`/`Pool` rows and pool are about 340 lines in
  `ir_flow.ves`. The test's flat pool is smaller than the census one only
  because its states have one row sequence, not four.

## Compiler gaps

**G1. No field paths through an element.** A view must be bound first:

```text
fn f(inout xs: ChunkSeq[State]) { xs[0].n = 1; }
// E1522, or a confusing E0202/E0206 for `f(inout xs[0].slots)`
```

Write `match xs.try_get_mut(0) { Some(s) => { s.n = 1; }, None => {} }`.

**G2. Two views of one sequence in one call.** The loan is the whole
sequence, so disjoint indices are still refused:

```text
swap(inout xs[0], inout xs[1]);      // E0301
f(inout xs[0], in xs[1]);            // E0301
```

There is no `xs.swap(i, j)` yet. Nested `in` views (two `try_get` matches)
are accepted.

**G3. Copy elements cannot be lent to a call.** `f(inout xs[i])` with a copy
element is still a value copy. Use `try_get_mut`, or `set` after the call.

**G4. Raising element views across a live linear value.** `stamp(inout
s[0])` raises `Fail` on a bad index, so with a live linear sequence in scope
it is `E0402`. Use the total `try_get_mut` form.

**G5. Closures have no parameter modes.** A collection cannot take a lookup
callback such as `with_ref(k, |in v| ..)`. That is why the collections expose
a position plus their `vals`/`items` sequence, and callers write the view
match themselves.

**G6. `IdxMap` of non-copy values cannot fill gaps.** There is no "default
for each new slot" without `Clone` or a constructor callback (G5), so
`put` only writes at an existing index or the next one.

**G7. One more indirection in `CacheBTree`.** Leaves hold value positions,
so copy users (and `OrderedSet`) now read `lvids` and then `vals`. The
Strata traces are unchanged, and the vesper_std tests did not slow
measurably, but it is not free.

**G8. Narrow writes inside inlined effectful callees.** A `u8`, `bool`
or `u32` element lent `inout` goes through a word of scratch. The write-back
runs before a called function's failure status is re-raised, so a raising
callee keeps its writes. But when the callee is specialized inline for its
handler (or is an effect operation), a raise jumps to the handler from
inside the body and skips the write-back. Wider elements are lent by address
and are not affected.

**G9. Functions with non-copy views are not inlined.** A matched `try_get`
counts as a borrowed scrutinee in drop elaboration (so a view of a `Box`
element is never freed), and the inliner refuses borrowed matches on
non-copy values.

## What is left

- `xs.swap(i, j)` and disjoint element views by index (G2), and field paths
  through elements (G1).
- Mode-carrying closure parameters (G5), then `with_ref`/`with_mut` on the
  collections and an `IdxMap` that fills gaps.
- Moving the census IR check sample onto `ChunkSeq[State]` and
  `CacheBTree[u32, State]`. That is the checker lanes' work (SH2 lanes 5-14).
