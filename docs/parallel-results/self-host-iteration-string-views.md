# Self-hosting SH3 lane 4: `for` over sequences, and bound string views

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1243. Base: `main` at `c8ac321`. Lane prompt:
[`docs/agent-prompts/self-host-sh3/04-iteration-string-views.md`](../agent-prompts/self-host-sh3/04-iteration-string-views.md).

## What changed

**`for` over arrays, sequences and views** (`src/loops/for_sequences.rs`,
hooks in `src/loops/check.rs` and `src/check.rs`). `for p in e { body }`
now iterates, in index order:

- a fixed array `[T; N]` (a place, or a literal such as `[1, 2, 3]`, whose
  unsuffixed elements take the kind of the loop variable's uses);
- an array literal of static text, `for s in ["a", "b"]`, binding each `Str`;
- a `ChunkSeq[T]` or `Slice[T]` place (a local, a field path such as
  `self.items`, a slice parameter or a bound view);
- a view expression: `s.as_bytes()`, `xs.as_slice()`, `xs[a..b]`, `v[1..v.len()]`;
- a temporary (`for x in make()`), evaluated once into a hidden binding the
  loop owns and drops.

Elements must be `copy` for now (`E0213` otherwise; non-copy elements need
#1221's element views, a follow-up). The length is read once and an
interval's bounds are evaluated once. The body may read the iterated place but
not write it, lend it `inout` or move it (`E0301`, "conflicts with the 'for'
loop that iterates 'xs'"), so every iteration reads in bounds.

How it works: the checker types the loop and records a rewrite plan by the
iterable's span; the module is then rewritten to an index loop over a range
and checked again, exactly like `?` on `Option` (`try_option.rs`). Every
backend runs an ordinary range loop:

```text
for x in xs { b }          =>  for $i in 0..xs.len() { match xs.try_get($i) { Option::Some($x) => { let x = $x; b; }, _ => {} } }
for v in a { b }           =>  for $i in 0..a.len() { let v = a[$i]; b; }        // total: the range proves the index
for s in ["a", "b"] { b }  =>  for $i in 0..2 { let s = match $i { 0 => "a", _ => "b" }; b; }
```

Range loops lower exactly as before: the range path only gains the loan for
hidden `$for` indices, so no existing program changes IR.

**Bound views of immutable owned locals** (`src/strings/check.rs`
`bind_view`, `record_view_source`, `check_view_sources`). `let v =
s.as_bytes();` (also `as_str()`, `s[a..b]`, `as_slice()`, field paths) now
binds when `s` is a `let` without `mut` holding a `String`, a `ChunkSeq` or an
aggregate of them. Such a binding already cannot be written (`E0313`); while
the view is in scope (to the end of its block) the source cannot be moved
either (`E1531`, "cannot move 's' while the view 'v' of it is in scope").
Views of `let mut` locals and `inout`/`sink` parameters stay refused.

**Library** (`packages/vesper-std`):

- `@vesper-std/bytes::views` (new): `find`, `rfind`, `starts_with`,
  `ends_with`, `split_once`, `rsplit_once`, `strip_prefix`, `strip_suffix`,
  `last_segment_start`. 24 unit tests.
- `@vesper-std/intern`: `Interner::lookup_view(in b: Slice[u8])`,
  `Interner::lookup_span(sym, from, to)` (a sub-range of a symbol's own text,
  read in place), and `hash_bytes`. `lookup` and `intern` now share the
  byte-view probe. 3 new unit tests (18 total).

**Manuscript** (flagged in the PR): `ForExpr ::= 'for' Pat 'in' (RangeExpr |
Expr) Block`; the loops paragraph of §2.2 gains the `for`-over-sequences
rule (replacing "waits for the port-readiness Slice lane"); §13 bound views
now cover immutable owned bindings and describe the offset-returning split
helpers and `lookup_view`; the sugar note says `for` covers arrays and
sequences.

## Decisions

1. **The split helpers return offsets, not views.** `Option[Str]` is refused
   by §13 (a `Str` result is static text only), so `rsplit_once` and friends
   cannot return views without a lifetime-carrying view result, which is a
   larger language change. They return the offsets; the caller binds
   `s[at..s.len()]`, which is now legal for immutable locals, `in`
   parameters and bound views. Nothing is copied.
2. **Rewrite, not new IR.** `for` over a sequence is checked twice (typed,
   then as its index loop) instead of getting a new Place IR form, so the
   evaluator, JIT, objects and every refusal path are unchanged, and the
   Stage 1 guard files are untouched.
3. **A loop loan, not a NLL borrow.** The iterated place is lent to the
   whole body; the bound view lives to the end of its block. Both are
   conservative (a move of the source in the tail of the view's own block is
   refused even after the view's last use).

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed (see the
PR diff). The verified backend refuses loops already, and range loops lower
unchanged. The bootstrap-stage1 workflow result for the branch is linked in
the PR.

## Tests

- `tests/for_sequences.rs`: string-literal arrays with `break`/`continue`;
  nested loops over one `ChunkSeq`, slice parameters, intervals of a `let mut`
  sequence, `as_slice()`, a temporary; arrays (place and literal), a field
  path and tuple patterns in a method; string bytes; rejections (push,
  assign, move in the body, view of a pushed sequence, non-copy elements, a
  non-iterable, refutable patterns over arrays and sequences).
- `tests/string_views.rs`: views of immutable `String`/`ChunkSeq` locals; the
  last `::` segment of a name as a view compared without a copy; the source
  moved after the view's block; rejections (move while the view is in scope,
  `let mut` source, write through `E0313`, returning a view).
- Every differential runs the evaluator, the JIT with the optimizer on and
  off (no stray or teardown frees), and an executable.
- Existing suites rerun: loops, bound_views, strings, str_literals,
  literal_typing, arrays, std_intern, try_option, byte_loop_facts,
  string_copy, and the std source unit tests in test_runner_cli.

## Numbers

Local rewrites of the two helpers (patches in the project files,
`sh3-iteration-string-views/`; not committed, they belong to the owning
lanes):

- `@vesper/check-loops-atomics` `strip` (#1223): the `Atomic::` rest is looked
  up as `names.lookup(in names.text[a + p.len()..end])` instead of copying it
  into a `ChunkSeq[u8]`; `syms_new`'s 24 `push(intern(..))` lines become three
  `for n in ["load", ...]` loops. `atomics_rules_match_rust`: **0 of 535
  worlds differ** with the rewrite.
- `@vesper/check-regions-seqs` `last_segment` (#1226): the segment is looked
  up in place and copied (with `for x in names.text[cut..b]`) only to intern
  a name the table has not seen. The package checks.

Micro benchmark of `strip` (the copying form against the view form, same
table and lookup, 300,000 calls, optimized executable, callgrind):

| | Ir | `malloc` calls | `vesper_seq_grow` calls |
|---|--:|--:|--:|
| copy into `ChunkSeq[u8]` (before) | 221,763,489 | 300,004 | 300,004 |
| view (after) | 83,063,337 | 4 | 4 |

The view form is 0.37x the instructions and allocates nothing per call (the
4 calls are table setup).

## Compiler gaps

1. **Non-copy elements.** `for s in names` over `ChunkSeq[String]` is
   `E0213`. Needs #1221's element views to bind `in` views.
   `fn f(in q: ChunkSeq[String]) { for s in q { } }`
2. **Views inside results.** `fn tail(in s: Str) -> Option[Str]` is `E1530`,
   hence the offset-returning helpers.
3. **Text views as receivers.** A `Str`/`as_bytes()` view of a `let mut`
   string cannot be iterated or used as a method receiver (`E1531`); only
   slice views of sequences can.
   `fn f() { let mut s = String::new(); for b in s.as_bytes() { } }`
4. **Interning a view of the interner's own text** still needs a copy on a
   miss: `names.intern(in names.text[a..b])` lends `names` both `inout` and
   `in`. `lookup` first makes the common hit copy-free.
5. **Diagnostics on synthesized code.** An error that only the rewritten
   form can produce lands on a synthetic span past the source (the renderer
   falls back to a plain line), as for `?` on `Option`.

## Cross-lane impact

PR #1223 (`@vesper/check-loops-atomics`) mirrors the old syntactic rule
"'for' iterates an integer range 'a..b' or 'a..=b'". Merged with this
change, its `control_rules_match_rust` reports 56 of 903 worlds differing
(the `#m2` mutants, which turn `for i in a..b` into `for i in a`): Rust now
reports a typed message the harness filters out. The fix belongs to that
lane: the non-range check is now a typed rule, so the Vesper control pass
should drop it (and `CONTROL_MESSAGES` its message).

## What is left

- Non-copy elements through element views (after #1221).
- `for` over a `String`'s chars, and over `TiledSeq`/other collections.
- Lifetime-carrying view results (`-> Option[Str]` borrowing an `in`
  parameter), which would let the split helpers return views.
