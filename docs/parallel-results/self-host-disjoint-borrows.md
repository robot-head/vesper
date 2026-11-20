# Disjoint borrows (SH3 lane 2, #1241)

Stacked on #1221 (element views, head `6d96c9c`); #1221 merged as
`005860b`, which this branch has merged. Branch
`claude/project-thread-l1llre`.

## Result

Every repro from the lane prompt either works or has a documented, sound
refusal. The evaluator, the JIT with the optimizer on and off (with leak
counters) and an executable agree on each runnable one
(`tests/disjoint_borrows.rs`).

| Repro | Before | Now |
|---|---|---|
| `xs[0].n = 1` (G1) | `E0300`, `E1522` | works, also nested `os[i].inner.k = v` and copy elements |
| `f(inout xs[0].slots)`, `xs[1].slots.push(1)`, `xs[1].n` (G1) | `E1522`, `E0202` | works |
| `swap(inout xs[0], inout xs[1])` (G2) | `E0301` | works (distinct literal indices are disjoint) |
| `f(inout xs[i], in xs[j])`, `i != j` not provable (G2) | `E0301` | works, with a run-time check: equal indices raise `IndexOutOfBounds` |
| `xs.swap(i, j)` (G2) | `E1525` unknown method | works on sequences and slices of any element type |
| `f(inout xs[i])` with a copy element (G3) | `E0300` | lent in place |
| `put_name(in st, inout st.err, x)` (SH2-14 gap 4) | `E0301` | still `E0301` (sound refusal), with a help that names the fix |
| raising `stamp(inout s[0])` with a live linear value (G4) | `E0402` | still `E0402` (sound refusal); `try_get_mut` is the total form |
| functions holding non-copy views inline (G9) | refused, "borrowed match" | inline |

## What changed

### The language (manuscript §13, flagged)

The *Element views* item now says a view lent to a call borrows the
element's place, and that any element may be lent `inout`. A new item,
*Disjoint element borrows*, says:

- `s[i]` lent to a call borrows the place of element `i`. It overlaps `s`
  and any element that may be the same one, and is disjoint from `s[j]`
  when `i` and `j` are distinct integer literals.
- Otherwise, when one of the borrows is `inout`, the indices are evaluated
  first and checked pairwise unequal; equal indices raise
  `Fail[RuntimeError]` `IndexOutOfBounds`, like an index past the end
  (the prompt left refuse-or-check to the lane: this lane checks).
- `s.swap(i, j)` exchanges two elements (no-op when `i == j < len`).
- A field path through an element, `s[i].f.g`, is a place: assignment,
  `in`/`inout` lending, receivers and reads of copy fields. It means the
  element view `match s.try_get_mut(i) { Some(x) => .., None => raise }`
  (`try_get` for `in` borrows and reads) around the expression, so it costs
  one bounds check and the element's address.
- A field of a lent receiver stays refused: `in st` reaches every field.

### Checker (src/check.rs, src/seq/check.rs)

- `Place::element(base, index)` adds an element segment to a place: `[n]`
  for an integer literal, `[$ixK_m]` for an index of a checked distinct
  group, `[_]` for anything else. `Place::overlaps` compares segments: two
  elements are disjoint only when both are literals that differ or two
  members of one group. Field-only and interval paths behave exactly as
  before (`is_element_segment`), so no other caller changes.
- `place_type` resolves element segments to the element type, so the loan
  checks in `place()` work on element places.
- Element call arguments (`boxed_element_place`), the place primitives
  (`swap`, `replace`) and lent elements in `seq_index` now borrow the
  element's place instead of the whole sequence, for copy elements too when
  lent `inout`.
- The `E0301` help for `f(in st, inout st.err)` explains that the `in`
  borrow reaches every field.

### The element-path rewrite (src/seq/element_paths.rs, new)

Like `?` on `Option` (`try_option`), the module is checked, the forms are
found with the first check's types, and the rewritten module is checked
again (`check_core`, and `check_isolated_item` for test functions). Every
backend therefore runs them as ordinary element-view matches; none needed a
change. The rewrite:

- wraps an assignment, call, method call or `match` whose target, explicit
  `in`/`inout` argument, receiver or scrutinee goes through `xs[i]` in one
  view per element, and a read `xs[i].f` of a non-copy element in an `in`
  view;
- binds an index that is not a literal or a name, and any value operand
  that mentions the sequence, before the view;
- binds the indices of two or more element arguments of one sequence (one
  `inout`) to a distinct group and checks them unequal;
- turns `xs.swap(i, j)` into the group check plus `swap(inout xs[a],
  inout xs[b])`.

The scrutinee takes the span of the source's `xs[i]` and the raise takes
that span less its last byte, so diagnostics about the view (`E0307` on an
`in` sequence, `E0301` from a second view, an undeclared
`Fail[RuntimeError]`) point at the source.

### Inliner (src/ir_inline.rs)

`view_scrutinees` finds the borrowed matches that do not depend on the
callee's parameter list: matched `try_get`s and matches on names they bind.
Those no longer refuse inlining. After splicing, the caller's own
`fip::borrowed_scrutinees` finds the matched `try_get` again, so drop
elaboration never frees a viewed element (the JIT's free counters in
`functions_with_element_views_inline` check it).

Nothing under src/verified_codegen/**, src/ir.rs, `ir_opt::optimize`,
lean/**, bootstrap/** or packages/vesper-compiler/** changed. Rust stays the
default; no Rust code was deleted.

## Tests

- `tests/disjoint_borrows.rs` (9): one per repro plus overlap refusals and
  inlining. Runnable ones go through the Place IR check, the evaluator, both
  JIT configurations with leak counters, and an executable.
- `tests/disjoint_borrows_pool.rs`: the census state pool written with
  paths (`pool_paths`) against `pool_views` and Rust `Vec<State>`; ignored
  ABBA and callgrind entries.
- `tests/element_borrows.rs`: the two "two views of one sequence overlap"
  refusals now use the same index (distinct indices are accepted).
- Also passing: arrays, borrow_paths, borrowed_box_match, bound_views,
  metatheory_places, native_empty_seq, native_inline,
  native_inline_inout_slice, native_seq_access, native_seq_drop,
  native_slice_headers, native_write_in_place, owning_collections,
  self_host_check, seq, seq_bulk, two_phase_arguments, two_phase_borrows,
  vector_slice_io.

## Stage 1 guard

None of the guarded paths changed. The bootstrap-stage1 workflow runs on
the branch; its result is recorded in the PR.

## Numbers

See "Numbers" in the PR body (release ABBA of `abba_against_vec_state`,
256 blocks x 8 rounds).

## Compiler gaps

**D1. Two paths through elements of one sequence in one call, one `inout`.**
`mv(inout xs[0].a, in xs[1].b)` nests two views; the outer `inout` view
lends `xs` exclusively, so it is `E0301`. Two `in` paths work. Workaround:
`mv(inout xs[0], in xs[1])` and project inside.

**D2. A path through an element needs `Fail[RuntimeError]`.** The view's
`None` arm raises even where bounds facts would prove `i < xs.len()`.

**D3. Operands evaluated inside the view.** Only operands that mention the
sequence are bound first; others run inside the view, so a call operand
that reads the sequence through another name (a function that takes it
`in`) is `E0301`.

**D4. Field of a lent receiver.** `f(in st, inout st.err)` would be sound
only with a callee summary of which fields an `in` parameter reaches. Not
done; refused with a help.

**D5. Raising views with a live linear value (G4).** Unchanged by design:
an element lent to a call can raise, and a live linear value may not cross
a raise. Use `match s.try_get_mut(i)`.

**D6. `xs.swap(i, j)` with a declared `swap` in scope.** The rewrite calls
`swap`, so a declared `swap` (for example `@vesper-std/core`'s copy-bound
one) shadows the primitive and needs a copy element type.

## What is left

D1 (disjoint paths by literal or checked index, like plain elements), D2
(drop the raise where bounds facts prove the index), and D4 (callee field
summaries).
