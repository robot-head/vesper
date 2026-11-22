# Rust-exit gap wave 1, lane 6: slices as values

Claim #1503. Prompt:
[06-slices-as-values.md](../agent-prompts/rust-exit-gaps-1/06-slices-as-values.md).
Base `66255e17` (origin/main when the lane started).

## Result

| Gap (Quarry/Flux and small-libs reports) | Before | After |
|---|---|---|
| `let s = seq.as_slice();` of a `let mut` local, an `inout` or `sink` parameter, or a field of one | E1521 | accepted; the view freezes the overlapping places until its block ends (E1531 on a write, an `inout` loan or a move) |
| Slice parameters block inlining | already fixed by #1109 (p3b-gaps lane 4) | no slice-related refusal left in Semver, Loom calculus, audit, solver or Quarry; a new test covers bound-view arguments |
| `a[lo..hi].cmp(in b[..])` on bytes | 16-byte steps, then the differing step rescanned byte by byte; under 16 bytes a byte loop | the first differing lane comes from the step's lane mask (`ctz`); under 16 common bytes, one over-reading masked step in hosted code |

Micro-benchmark (`vesper build`, x86-64, callgrind): 1M compares of two
47-byte views that differ in the last byte cost 186 instructions each
before and 99 after (loop overhead of 16 subtracted). `eq` already stopped
at the first differing step and is unchanged.

**No reporting port moves measurably**, because none of them has the
compare or the binding on its hot path any more (numbers below). The
compiler gaps are closed, so ports can drop the workarounds as they are
next edited, but this lane does not flip a port.

## The manuscript change (flagged for the owner)

§13 "Bound views" in `docs/manuscript/content.typ`. Before: a bound view
was allowed only of a place that cannot change (an `in` parameter, an
immutable view binding, an immutable owned binding); a view of a `let mut`
local, an `inout` or a `sink` parameter "stays refused". Now:

> an immutable `let b = v;` may bind a string or slice view `v` of a place:
> an `in` parameter, an immutable binding that is itself a `Str` or
> `Slice`, or a place of an owned or `inout` binding (a `let` or `let mut`
> local, or an `inout` or `sink` parameter ...). The view borrows its source
> `in` until the end of the view's block: while it is in scope, no place
> that overlaps the source may be written, lent `inout` or moved (`E1531`
> ...), so the bytes the view reads stay put, and disjoint places
> (`l.n = 3` beside a view of `l.words`) stay usable. ... a mutable binding
> of a view (`let mut b = v;`) stays refused.

After the owner's review (2026-11-22) the paragraph also names `inout`
`Slice` parameters as sources, drops `Slice` from the owned list (no local or
aggregate can hold one), says that a `for` over a view borrows only for the
loop, and that rebinding the source's name in the view's block (including
`let xs = xs.as_slice();`) is E1531, as the checker now enforces.

The loan is lexical (to the end of the block), like the existing move rule
for views of immutable owned bindings, not liveness-based. A `for` over a
view of a changing place keeps its own loop loan (E0301), as before.

## What changed

- `src/strings/check.rs` `bind_view` takes `changeable`: a `let` (not a
  `for`) may bind a view of a `Sink` or `Inout` binding. `record_view_source`
  records those sources too, and `check_view_sources` reports a write or
  `inout` loan as well as a move ("cannot write or lend 'inout' 'xs' while
  the view 's' of it is in scope").
- `src/check.rs` `place`: the view-source check runs for `inout` borrows
  (assignments, mutating methods, `inout` arguments, `try_get_mut`) as well
  as for moves.
- `src/loops/for_sequences.rs`: passes `changeable = false`.
- `src/seq/native_bytes.rs` `bytes_cmp`: lane-mask exit and the short step.
- Tests: new `tests/slice_values.rs` (bindings across every backend and an
  executable, the freeze rules, inlining through bound views, and `cmp`/`eq`
  of every length 0..=40 against lengths across the 16-byte boundaries with
  a difference at every position). `tests/bound_views.rs`, `seq.rs`,
  `strings.rs`, `string_views.rs`, `str_literals.rs` and
  `native_seq_drop.rs` had cases that expected the old refusal; each now
  checks the new freeze (E1531) or the still-refused `let mut` view.

The Vesper port of the checker (`packages/vesper-check-regions-seqs`) does
not yet hold the AST-walking seq and string checker methods (its header:
"a later PR of #1181"), so there is no Vesper hunk to keep in step. The
inliner and `bytes_cmp` have no Vesper port either.

## Ports, re-measured

Callgrind instruction counts, release builds (`--features vesper-port`
where the port is opt-in). "main" is the port built by the compiler at the
base, "new compiler" the same port source built by this branch's compiler.

| Workload | Rust | Port, main | Port, new compiler | Port, workaround removed |
|---|--:|--:|--:|--:|
| Quarry planner, `planner_abba window 20` | 19.47 M | 25.64 M (1.32x) | 25.64 M | 26.07 M (`same`/`order`/`is_lit` as slice `eq`/`cmp`) |
| Loom calculus, `abba window 2` (64 packages) | 64.99 M | 46.88 M (0.72x) | 46.85 M | 46.86 M (`occ_ord` as `pool_ord`, no big-endian words) |
| Loom audit, `audit_abba window 5` (8 packages) | 12.53 M | 13.92 M (1.11x) | 13.90 M | not tried |

- Quarry compares identifiers of a few bytes that usually differ in the
  first byte or the length. The byte loop already stops after one step
  there, so a vector step is not cheaper, and the longer `same` pushed the
  single-site callee `check::find_column` over the inliner's size budget
  (`Size: 1`), which cost 0.43 M. The rewrite is not committed.
- Loom calculus and audit: the string sort is a small share of either
  workload (audit's time is in the Rust-side encoder and BLAKE3). Removing
  the big-endian word table is instruction-neutral; the patch is in the
  project files (`slices-as-values/calculus-occ-ord-rewrite.patch`) for
  SH6 lane 12 (#1407), which owns the port, if it wants the simpler code.
- Because instruction counts did not move, I did not run wall-clock ABBA;
  the census rows (`scripts/rust-census.py`) are unchanged.

## Stage 1 guard

No edit to `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/stage1*` or `packages/vesper-compiler/**`. The
`bytes_cmp` change is on the optimized native path only (the Stage 1
corpus is built unoptimized), and the checker change only accepts more
programs. See the PR for the `bootstrap/run.sh stage1 --reproduce --outcome`
result.

## New gaps found

1. **"callee re-binds a borrowed parameter" blocks inlining** in Quarry
   (`compare_vals` 6 sites, `check_table`, `validate_rows`, `bounds_of`,
   `column_of`, `index_column`) and Loom (`node_sets`, `closure`, `locked`,
   `collect_caps`). Not slice-related: `ir_inline::site_bindings` refuses a
   callee whose patterns bind the name of a parameter it replaces with the
   caller's place. Belongs to the inliner-policy lane (#1499).
2. **Short byte compares**: for strings under 16 bytes that differ early,
   the masked vector step costs more than the byte loop it replaces in
   `cmp`; `eq` has the same shape. A first-byte check before the vector step
   would cover Quarry's case; not done here because no port needs it.
