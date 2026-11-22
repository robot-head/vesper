# Self-hosting SH5 lane 10: one shared world and word answers across hybrid swaps

> Claim: #1333. Prompt:
> [`docs/agent-prompts/self-host-sh5/10-swap-crossing.md`](../agent-prompts/self-host-sh5/10-swap-crossing.md).
> Base: #1307's head `b5ee0be` (SH4 lane 2 part 1), `origin/main` at
> `bbda89e`. The [manuscript](../manuscript/README.md) is the sole
> specification; this lane needs no change to it.

Rust stays the default build. Nothing in the default compiler changed:
every edit is in `crates/vesper-hybrid` plus one field in the shared
encoder (`tests/common/check_words.rs`), and no Rust code was deleted.

## Summary

SH4 lane 2 left four gaps in its report. This lane closes the first two
and most of the fourth:

1. **One decoded world per compilation, shared by both checker swaps.** A
   new hybrid module, `modules/checks` (library `hybrid_checks`), links
   `@vesper/check-layout` and `@vesper/ir-check` into ONE Vesper library.
   One `Checks` value per compilation holds the one decoded `Ctx`, and both
   `Layout::of` and `ir_check::validate` read it. Before, the layout and the
   IR check each decoded their own copy of the world.
2. **Layout answers cross as words, not text.** Answer format 2 names the
   input's own names and types by index. Only a name or type the layout
   made itself (a tuple field's digit name, a substituted field type)
   crosses in full. Rust reads the answer back from the compilation's
   `WorldLog` tables.
3. **Per-call overhead.** A repeated layout call with nothing new to read
   used to cost 0.7-1.5 µs in Vesper. It now costs 0.19-0.28 µs, which is
   less than the Rust layout itself (0.32-0.45 µs). Its decode halved too.
   The call now allocates nothing on either side once its buffers have
   grown: the arena is kept between calls, and Rust reads the answer in
   place.
4. **The 10% input target is not met.** The layout swap's input is 0.30x
   of the Rust pass, down from 0.37-0.42x. What is left is almost all the
   memo lookup of SH4 lane 2's answer reuse (98% of calls are reused), not
   the crossing. See "What is left".

`VESPER_HYBRID_SHARED=0` turns the shared world off. That is the "before"
side of every measurement here: the same binary on SH4 lane 2's paths.

## What changed

- **`crates/vesper-hybrid/modules/checks`** (new Vesper package
  `@vesper/hybrid-checks`, capability ceiling `["Fail", "Alloc"]`):
  - `Checks` holds the shared `Ctx`, the layout arena (kept between
    calls), the inverse name and type maps (symbol or type id to input
    index, extended incrementally after each part), the answer's own
    name and type tables (stamped per answer, so nothing is cleared), and
    the `out` buffer.
  - Entry points: `checks_new`, `checks_extend` (add a part),
    `checks_validate` (an IR-check part, the same stream as
    `ir_check_part`), and `checks_layout_of` (a layout part; the answer
    goes into `out`, and the call returns its word count).
  - The format-2 writer (`put_ids` in the module's comment) is the binary
    answer encoder. Format 1 stays in `@vesper/check-layout` for the
    per-call paths and the root tests.
- **`crates/vesper-hybrid/src/crossing.rs`** (new): `Shared::open` sends
  every ADT (in name order) and every effect once, then `decode_layout`
  reads format 2 straight out of the value's `out` sequence (a `SeqRef`,
  so there is no copy of the answer).
  - Why every ADT up front: the IR check's constructor lookup takes the
    first ADT in name order, and the layout's lazy parts would otherwise
    have added ADTs to the shared table out of order.
  - A world with an ADT the encoder refuses opens no shared input. Each
    swap then keeps its own input, exactly as before.
- **`crates/vesper-hybrid/src/hybrid.rs`**: the compilation slot gets a
  `SharedState`. Both swaps use the shared input when it is open, and fall
  back to their SH4 lane 2 inputs when it is off or refused, and to Rust
  outside a scope. SH4 lane 2's answer reuse is unchanged.
  `VESPER_HYBRID_STATS` cost lines end with a new `N shared` count (calls
  that crossed into the shared world).
- **`tests/common/check_words.rs`**: `Enc` keeps `type_list`, every
  interned type by index, for reading format-2 answers. This file belongs
  to #1176, #1232 and #1307; this is a one-field addition.
- **build.rs and BUILD.bazel**: the new module joins `MODULES`. Both
  divergent builds also break it: the `divergent` build adds one byte to
  every shared layout answer's size, and `divergent-ir-check` gives every
  shared IR check the made-up E0410. Bazel gets the module's package
  dependencies, its divergent object, and `vesper-hybrid` on `PATH` for
  the new test.
- **Tests**:
  - `crates/vesper-hybrid/tests/swap_crossing.rs`: the hybrid builds
    objects with both swaps on the shared world, with 0 differing answers,
    and the same with `VESPER_HYBRID_SHARED=0`.
  - `crates/vesper-hybrid/tests/swap_crossing_words.rs`: every example and
    bootstrap-corpus world is checked through one `Checks`, in the order
    IR check, then layouts (every ADT, every field type, a tuple, a
    failing layout, each twice), then the IR check again. Every answer is
    compared with Rust, and the test asserts that dropping the values
    frees all their allocations. Its ignored `repeated_layout_call_costs`
    is the per-call timing below.

No export ABI change was needed: one library means one runtime, so the
`Ctx` is never lent across libraries.

## Results

### Differences

| Check | Differences |
|---|--:|
| Hybrid over the corpus with `VESPER_HYBRID_STATS=time` (159 programs, 46,421 layouts, 477 IR checks), shared on | 0 |
| The same with `VESPER_HYBRID_SHARED=0` | 0 |
| `swap_crossing_words`: 18 worlds, 264 layouts, 36 IR checks against Rust | 0 |
| Leaks: `Checks` dropped after every world | 0 live allocations left |
| Divergent `hybrid_checks` objects (both edits) | build |

### Lane 2's table, re-measured

Measured inside the hybrid over the same 159 programs as SH4 lane 2
(`bootstrap/corpus`, `examples`, `tests`), release build, two runs each
(ranges), with `VESPER_HYBRID_STATS=time`:

| Swap | | Input | Vesper | Answer decode | Whole swap |
|---|---|--:|--:|--:|--:|
| check-layout | lane 2 (shared off) | 0.37-0.42x | 0.23-0.25x | 0.06-0.07x | 0.67-0.74x |
| check-layout | **this lane** | **0.30-0.31x** | **0.12-0.14x** | **0.04-0.05x** | **0.47-0.50x** |
| ir-check | lane 2 (shared off) | 0.42-0.47x | 0.67-0.70x | 0.00x | 1.09-1.17x |
| ir-check | this lane | 0.48-0.58x | 0.72-0.84x | 0.00x | 1.20-1.42x |

All figures are ratios to the Rust pass, which took 27-33 ms for layout
and 85-105 ms for the IR check. 784 layout calls crossed into Vesper and
45,575 were reused answers. 159 of the 477 IR checks ran in a scope.

How to read the IR-check row: whichever swap runs first opens the shared
world (it encodes the whole world and Vesper decodes it), and in object
builds that is always a layout. So with sharing on, the world's one-time
cost lands in the layout's input column. With sharing off, the IR check
paid it in its own Vesper column. The IR-check rows differ by about as much
as two runs of the same build do on this host.

Instruction counts are steadier than wall time. Callgrind on
`tests/fixtures/strings/utf8.ves` (10,828 layout calls, 15 crossings):

| | Lane 2 | This lane |
|---|--:|--:|
| Vesper layout calls (15) | 234,407 Ir | 77,652 + 16,635 (world opened once) |
| Answer decode (15) | 80,252 Ir | 37,780 Ir |
| Whole layout swap | 12.99 M Ir | 12.72 M Ir |
| Whole IR-check swap | 36.86 M Ir | 36.74 M Ir |

### Where the 1.4 µs went

`repeated_layout_call_costs` lays out the same type 20,000 times, with
nothing new to read (release build, `examples/transaction.ves`):

| Type | Rust layout | Lane 2 Vesper | Lane 2 decode | Shared Vesper | Shared decode |
|---|--:|--:|--:|--:|--:|
| `ArithOverflow` | 323 ns | 685 ns | 186 ns | 282 ns | 103 ns |
| `RuntimeError` | 391 ns | 1,490 ns | 328 ns | 189 ns | 151 ns |
| `WriteTxn` | 446 ns | 1,420 ns | 377 ns | 271 ns | 163 ns |

Lane 2's call spent its time on:

- a fresh `Layouts` arena: 6 sequence allocations and their drops;
- a fresh `Out`: 7 more allocations, plus a dedup array per name and per
  type, sized to the largest symbol and type id;
- every name written out as text, and every type re-written as a tree;
- `Out::finish` copying the sections into the result;
- the binding's `seq_take` copying the result into a Rust `Vec` and
  freeing it.

Now the arena and every buffer live in `Checks`. Names and types that
Rust already has cost one word each, and the answer is read in place. The
crossing is no longer the overhead. On a reused call (98% of calls), the
remaining cost is in the hybrid's Rust lookup: per call, about 990 Ir.
Callgrind on utf8 splits it like this:

- the answer clone: ~340 Ir;
- two thread-local `swap_input` slot borrows: ~165 Ir;
- the stats timers: ~100 Ir;
- `type_hash`: ~92 Ir;
- the bucket's `Type` comparison: ~75 Ir.

## Stage 1 guard

`src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`,
`bootstrap/**` and `packages/vesper-compiler/**` are untouched. Nothing
under `src/` changed at all, so the default compiler and its objects are
byte-identical to the base. The verified backend never reaches a hybrid
swap. The bootstrap-stage1 workflow runs on this branch once #1307 has
merged and main is merged in (owner CI rule); its run is linked from the PR.

## Compiler gaps

1. **Constants cannot be imported across packages here.** `modules/checks`
   repeats check-data's `K_*` and check-layout's `L_*` constants, as
   check-layout already does. Repro: `use @vesper/check-layout::{L_ENUM}`
   in `modules/checks/src/lib.ves`.
2. **No in-place slice view of a `u64` sequence in the binding.**
   `SeqRef<u64>` reads element by element (`get(i)`, a bounds check and an
   unaligned read per word), so format-2 decoding cannot use
   `&[u64]`-based readers such as `check_words::Words`. Repro: the
   `Reader` in `crates/vesper-hybrid/src/crossing.rs`. An `as_slice()`
   for 8-byte elements in `vesper-port-abi` would allow it.
3. **Hybrid libraries cannot share a runtime.** This lane avoided the
   problem by linking both swaps into one library. Any future pair of
   swaps that must share a value has to do the same, because a handle of
   one library still cannot be lent to another (SH4 lane 2's gap 1).

## What is left

- **The 10% input target.** The layout input is 0.30x of the Rust pass,
  and nearly all of it is the reuse lookup listed above. Two parts of it
  are outside this lane:
  - `swap_input::with`'s two thread-local borrows. A single-borrow fast
    path would need a change to `src/swap_input.rs` (SH4 lane 2's).
  - A key that is cheaper than hashing and comparing the whole `Type`.
    That needs a pre-hashed or interned type at the `Layout::of` call
    sites in `src/layout.rs` callers.
- **IR checks outside a scope.** 318 of the 477 IR checks run outside a
  native compilation scope, so each one still encodes its whole world. On
  utf8 those two calls cost twice as much as the scoped one. That is scope
  placement in `src/` (SH4 lane 2's open call sites).
- **IR-check answers** are already words. Only diagnostic codes,
  messages and help text cross as bytes, and a clean program's answer is
  one word, so nothing is left to gain there.
- **The old modules** `check_layout` and `ir_check` stay. They serve the
  per-call paths, the root tests and the `VESPER_HYBRID_SHARED=0` side of
  the measurement. Folding them into `modules/checks` is a follow-up once
  nothing reads their bindings.
