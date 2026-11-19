# P3b gaps lane 9: two-phase `inout` arguments and bound views

Claim #1094. Base: `origin/main` at 9305b24 (plan PR #1098).

## What changed

1. **Two-phase argument loans** (`src/check.rs`, `src/ir_check.rs`). #1004
   reserved only the receiver's `inout` loan. Now every `inout` argument
   loan of a call is reserved while the other arguments are evaluated and
   activated when the callee is entered, with the same rules:
   - reads of the place in a nested call (`f(inout c, g(in c))`,
     `put(inout s, s.len())`) are allowed and see the value from before the
     call;
   - a scalar (integer, float, `bool`, `Unit`) read straight from the place
     (`f(inout s, s.n)`) is copied before the call. The checker allows it,
     and the Place IR check no longer refuses the scalar's `in` loan when the
     `inout` loan activates;
   - writes, moves and `inout` loans of an overlapping place in the
     arguments are still refused (`E0301`, `E0410`, `E0305`). That includes
     the nested `w8(inout out, w8(inout out, 0, 1), 2)` from the Warp report,
     whose inner call writes the reserved place;
   - a loan that is still live when the call runs may not overlap the
     activated place: `f(inout p, in p)`, and a copy aggregate passed straight
     from the place, `f(inout q, q.p)`. The second case is refused on
     purpose. Native code lends copy aggregates by address, and a probe
     accepted without this rule returned 100059 in the JIT but 3059 in the
     evaluator.
   The Place IR check already reserved every `inout` argument that a call
   consumes (`reservable`, #1004). Its only change is the scalar case at
   activation.
2. **Bound views** (`src/strings/check.rs`, plus a 3-line call in the
   checker's `let` arm). `let b = s.as_bytes();` and other string or slice
   views (`s.as_str()`, `t[a..b]`, `q.as_slice()`) may initialize an
   immutable `let` when the source cannot change while the function runs.
   That means an `in` parameter, or an immutable binding that is itself a
   `Str` or `Slice`. The binding is shared (`in`), so it cannot be moved into
   a `let mut` or written (`E0305`, `E0307`). It also follows the existing second-class rules
   (`E1520` for results, `sink`, fields), so it cannot outlive its source.
   Views of locals, `inout` or `sink` parameters, and `let mut` views stay
   `E1531`/`E1521`.
3. **Slice views as receivers** (`src/seq/check.rs`). `b[a..c].find_not_in(..)`,
   `b[1..4].len()`, `s.as_slice().len()`, `b[2..4].try_get(i)`: a view
   receiver of a reading `Slice` method is borrowed `in`, like a view
   argument, and its source stays borrowed until the method's arguments are
   evaluated, so they cannot move or update it. A view receiver of an updating method (`o[0..2].fill(1)`) is
   `E1521` with a hint to pass the view as an `inout` argument.
4. **Manuscript** §4.3 "Two-phase argument loans" (it was "Two-phase
   receiver loans") and the `Slice`/`Str` paragraphs of §13 (bound views,
   view receivers). **Spec calls (flagged):** reservation now covers every
   `inout` argument; scalar copies may read a reserved place; views bind to
   immutable lets of unchanging sources.

The evaluator, JIT and object backends needed no change. They already
evaluate arguments left to right before the call, `seq/ir.rs` lowers a
view receiver as `SubSlice`, and a `let`-bound view lowers like a view
parameter.

## Stage 1 guard

Not touched: `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**`, `packages/vesper-compiler/**`. The self-hosted
Stage 1 checker does not reserve these loans, so it stays stricter. The
`bootstrap-stage1` workflow runs on the branch, and its result is linked in
the PR.

## Tests

- `tests/two_phase_arguments.rs`: the Chorus, Pact/Loom-audit and eBPF
  repros, plus scalar copies that the callee overwrites before it reads its
  parameter. Each runs through the evaluator, the JIT with optimize on and
  off, and an executable, and all must agree. The test also covers the
  refused cases (writes, nested `inout`, `sink`, overlapping `inout`, live
  `in`, copy aggregate) and checks that the Place IR check accepts the
  accepted programs.
- `tests/bound_views.rs`: bound `as_bytes`/`as_str`/interval/`as_slice`
  views and views of views; slice-view receivers (`len`, `find_byte`,
  `find_not_in`, `try_get`); refused views of changing places, `let mut`
  views, escapes, and updating view receivers. All run differentially.
- Self-hosted checker lockstep fixtures: `tests/fixtures/self_host_check/inout_overlap.ves`
  (`pair(inout a, in a)` of an `i64`) and `nested_loans.ves`
  (`pair(inout a, identity(a))`) are now accepted by the Rust checker but
  still refused by the stricter Stage 1 checker, so `tests/self_host_check.rs`
  saw the two disagree. Both fixtures now use an overlap that both checkers
  refuse (`pair(inout a, inout a)`, `pair(inout a, bump(inout a))`), with the
  same E0301 diagnostics.
- Updated tests: the "only receivers" case in `tests/two_phase_borrows.rs`
  now checks a write in a plain call's argument. Two `tests/strings.rs`
  cases bind views of `inout` strings, which are still refused. In
  `tests/language.rs`, `projection_overlap` now uses a lent copy aggregate.
  In `tests/diagnostic_causes.rs`, `both(inout p, in p)` now reports only the
  live overlap.

## Repros before and after

`vesper run` with main (9305b24) and with this branch. The JIT and an
executable give the same values; the tests check this.

| Repro (report) | main | branch |
| --- | --- | --- |
| `f(inout c, g(in c))`, `f(inout c, ga(in c, 3))` (Chorus gap 1) | E0301 | 450 |
| `put(inout s, get(in s, 0))`, `put(inout s, s.len())`, `note(inout st, st.n)` (Pact gap 1, Loom audit) | E0301 | 55030103 |
| `set(inout v, 1, reg(in v, 0) + reg(in v, 1))`, two reserved loans in one call (eBPF gap 7) | E0301 | 8311 |
| scalar copies overwritten by the callee | E0301 | 38100 |
| `w8(inout out, w8(inout out, 0, 1), 2)` (Warp side note) | E0301 | E0301 (by design: the inner call writes the place) |
| `let b = s.as_bytes();` on `in Str` (Chorus gap 2) | E1531 | 100 |
| `b[a..c].find_not_in(..)` and other view receivers (Aegis gap 6) | E1521 | 63104 |

## Port rewrites

Local rewrites only. No port change is committed (lane 12 or each port's
own lane adopts them). The patches are in the project files under
`p3b-gaps-argument-borrows/`.

- **Pact, `out.ves`**: 34 hoisted `let`s inlined into their `inout` call
  (`w(inout s, nlen(in s, n))`). Main refuses the result with 34 `E0301`
  errors, and the branch checks it. `cargo test -p vesper-pact` passes all 51
  tests. The release `abba` binary is **byte-identical** to the one built
  from the unrewritten source, so speed cannot change. Callgrind `once`:
  7,523,549 vs 7,523,563 instructions on `world.json` (the difference is
  process noise in identical binaries) and 214,262,111 on both for the
  generated world.
- **Chorus, `local.ves`** (on PR #1081's branch merged with this one): 57
  hoisted `hN` locals inlined. Across all modules the same rewrite finds
  137, which matches the report's "about 140". Main refuses the result with
  39 `E0301` errors, and the branch checks it. The library differs by 40
  bytes from the unrewritten build.
  `cargo test -p vesper-chorus` passes. The release `abba once`
  digests are identical (`project` fec57de8c2e6445a, `explore`
  1bb1151218a12d94). Callgrind: `project` 414,977,655 -> 415,209,725
  instructions (+0.06%), `explore` 6,158,362 -> 6,158,812 (+0.01%). Code
  generation is essentially unchanged.

Ports not re-measured here: eBPF, Loom audit, Aegis and Warp. Their gaps are
covered by the repros above. Their hoisted locals would compile to the same
code, as Pact's did, so the change is about readability, not speed.

## What is left

- An `inout` loan in a nested argument (`w8(inout out, w8(inout out, ..))`)
  stays refused. Allowing it would mean a write during reservation, which
  the manuscript rules out for receivers too.
- A copy aggregate read straight from a reserved place (`f(inout q, q.p)`)
  stays refused because native code lends it by address. Copying such
  arguments into a temporary in lowering would allow it, but that lives in
  `src/ir.rs` (Stage 1 guard).
- Bound views of mutable locals or `inout` parameters would need the checker
  to track the view's last use against later writes. Today only unchanging
  sources are allowed.
- Updating methods on view receivers (`o[a..b].fill(x)`) still need the view
  passed as an `inout` argument.
- The self-hosted Stage 1 checker (`packages/vesper-compiler`) stays
  stricter, as it already was for receivers.
