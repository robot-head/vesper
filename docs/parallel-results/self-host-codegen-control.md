# Self-hosting SH4 lane 10: closures, handlers, effects, dyn and sessions

Claim: #1284. Prompt: [10-codegen-control.md](../agent-prompts/self-host-sh4/10-codegen-control.md).
Package: `packages/vesper-codegen-control` (`@vesper/codegen-control`).
Differential: `tests/self_host_codegen_control.rs`.

Part 1, 2026-11-21. Stacked on #1259 (SH3 lane 1 part 1), first at `8ac0f94`, then merged up to its head `5b50435`;
`origin/main` was `e73cfd6`. Lane 1 (#1275, lent Cranelift handles and
`src/codegen_swap.rs`) has no pull request yet, so this part ports only the
decisions that emit nothing. Rust stays the default build; nothing here
installs a hook, and no Rust lowering changed.

## What part 1 ports

| Rust | Vesper | Differential | Result |
|---|---|---|---|
| `native::session::plan` (src/native_session.rs): binding validation, `word_buffer`, `validate_op`, `reject_suspending_recursion`, `specialized_function`, `fail_only`, `capability_only`, `contains_function`, the data-frame layout | `session.ves` | `{:?}` of the plan, or the refusal message, per case | 0 of 292 corpus worlds differ: 1,872 cases, 205 plans; plus the native-pulse examples' own bindings and 9 hand-written refusals |
| `native_handlers::effect_key`, `Kind::of` (src/native_handlers/lower.rs) | `keys.ves` | the key of every effect instance of every row a world mentions, and the kind of every type | 0 of 292 worlds differ (25,864 lines) |
| the parts of `World` the plans read (function signatures, effect declarations with operations) | `tables.ves` | through the two above | |

The session cases: every function whose row names a declared linear
effect, with bindings synthesized from its operations' result enums and
up to 15 mutants of them (a qualified constructor, a dropped, duplicated or
renamed operation, swapped constructors, the wrong operation kind, a
ghost or repeated effect, and so on), plus `main` and two other functions
per world with their base bindings, which hit the early refusals. The test
asserts that 17 of the refusals fire on the corpus; the hand-written cases
cover the read-buffer and constructor-shape refusals the corpus never
reaches. Two refusals are not reached by any case: "has no body" and
"appears twice in the entry row" (the checker merges a repeated row
instance).

Times, debug build on a 4-core cloud host: each corpus test is about 240 s,
almost all of it loading the corpus (`check_harness::corpus`); the Vesper
package compiles in well under a second. Performance is not measured in
part 1: these plans run once per session entry and per row, and the parity
bar of the wave is the whole compile with the family swapped, which needs
lane 1.

## Stage 1 guard

Nothing under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changed, and no Rust source
changed at all: the diff is a new package, a new test, this report and the
test's data and timeout lines in `BUILD.bazel`. `bootstrap/stage1.lock` is
untouched by construction; the bootstrap-stage1 workflow runs on the branch.

## How the family emits today, and what the swap needs from lane 1

The control family is not a separate pass: almost every decision is made
while the `Codegen` walks one function and reads state the walk built.

- **Closures are resolved statically.** `closure_tag` and `function_tag`
  push a `NativeClosure` descriptor (type, target, captured stack slots,
  generic bindings, capability evidence) onto the per-compilation table and
  emit its index as an `i64` constant. `NativeInfo.targets` follows each
  value through variables, block parameters and aggregate fields, so a call
  site knows which descriptors can reach it. `invoke_tag` emits one compare
  and branch per candidate whose `callable_shape` matches and inlines that
  candidate's body (or calls the named function, or resumes a
  continuation), ending in `trap user3`. `qualify_tag` re-numbers
  descriptors whose evidence changes when a value is read at a wider type.
  A port must keep the table and the target sets in step with Rust's,
  value for value, or the tag constants and branch order differ.
- **Handlers are inlined.** `Op::Handle` lowers its AST expression through
  `Codegen::expr`: a dispatch block, a selector variable and one return
  block per resumption site (`HandlerControl`). `perform` jumps into the
  clause; `invoke_continuation` jumps back, saving the selector in a stack
  slot unless the resumption is in tail position, and refuses a non-tail
  resumption from inside a loop (`LOOP_RESUME_REFUSAL`). Functions with
  handler effects are specialized into each caller (`specialized_function`,
  ported here as part of the session plan); `Fail`-only and
  capability-only rows are compiled once with a failure record or register
  status (`fail_exit`, `reraise_status`, `reraise_each`).
- **`dyn`** builds a `{data, vtable}` stack pair (`dyn_coerce`), emits one
  data object per (object, type) (`dyn_vtable`: drop word, size, align,
  one slot per method, a by-value adapter where the receiver is a scalar),
  and calls effectful slots by comparing vtable addresses
  (`dyn_call_effectful`).
- **Sessions** run `plan` (ported) and then lower each bound operation to a
  fiber ABI v2 park with value, error and cancellation edges
  (`session_park`, `session_fail`, `session_return`).

So the `control` hook needs more than one entry point and needs the walk's
state, not only the builder:

1. Entry points at `closure_tag`, `function_tag`, `qualify_tag`,
   `invoke_tag`, the `Op::Handle` arm, `perform`/`perform_values`,
   `fail_exit`, `reraise_*`, the trace helpers, `dyn_coerce`, `dyn_call*`,
   `dyn_vtable` and the session park, fail and return sites.
2. Lent access to the `Codegen` state these read and write: the closure
   table, `infos` (target sets), `vars` and `local_types`, `capabilities`,
   `generic_vars`, `inline_depth`, `cycle_depth`, `tail_call`, the
   `HandlerControl` of each enclosing `handle`, and the stack slots of
   captures. Re-entering Rust for nested expressions (`self.expr`,
   `call_function`, `ir_graph`) from Vesper is required too: a closure body
   or a clause is ordinary code lowered by the other families.
3. Lane 4 (#1278) changes how callback parameters are homed
   (`in_param_registers`); `invoke_closure_body` reads that, so emission
   waits for lane 4 as the plan says.

I will send this list to lane 1 when its first pull request is up, so the
lent handles cover it.

## Compiler gaps

1. **Closure and handler expressions are not in the Vesper tables.** The
   native backend lowers `Op::Closure` and `Op::Handle` from their AST
   expressions (`Codegen::expr` on `Op::Closure { expr, .. }`), and
   `@vesper/place-ir` keeps those expressions in a Rust `Pool`
   (tests/common/ir_words.rs). Checked function bodies are not encoded
   either, which is why the harness computes the call edges of
   `reject_suspending_recursion` (`native_session::callees`, copied into
   the test) and passes them in. Closure shapes and handler plans (captures,
   clause kinds, tail resumptions) need an expression encoding first. That
   belongs with #1259's follow-ups or lane 2 (#1276); this lane will use it.
2. **The Vesper package assembler does not import constants across
   packages.** Rust's loader accepts `use @vesper/check-layout::{L_ENUM};`
   since #1239, but `@vesper/check-names-packages` reports E0111 "module
   @vesper/check-layout has no item 'L_ENUM'", so `self_host_check_names_packages`
   fails on any package that does it. `L_PRODUCT` and `L_ENUM` stay copied.
3. **`effect` is a keyword, not a contextual name.** A local or field named
   `effect` is a parse error (E0002 "expected a lower-case identifier, found
   the keyword `effect`"). #1264 makes it a name; until it merges the port
   uses `eff_name`.

No Rust bug was found: every case matched.

## What is left

- Closure shapes, tags and handler plans, after gap 1 (an expression
  encoding).
- Emission of the whole family through lane 1's lent builder and the hook
  state above, then a CLIF and object differential against Rust with
  optimize on and off, after lane 1's first PR; callback parameters after
  lane 4.
- The `codegen_control` hybrid module and `control` hook install, the swap,
  the full suite and the SH4 scoreboard row.
- Measurements: emission alone and the whole compile with the family
  swapped, ABBA and callgrind.
