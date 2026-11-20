# Closure parameter modes and collection view callbacks (SH3 lane 3)

Claim #1242. Self-hosting wave SH3, lane 3, gaps G5 and G6 of SH2 lane 1
([self-host-element-borrows.md](self-host-element-borrows.md)). Started on
the head of #1221 (`6d96c9c`, origin/main `c8ac321`); #1221 merged during the
lane and main (`005860b`) is merged in.

## What the repros showed

The prompt's repros split in two:

- `|in v: State| v.n` and `|inout v: State| { v.n = 1; }` already worked:
  closure parameters have taken pass modes since the parser accepted
  `ClosureParams ::= Param`, and the checker and every backend handled them.
- What was missing was everything around a callback. Inside `with_ref` the
  parameter `f: fn(in V) -> R` could not be called (`E0305`, calling an
  affine function value consumes it, and it is borrowed), so the callee
  needed `fn@copy`. And at the call site a closure could not use a borrowed
  parameter of the caller at all (`E0310`, even a `u64`), while a non-copy
  local it used was moved into it, so the caller lost it, and the JIT then
  leaked it. So `t.with_ref(k, |in v| v.n == key.n)` was not writable.

## The language change (manuscript §4.2, flagged)

One new item in §4.2, "Closure Parameter Modes and Lent Closures":

- A `copy` value a function holds `in` (every by-value parameter) is captured
  as a copy. This was `E0310`.
- A parameter `f: fn(..) -> R eff [E]` passed `in` whose function type is not
  `copy` is a *lent function parameter*. The body may call it any number of
  times (each call is a read), and may lend it again as the argument for
  another lent parameter. Binding, storing, returning, moving it, or
  capturing it in a closure that is not itself lent stays refused (`E0305`,
  `E0310`). This form was unusable before (every call was `E0305`), so no
  program changes meaning.
- Its argument must be a closure literal, a `copy` function value or a lent
  parameter (`E0310`). A closure value that owns its captures is call-once
  and is never lent.
- A closure literal written directly as that argument is a *lent closure*.
  It is second-class and lives for the call only. Instead of capturing the
  names it uses, it borrows each one `in` for the call, as one more `in`
  argument of the call. Those names stay the caller's (unmoved, dropped by
  the caller), are read-only in the body (`E0307`, `E0305`), and conflict
  with the call's other argument loans (`E0301`). That includes a reserved
  two-phase `inout` receiver, which stays active for the whole call:
  `t.with_mut(k, |inout v| v.n = t.len())` is refused.

There is no new syntax. The parser and its `packages/vesper-front` /
`packages/vesper-ast` mirrors are unchanged.

## What changed

- `src/check.rs`: lent closures (`lent_closures`, `lent_captures`,
  `World::lent_closures`, keyed by the body's `ir::nested_name`), lent
  callees in `call`, the argument rule in `apply_args`, and copy captures of
  `in` parameters in `closure`.
- `src/ir_check.rs`: a lent closure body's captures are `in` slots, and an
  indirect call of a borrowed function value that is not copy reads it.
- `src/fip.rs`: box drop elaboration treats a lent body's captures as
  borrowed (otherwise the body freed the caller's boxes: double free, then a
  trap). The sequence drop planner no longer refuses every name in scope at
  a closure: a lent closure refuses nothing, and any other closure refuses
  only the names it uses and what it moves.
- `src/native.rs`:
  - An `in` read of a function value is now its tag, not its slot's address.
    Passing a function parameter on to another function trapped (`user3`)
    on main too, even with `fn@copy`.
  - A lent closure's aggregate captures are borrowed by address (one word
    stored, no copy at creation, no copy at the call).
  - A call whose tag can name only one closure branches without a tag test.
- `@vesper-std/collections`: `FlatTable::with_ref`/`with_mut` and
  `CacheBTree::with_ref`/`with_mut` (`Option[R]`, `None` for an absent
  key), effect-polymorphic (`eff [e]`).
- `@vesper-std/arena`: `IdxMap::with_ref` (reads the default when the index
  is unset), `IdxMap::with_mut`, and `IdxMap::put_with(i, v, |j| ..)`, which
  fills the gap below `i` with constructed values (G6). `arena` is outside
  the lane's listed paths, but it is where `IdxMap` lives.
- Tests: `tests/closure_modes.rs` (repros, borrowing, passing on, copy
  captures, refusals, a `Box` tree borrowed by a lent closure with box
  counts) and `tests/closure_modes_collections.rs` (the callback form of
  every lookup equals the element-view form). Each runs in the evaluator, the
  JIT with the optimizer on and off, and an executable, and checks that the
  JIT frees every sequence exactly once. `tests/language.rs`
  `borrowed_capture` changed with the rule: capturing an `in` `i64` now runs,
  and `inout` and non-copy borrows stay `E0310`.

## Numbers

Valgrind callgrind instructions, executables built by `vesper build`. Each
driver runs the lookup part of `tests/closure_modes_collections.rs` 20 times
with 2,000 entries, in its element-view form and its callback form. Both
forms return the same value.

| Part | Views | Callbacks before the native work | Callbacks |
|---|--:|--:|--:|
| `FlatTable` build + 500 `with_mut` + 2,003 `with_ref` | 56.67 M | 59.00 M (+4.1%) | 58.51 M (+3.2%) |
| `CacheBTree`, same | 102.81 M | 104.74 M (+1.9%) | 104.16 M (+1.3%) |
| `IdxMap` `put_with` + `with_mut` + `with_ref` | 83.58 M | 88.98 M (+6.5%) | 86.98 M (+4.1%) |

Part of the gap is the API, not the closure: the same view lookup written
to return an `Option` and unwrap it, as `with_ref` does, costs 57.27 M for
`FlatTable`. Against that, the callback is +2.2%, about 25 instructions per
call. The acceptance bar ("no more than the hand-written view match") is not
met yet. The rest is slot traffic, see gap 1.

Checks: the closure, drop, sequence, borrow, box and language suites (32 test
binaries) pass. `cargo +1.100.0 clippy --all-targets` is clean. The Stage 1
guard: see the PR. The verified pipeline does not reach any changed code
(no `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` edit), and the
`bootstrap-stage1` workflow is run on the branch.

## Port rewrite (step 5)

No checker port on main holds non-copy values in a collection. Every
`FlatTable` in `@vesper/check-names-packages` holds copy values and uses
`get`. The `@vesper/check-fip` bindings the prompt names are on #1228, not
yet merged. So the measurement above uses the collections driver, and the
check-fip rewrite is a follow-up once #1228 lands.

## Compiler gaps

1. **Callback slot traffic.** Inlined `with_ref` keeps its function
   parameter and the names a closure uses in stack slots:
   `in_param_registers` (native.rs) keeps every parameter of a function that
   calls, and never puts a function-typed one in a register. So each call
   stores and reloads the tag and the loop index. Repro: the `flat` driver
   above, `vesper emit --emit clif`, function `flat_callbacks`, slots ss4/ss5.
2. **Closures that own their captures leak them natively.** `fn main() -> u64
   { let s = fresh(1); let f = || s.slots.len(); f() }` leaves one sequence
   for teardown. The capture is moved into the closure and nothing drops it.
   This was so before the lane; lent closures avoid it.
3. **Lent closures are read-only.** `t.with_ref(k, |in v| total += v.n)` is
   `E0307`. An `inout` borrow of a caller's name for the call (a lent
   `FnMut`) is the natural next step.
4. **Type display hides the closure mode.** A mode mismatch prints as
   `expected fn(State) -> u64, found fn(State) -> u64`.

## What is left

- Gap 1, then re-measure against the bar.
- The `@vesper/check-fip` bindings rewrite after #1228.
- Lent `inout` captures (gap 3).
