# Self-hosting SH4 lane 2: swap inputs without re-encoding

> Claim: #1276 (part 1). Prompt:
> [`docs/agent-prompts/self-host-sh4/02-swap-inputs.md`](../agent-prompts/self-host-sh4/02-swap-inputs.md).
> Base: #1259's head `8ac0f94` (SH3 lane 1 part 1, which stacks on #1232),
> `origin/main` at `e73cfd6`; #1259's later head `5b50435` merged in. The
> [manuscript](../manuscript/README.md) is the sole specification; this
> lane needs no change to it.

Rust stays the default build. Nothing in the default compiler enables the
new scopes or installs a hook, and no Rust code was deleted.

## Summary

Every swap encoded its whole input on every call. For the IR check that is
one call per compilation phase, but the layout (`Layout::of`) is called
about 290 times per compiled program, and each call encoded the ADTs the
type reaches and had Vesper decode them again. Measured inside the hybrid
compiler over every corpus program it builds (159 programs, 46,421 layout
calls):

| Swap | Rust pass | Input before | Input after | Whole swap before | Whole swap after |
|---|--:|--:|--:|--:|--:|
| check-layout (`Layout::of`), answers reused | 28-29 ms | 71 ms (2.43x) | 9.9 ms (0.36x) | 11.4x | **0.62x** |
| check-layout without answer reuse (first PR commit) | 29 ms | 70 ms (2.39x) | 11.6 ms (0.41x) | 11.3x | 3.83x |
| ir-check (`ir_check::validate`) | 94 ms | 0.41x | 0.47x | 1.04x | 1.11x |
| parse | 49-55 ms | none (source lent) | none | 1.6x | 1.6x |

"Input" is Rust-side encoding; "whole swap" adds the Vesper call (which
includes Vesper's decode of its input) and Rust's decode of the answer.
The "before" column is the same hybrid with `VESPER_HYBRID_SCOPES=0`,
which already uses this lane's faster type interner, so it understates
the original cost: the base hybrid's own per-call layout measurement
(`tests/hybrid_layout.rs`, `layout_speed`) is 5.02x of Rust on its corpus.

Both IR-check columns already use the lean world section (below), which
cut its input by 23% against the whole world section (489 ms to 375 ms in
the encoder-only measurement). Only one IR check per compilation runs
inside a scope, so the encode-once path cannot save anything there yet,
and its bookkeeping costs a little (0.41x to 0.47x, within this host's
noise). It pays off once a hook runs per function.

**Answer reuse** (approved 2026-11-21): `Layout::of` depends only on the
world and the type, so the hybrid keeps every answer the Vesper layout
gave in the compilation's slot and returns it for a repeated type. Each
distinct type still runs through Vesper once per compilation, and
`VESPER_HYBRID_STATS=time` still compares every call, reused or not, with
the Rust layout: 0 of 46,421 differ. Most calls repeat (121 of 126 on
`bootstrap/corpus/ackermann.ves`), so the whole layout swap is now faster
than the Rust pass (0.62x): Vesper 5.4 ms and answer decoding 1.9 ms, down
from 65 ms and 30 ms. The remaining "input" time (0.36x) is mostly the
slot and memo lookup plus the stats timers themselves.

The layout's input is 0.36x of the pass, not under the 10% target, but
the swap as a whole now beats the pass it replaces; see "What is left".

## What changed

- **`src/swap_input.rs`** (new): one input per compilation. A `Scope`
  holds a shared borrow of the `World` for one native pipeline, and
  `with::<T>(world, init, f)` gives each hook its own slot (by Rust type)
  for that compilation: its encoder state and the Vesper value it decoded
  once. A slot is found by the world's address, which is sound because the
  scope's borrow keeps that world alive and unchanged (it has no interior
  mutability) until the scope closes; the address is only compared. Nested
  scopes of one world share slots; a re-entrant use of a slot falls back.
  Closing the outermost scope drops the slots (freeing the Vesper values).
  Without `swap_input::enable()` (nothing in the default compiler calls it)
  `open` reads one atomic flag and `with` returns `None`.
- **Native call sites**: `compile_entry` (src/native.rs), `compile_exports`
  (src/native_export.rs) and `compile_session_with_linkage`
  (src/native_session.rs) open the scope on their first line. That covers
  both `ir_swap::lower` calls of one entry, `ir_opt::optimize`'s IR check
  and every `Layout::of` of code generation. `src/ir_swap.rs` points hooks
  at it. The verified backend, the evaluator and the checker never open a
  scope.
- **`tests/common/check_words.rs`** (the encoders #1232 added):
  - `WorldLog` (one per compilation: the shared `Enc`, each ADT's record
    encoded once, the effect table) and `Cursor` (what one Vesper input
    already holds). `WorldLog::part` writes only what that input lacks:
    new names, new types, a world section of the ADTs it has not had, then
    the call's words. `layout_part` sends the ADTs a type reaches and the
    type's index; a type laid out before sends a constant 72-byte part.
    `ir_part` sends every ADT and effect the first time (the IR check's
    `constructor` takes the first ADT in name order, so its ADTs must
    arrive whole and in order), then only the name ranks and the program.
  - Lazy sections: neither the layout nor the IR check reads expression
    types or the partial/overflow span tables (the IR check gets those
    facts as instruction flags), so no input carries them any more.
    `ir_input` uses `Enc::world_lean`; `ir_input_full` keeps the old input
    for the measurement. Programs whose expression types hold a 128-bit
    literal no longer fall back to Rust for that reason alone.
  - `Enc::ty` interns by a structural hash (`type_hash`, an Fx-style
    hasher) instead of `format!("{t:?}")`. Indices are the same (order of
    first use). It also speeds every other user of `Enc`, including
    `ir_words` (#1259).
- **Vesper side** (decoder hunks of #1176's and #1232's packages):
  - `@vesper/check-data`: `ctx_new` (an empty context) and `extend_ctx`
    (append one part's names, types and world section to a context).
  - `@vesper/check-layout`: `layout_part` and the harness entry
    `check_layout_parts`.
  - `@vesper/ir-check`: `ir_check_task` (the program half of
    `ir_check_words`), `ir_check_part` and the harness entry
    `check_ir_parts`.
- **Hybrid** (`crates/vesper-hybrid`): `modules/check_layout` exports the
  handle `LayoutInput` and `modules/ir_check` exports `IrInput` (owned
  Vesper `Ctx` values, export ABI v4); `src/hybrid.rs` keeps one of each,
  with its `Cursor`, in the compilation's slot, and falls back to the
  per-call input outside a scope. `install` calls `swap_input::enable()`
  unless `VESPER_HYBRID_SCOPES=0`. `VESPER_HYBRID_STATS=1` now also prints
  each swap's calls, scoped calls, and encode, Vesper and decode time;
  `VESPER_HYBRID_STATS=time` also runs the Rust pass on every swapped call,
  times it, and counts calls whose answers differ. Bindings regenerated.
  The divergent builds also break the scoped paths (build.rs and
  BUILD.bazel).
- **Tests**:
  - `tests/self_host_swap_inputs.rs`: scope semantics (no scope, one slot
    per world and type, nesting, another world, re-entrancy, drop on
    close); `layout_parts_match_rust` (every corpus world sent to one
    Vesper context in parts, every type twice, each answer against
    `rust_layout_of`, repeats exactly 72 bytes); `ir_check_parts_match_rust`
    (each world's program and its five mutants as parts of one context,
    against `rust_validate`, every fifth world by default,
    `SWAP_INPUTS_STEP=1` for all); `swap_input_costs` (ignored timing).
  - `crates/vesper-hybrid/tests/swap_inputs.rs`: the hybrid builds objects
    with the checker swaps inside a scope, zero differing answers, and the
    same with scopes off.
  - `crates/vesper-hybrid/tests/check_swaps_leaks.rs`: a dropped
    `LayoutInput` or `IrInput` frees everything it held.
  - BUILD.bazel: data and `long` size for the new root test; the hybrid
    test gets `vesper-hybrid` on `PATH`.

## Results

Differentials (optimized test builds, this machine):

| Check | Differences |
|---|--:|
| `layout_parts_match_rust`, every corpus world, every type twice | 0 |
| `ir_check_parts_match_rust`, every fifth world with five mutants each | 0 |
| `tests/hybrid_layout.rs`, `tests/hybrid_ir_check.rs` (now with lean IR input) | 0 |
| Hybrid over the corpus with `VESPER_HYBRID_STATS=time`: 46,421 layouts, 477 IR checks, 581 parses | 0 |
| `crates/vesper-hybrid` tests, including the differential against the default compiler | pass |
| Divergent layout and IR-check builds | caught |

Encoder-only costs over the root corpus (`swap_input_costs`, no Vesper,
best of five, 7,568 layout calls):

| | Rust pass | Per-call input | Encode-once parts |
|---|--:|--:|--:|
| Layout, input | 19.3 ms | 24.7 ms (1.28x), 2.8 MiB | 18.7 ms (0.97x), 1.4 MiB |
| Layout, Vesper with its decode | | 47.1 ms | 22.0 ms |
| IR check input, whole world section | | 489 ms, 79 MiB | |
| IR check input, lean world section | | 375 ms, 67 MiB | 318 ms on a second call in a scope |

(The root corpus's IR check takes 56 s in Rust because a few packages are
pathological for it; the in-hybrid numbers above are the representative
ratios.) This harness calls `world_types` once per world, so few types
repeat; in the compiler most layout calls repeat a type, which is why the
in-hybrid input cost (0.41x) is lower than this one (0.97x).

End to end, `emit --emit object` over the same 159 programs (best of
three, process start-up included): default compiler 3.19 s, base hybrid
3.73 s, this hybrid 3.48 s. The hybrid's extra time over the default
compiler went from 0.54 s to 0.29 s.

### Lent accessors instead of copies

The prompt asks to compare reading the Rust `World` through ABI v4 lent
handles against copying it. I did not build the accessor version, and
this is a decision by design rather than by measurement, for three
reasons:

1. After this lane a repeated layout sends a constant 72-byte part, and
   each ADT crosses once per compilation. Accessors would replace that
   one-time copy with a foreign call on every ADT, variant and field read
   inside the layout's recursion, on every call.
2. The Vesper layout and IR check work over check-data's interned type
   ids. A `Type` read through an accessor would still have to be interned
   into the Vesper arena, which is the same work as decoding it.
3. Accessor imports are `eff [Ffi[Host]]`. `@vesper/check-layout`'s
   capability ceiling is `["Fail", "Alloc"]`, and every function on the
   read path would become effectful (gap 3).

The measurement is still owed; it belongs with the first hook that reads
large Rust tables it does not intern, such as SH4's codegen families
reading Place IR.

## Stage 1 guard

`src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`,
`bootstrap/**` and `packages/vesper-compiler/**` are untouched. The only
compiler edits are the new `src/swap_input.rs`, its `pub mod` line, one
doc paragraph in `src/ir_swap.rs`, and one `swap_input::open(world)` line
at the top of each of the three native pipelines. Without `enable()` that
line reads an atomic flag and does nothing else, so default-compiler
objects are byte-identical. The bootstrap-stage1 workflow run on this
branch is linked from the PR.

## Compiler gaps

1. **One decoded world per Vesper library.** The layout and IR-check
   modules are separate Vesper libraries, each with its own runtime and
   its own copy of `@vesper/check-data`. A `Ctx` one of them decoded
   cannot be lent to the other: each binding has its own
   `vesper__check_2data__input__Ctx`. The encoding is shared on the Rust
   side (one `WorldLog`), but each library decodes its own copy. Repro:
   build `modules/check_layout` and `modules/ir_check` with `--rust-binding`
   and compare the two `Ctx` types. A hybrid that links all swapped
   modules as one Vesper library (or handles shared across libraries
   with one runtime) would fix it.
2. **Answers still cross as text.** `put_layout_words` writes field names
   and types as strings and type trees, and Rust's `decode_layout` costs
   about as much as the Rust layout itself (30 ms against 28-29 ms).
   Answers could name the input's name and type indices instead. The
   Vesper side needs the inverse of `nmap`/`tmap`; Rust already has the
   `Enc` tables. That is a format change for a follow-up, not a compiler
   gap.
3. **Lent accessors are effectful.** A `.vsys` accessor of a lent handle is
   `eff [Ffi[Host]]`, so a pure pass that reads Rust tables through one
   needs `Ffi` in its signature and in its package's capability ceiling.
   Repro: the `World` pilot in `tests/tree_abi.rs`
   (`fn world_type_count(in w: World) -> ForeignVal[u64]`), called from a
   package whose `capability_ceiling` is `["Fail", "Alloc"]`.
4. **Per-call Vesper overhead.** A scoped layout call with nothing new to
   read costs about 1.4 µs in Vesper (extend an empty part, a fresh
   `Layouts` arena, the output stream) against 0.6 µs for the whole Rust
   layout. Repro: `VESPER_HYBRID_STATS=time vesper-hybrid emit --emit
   object -o x.o bootstrap/corpus/ackermann.ves`, then compare the
   `check-layout` vesper and rust times.

## What is left

- **The 10% target.** With answer reuse the layout swap is 0.62x of its
  Rust pass overall, but the lookup before a reused answer (slot, type
  hash, bucket compare, timers) is still 0.36x. A cheaper key (the type's
  address within one compilation is not stable, so a pre-hashed type in
  the caller would be needed) is a change to `src/layout.rs` callers,
  outside this lane.
- **The `Program` updated in place.** No IR pass hook is installed in the
  hybrid at this base (#1259 part 2 adds `place_ir`), so a program is
  still encoded whole per call. Next: a per-function program-section
  cache in `ir_words` keyed by function, re-encoding only functions a pass
  rewrote, for the SH3/SH4 IR and codegen hooks. Lane 1's pilot does not
  exist yet, so it is not measured here.
- **Lent accessors** (see above): measure when a hook first reads a large
  Rust table.
- **Gap 1:** one Vesper library for all hybrid modules, so one decoded
  world serves every swap.
