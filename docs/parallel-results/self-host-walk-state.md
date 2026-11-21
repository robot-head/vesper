# Self-hosting SH5 lane 1: lent walk state and re-entry into Rust (#1324)

Stacks on #1309 (SH4 lane 1, the codegen swap point). Base: #1309's head
`6e22c6c`, which merges origin/main `a656954`. origin/main was `bbda89e`
when this lane started.

SH4's family hooks get the live Cranelift builder and module, so they can
emit leaf instructions. They cannot emit anything that needs the graph
walk's own state: which frontend variable a local is, whether it lives in
a register or a home slot, a split local's words, the plans the walk made
for the instruction, the value infos the Rust bookkeeping keeps, or a
sub-expression the hook cannot lower yet. SH4 lane 6 left every `Read`,
`Bind` and `Write` to Rust for that reason (10,307 + 3,649 + 295 sites),
and lanes 7 and 10 left result stores, `inout` spills, the Fail record,
closure and clause bodies, `perform`, `fail_exit` and `dyn` for the same
reason.

This lane lends that state to an instruction request, lets a hook ask Rust
to emit a piece of the instruction, and proves it on `Read`, `Bind` and
`Write` with a demonstration hook. The Rust backend stays the default and
nothing in the default compiler installs a hook.

## What changed

### The protocol (`src/codegen_swap/walk.rs`)

While a hook answers an `INSTRUCTION` request at the graph walk, three
calls reach the walk. They are Rust functions (`walk::query`,
`walk::record`, `walk::reenter`) and C-ABI symbols (`vesper_walk_query`,
`vesper_walk_effect`, `vesper_walk_reenter`, listed by `walk::symbols()`)
that a Vesper hook imports through `@vesper/codegen-walk`.

- **Queries** read the walk state. Facts about the instruction: its kind
  (`Read`/`Bind`/`Write`), its place's root variable and path length, a
  read's pass, whether a write is `=`, type flags (aggregate, function,
  narrow or 128-bit integers that `coerce` changes, constructors), the
  walk's plans for it (hoisted header read, split local, homed read,
  split source, array in place, bound early, placed write, drop slot,
  replacing write, fused `try_get`, element views), and whether the walk
  is on the optimize path. Maps: a variable's home-slot address variable
  (`pointers`), narrow element views (`narrow_homes`), register locals, a
  split local's word variables, the variable that holds an operand, and
  the words kept across re-entries. A query outside a request answers
  `NONE`.
- **Effects** are the Rust bookkeeping of what the hook emitted. The hook
  emits the instructions itself and records `USED` (`use_var`), `DEFINED`
  (`def_var`), `STORED` (`store` and `def_var`'s home store), `EDGE`
  (`jump`/`branch`'s `edge_info`) and `TEMP` (`ir_temp`'s
  `$ir_value<id>` naming). The walk applies them in order when the hook
  returns, before anything else is emitted, so value infos (closure
  targets, field infos) and temporaries are what the Rust path leaves.
- **Re-entry** is continuation-passing. The hook calls `reenter(kind)` and
  declines. The walk emits the piece, appends its value to the request's
  kept words and asks the hook again with the same request. `KEEP` adds
  the hook's own words, so it resumes where it stopped: emit a prefix,
  keep what the rest needs, re-enter, and skip the prefix on the next
  call. The kinds are `INSTRUCTION` (the whole Rust arm; the hook then
  answers, usually with that value), `PLACE_VALUE` (`Codegen::expr` of the
  place) and `PLACE_ADDRESS` (`Codegen::address`).

Re-entry is not synchronous on purpose. The builder belongs to the hook
while it runs (the shim lends it from a `&mut FunctionBuilder`), so Rust
can emit only between two calls without aliasing it. A hook that never
calls the walk is unaffected, and the `Hook` type and `Lent` did not
change, so the SH4 lanes' hooks work unchanged. The protocol is written
into the `codegen_swap` module docs (a new "Walk state and re-entry"
section) and `walk.rs`'s own docs.

### The walk-state hunk of `src/native.rs`

`Codegen::ir_graph_at` asks `codegen_swap::walk::instruction` instead of
`codegen_swap::instruction`, only when `walk::wanted` (a hook of the
instruction's family is in effect). Then:

- `walk_facts` builds the facts from the walk's locals (hoisted reads,
  homes, split sources, bound-early binds, placed values) and `Codegen`'s
  state;
- `walk_instruction` calls the hook, emits the place re-entries and asks
  again (at most 64 times per instruction);
- an `INSTRUCTION` re-entry falls through to the Rust arm, and
  `walk_resume` asks the hook again with the arm's value;
- `walk_apply` applies the effects.

`WalkMaps` lends `pointers`, `narrow_homes`, `register_locals`, `split` and
the operand registers as shared borrows of `Codegen`'s fields next to the
lent builder. Nothing else in the backend changed. With no hook installed
the walk takes the same path as before.

### `@vesper/codegen-walk` (packages/vesper-codegen-base/walk)

A new package (new files only, under `@vesper/codegen-base`'s directory)
with its own `.vsys` library `Walk`: the `// @export vsys direct
rust=vesper::codegen_swap::walk` directive of `src/lib.ves`, copied to
`walk.vsys` and listed in the manifest's `vsys` field so dependents import
it (a test keeps the two equal). It has the query, effect and re-entry
constants and calls, `var_of_word`/`var_word`, and the reusable helpers
that mirror the Rust ones: `walk_use_var`, `walk_def_var`, `walk_store`
and `walk_temp` emit what `Codegen::use_var`, `def_var`, `store` and
`ir_temp` emit and record their bookkeeping. This is what SH4 lanes 6, 7
and 10 adopt in their own packages.

### The demonstration hook (crates/vesper-codegen-swap-test/walk-demo)

`@vesper/codegen-walk-demo`'s `walk_hook`, installed as the `scalar`
family hook in the test, answers every `Read`, `Bind` and `Write`. It
mirrors the Rust arms in order and:

- emits the plain forms itself: a scalar local's read (its variable, or
  its home slot's word), a binding of one name, an `=` to a scalar local
  (temporary, `def_var`, the home store, `Assign`'s second store) or to a
  field (value first, then the field's address by `PLACE_ADDRESS`
  re-entry, then the store), and the forms whose Rust arm is a zero;
- re-enters for the place's value (field reads) and address (aggregate
  reads);
- re-enters the whole instruction for everything else (split locals,
  homes, hoisted reads, moves, drops, narrow element views, coercing
  kinds, compound assignments, patterns), and answers with Rust's value.

It is a test of the protocol, not a port. Lane 6 owns the scalar
family's package.

### Tests (crates/vesper-codegen-swap-test/tests/self_host_walk_state.rs)

- `the_walk_demo_matches_the_rust_lowering`: the corpus differential with
  the demo installed, optimize on and off. Every `Read`/`Bind`/`Write`
  request must be answered, at least one emitted in Vesper and one
  re-entered, with 0 CLIF, object, run and one-sided differences.
  `CODEGEN_SWAP_STEP=n` samples, and `WALK_STATE_WORLD=a,b` selects worlds
  by name.
- `walk_protocol_errors_fail_the_compile`: an unknown re-entry, and a
  hook that re-enters and answers in one call, fail the compile with
  their messages.
- `the_walk_vsys_is_its_directive`, `the_walk_demo_compiles`,
  `no_walk_outside_a_request`.
- `dump_walk_worlds` (ignored): writes the Rust and hooked CLIF of
  selected worlds, for debugging a difference.

The Bazel target gets the `long` timeout, like the SH4 differential.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed
(`git diff --name-only 6e22c6c` filtered by those paths prints nothing).
The walk is reached only from the native graph walk. The verified backend
never calls `codegen_swap`. The bootstrap-stage1 workflow
(`bootstrap/run.sh stage1 --reproduce --outcome` against
bootstrap/stage1.lock) runs on the branch once #1309 has merged and the
branch can merge, per the CI rule.

## Numbers

Linux x86_64, debug builds.

**The default compiler is unchanged.** I compiled every `.ves` file under
examples/, tests/ and bootstrap/ under 200 KB (539 files) with `vesper
emit --emit object`, with and without `--no-opt`, using the base compiler
(`6e22c6c`) and this branch. 318 objects built on both sides and **0
differ**. Every file that failed, failed on both sides (0 one-sided).

**The demonstration** (`the_walk_demo_matches_the_rust_lowering`, full
corpus): 298 world builds (149 corpus worlds with a `main`, optimize on
and off), 882 functions and 74 JIT runs, with **0 CLIF, 0 object, 0 run
and 0 one-sided differences**. 14,251 `Read`/`Bind`/`Write` requests (the
sites lane 6 left to Rust), **all answered** from Vesper: 4,275 emitted in
Vesper on the first call, 9,976 through re-entry (9,128 of the whole
instruction, 101 of a place's value, 747 of a place's address), over
24,227 hook calls.

The SH4 differential (`self_host_codegen_swap`, 5 tests) still passes on
this branch.

**Cost.** I did not measure the crossing on its own. The whole comparison
loop (every world built with and without the demo, plus the runs) took
57.4 s in the debug test. Each request costs one crossing, one more per
re-entry, and a handful of walk queries; the facts are built only when a
hook of the instruction's family is in effect, so the default compiler
pays one relaxed load per instruction, as before.

## Compiler gaps

None blocks this lane. What the Vesper side ran into:

1. **No word conversion for `Var` in @vesper/cranelift.** `Value`, `Block`
   and `Ty` have `*_of_word`/`*_word`; `Var` has none, and the walk
   answers variables as words. `@vesper/codegen-walk` builds `Var { id }`
   itself, which works because struct fields are not private across
   packages. Lane 2 (shim extras, #1325) owns `packages/vesper-cranelift`
   and could add `var_of_word`/`var_word` there.
2. **`effect` is a keyword**, so it cannot name a function (E0002,
   "expected a lower-case identifier, found the keyword `effect`"). That is
   by design. The package uses `record`.
3. **Nested `inout` of one place in one call is refused** (E0301,
   `ins_load(inout b, value_type(inout b, v), ...)`), as Rust refuses
   `f(&mut b, g(&mut b))`. Not a gap, but the first thing a port of the
   Rust helpers writes. Hoist the inner call into a `let`.

## What is left

- **Adoption** by SH4 lanes 6 (`Read`/`Bind`/`Write` and the raising
  arms), 7 (result stores, `inout` spills, the Fail record) and 10
  (closure and clause bodies, `perform`, `fail_exit`, `dyn`, session
  parks) in their own packages and hybrid installs. The demo shows the
  pattern; it is not installed in the hybrid compiler.
- **More of the Rust conditions as facts are only bits.** A port that
  owns a construct must own the conditions too: the facts tell a hook
  which Rust arm applies, but the hook still mirrors Rust's order of arms,
  as the demo does. As ports take over more arms, the plans they read
  (homes, split sources, drops) can move into Vesper with them.
- **More effects.** The walk lends the bookkeeping of `use_var`,
  `def_var`, `store`, `jump`/`branch` and `ir_temp`. Others (`offset`'s
  pointer parents, `split_write`, the closure table, handler state) are
  added when a port needs them, the same way.
- **Re-entry beyond the instruction and its place.** Lane 10's nested
  graphs (closure and clause bodies) can re-enter the whole instruction
  today; re-entering one nested graph while the hook keeps the rest needs
  a new re-entry kind once lane 10 says which pieces it wants.
- **Synchronous re-entry** would need the shim to let Rust emit while a
  hook holds the builder. Continuation re-entry covers every case above;
  this is only ergonomics.
