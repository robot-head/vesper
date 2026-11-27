# Rust-exit gap wave 1, lane 11: the native walk (#1509), part 1

Base: origin/main `66255e17`. Plan: [lane 11](../agent-prompts/rust-exit-gaps-1/11-native-walk.md)
of the [gap wave](../agent-prompts/rust-exit-gaps-1/README.md).

SH4 swapped native lowering family by family through the codegen swap
hooks (`src/codegen_swap.rs`), and SH5 lane 1 lent the walk's state to an
instruction request (`src/codegen_swap/walk.rs`). The walk that drives
those hooks, `src/native.rs` (9,760 lines at the base), and the CLIF
clean-up pass `src/native_clif_opt.rs` (1,365 lines, #1426) are still Rust.
This part maps what the walk does outside the family hooks, ports the
first piece of it (the register plan the walk makes for each graph before
it emits it) to a new package, and proves it with a corpus differential. The
Rust walk stays the default; nothing installs the port yet.

## 1. The map: what `src/native.rs` does, and who owns each part

Line numbers are at the base. "Family" means an SH4 lane's hook already
answers that request (or will, in the lane's open PRs); "walk" means this
lane.

| Lines | What | Owner |
|---|---|---|
| 75-330 | Signatures and instances: `scalar`, `signature*`, `instance_key`, `concrete`, `call_vars`, `function_value_instance`, `specialized_function`, `capability_only` | calls family (SH4 lane 7, #1281; decisions in PR #1306) |
| 358-607 | Register plans: `register_locals`, `keeps_in_home`, `in_param_registers`, `bound_names` | walk (**part 1, ported**) |
| 608-770 | The failure-record and capability ABIs: `fail_only`, `fail_instances`, `capability_abi`, `fail_record_size`, `FnAbi`, `NativeAbi`, `register_result_words` | calls family (#1281, PR #1306) |
| 769-950 | Instance collection: `instances`, `instances_for_entry`, `instances_for_roots`, polymorphic recursion | calls family (#1281, PR #1306) |
| 955-1153 | `compile*`, `compile_entry` (entry checks, the entry wrapper) | calls family (#1281: "entries") |
| 1153-1612 | Declaring and defining instances: `declare_instances*`, `define_instances*` (the per-function context, entry block, parameters and homes, the Fail record, trace plumbing, `native_clif_opt::run`, `wide_native::fuse_wide_divisions`) | walk (part 3: function setup on the lent module); the symbols and signatures it declares are the calls family's |
| 1612-2240 | JIT and object front doors, `WordFunction`, ISA flags and target features | walk (part 4; stays a thin Rust shell until Stage 2, like the hybrid driver) |
| 2239-2458 | `InstrumentedJit`, handler and closure bookkeeping types | control family (#1284) for the handler types; walk for the rest |
| 2458-2650 | `Codegen`'s state and the lent walk maps (`WalkPlans`, `WalkMaps`) | walk (the state the port will own) |
| 2649-2893 | Variables and homes: `flush`, `def_local`, `bind_local`, `read_local`, `use_var`, `def_var` | walk (part 3) |
| 2893-3405 | Function tags, closures, continuations, clause bodies (`closure_tag`, `invoke_tag`, `invoke_closure_body`, `invoke_continuation`, `shared_control`) | control family (#1284) |
| 3405-3535 | `bind_ir`, `ir_temp`, `ir_graph` | walk |
| 3535-4882 | `ir_graph_at`: block layout, sealing, the per-instruction plans (hoisted reads, homes, split sources, arrays in place, early binds, placed writes), the dispatch to the families, terminators and edge infos | scalar family (#1280) owns the walk's arms and its block layout (`blocks.ves`); this lane owns the plans and the bookkeeping around the dispatch |
| 4882-5433 | Exits, defaults, allocation, loads and stores, `address`, `bind_with_box`, patterns | walk (exits, stores) and aggregate family (#1282, patterns) |
| 5433-5810 | The walk-state protocol: `swap_aggregate`, `walk_facts`, `walk_instruction`, `walk_resume`, `walk_call`, `walk_apply`, `walk_place` | SH5 lane 1 (done); it goes away when the walk itself is Vesper |
| 5810-6038 | `swap_overflow`, `pattern_open`, `data_into` | scalar and aggregate families |
| 6038-6875 | `expr_at` | scalar family (#1280) |
| 6875-7673 | `coerce`, `shift`, `bit_builtin`, `convert`, checked arithmetic, `binary`, `bitwise`, atomics, `parallel_for_call` | scalar (#1280) and numeric (#1285) families |
| 7673-8160 | `call_function_into` | call family (#1281) |
| 8160-8476 | `Box` and `fip` allocation and drops | aggregate family's `drops` hook (#1282) |
| 8476-8829 | `perform`, `perform_values`, traces, `return_value` | control family (#1284) |
| 8829-9295 | Equality and drop helpers (`emit_equal*`, `declare_or_define_*_helper`) | aggregate family (#1282) |
| 9295-9760 | `batch`, the far-call tests | walk (tests move with part 3) |

`src/native_clif_opt.rs` rewrites a finished Cranelift function (slot
promotion, branch threading, dead loads, block merging). Porting it needs
read and rewrite access to a lent `ir::Function`'s data flow graph, which
the shim does not lend yet (`vesper_cranelift_sys::lend` lends a builder,
not a finished function). It is still changing under #1426 (gap 8 was
handed over on 2026-11-22), so it waits for #1426 to say it is stable, as
the plan asks.

## 2. Part 1: the register plans in `@vesper/native-walk`

New package `packages/vesper-codegen-walk` (`@vesper/native-walk`; the
name `@vesper/codegen-walk` is the SH5 walk-state library). Over the
`@vesper/place-ir` tables and the `@vesper/check-data` world it computes,
for every lowered graph, `aggregates::cyclic_blocks` (an iterative Tarjan)
and `register_locals` with `keeps_in_home`, `in_param_registers`,
`bound_names` and `bounds_hoist::calls_out`, for a top-level graph and for
a graph inlined by a specialized call (`nested`). These are the scalar
locals and `in` parameters the optimize path keeps in Cranelift variables
instead of home slots (`Codegen::register_locals`), which every family's
reads and writes consult through the walk.

The plan asks two things of each callee: whether its row is empty and
whether it is specialized into its callers (`specialized_function`, with
`capability_only`, `fail_only` and `source_analysis::contains_function`).
Those predicates are the calls family's (`@vesper/codegen-calls`, PR
#1306, not merged yet), so the package keeps private copies, printed as
`sig` lines so the differential checks them too, and drops them for
#1306's once it merges.

The input is place-ir's, plus two sections: the signatures of
`world.functions` (check-data's world has no function table yet) and each
graph's handler free names (`Op::Handle` bodies stay Rust payloads in the
place-ir encoding, so `check::free_names` of them is computed on the Rust
side and passed in).

### The differential (`tests/self_host_native_walk.rs`)

- `vesper_decisions_match_rust`: every corpus world's program, optimize on
  and off (the stages of tests/self_host_codegen_scalar.rs), Rust and
  Vesper text line for line.
- `vesper_decisions_match_rust_on_pass_mutants`: the same with every
  parameter, read and argument pass cycled through `in`, `inout` and
  `sink`, so the plans meet loans and callbacks the corpus rarely has.
- `native_walk_decision_cost` (ignored, `CHECK_HARNESS_SPEED=1`): Rust
  against Vesper, best of five.

src/native.rs is read-only to this lane and keeps these functions private,
so the test's Rust half restates them, calling the compiler's public
predicates where they exist (`Layout::of`, `check::free_names`,
`numeric::default_type`, the effect name tests). Part 2 replaces the
restatement with the walk's own answers through the swap point.

## Results

At the base, debug build, default step (every 4th corpus world, optimize
on and off):

| Test | Programs | Differ | Notes |
|---|---|---|---|
| `vesper_decisions_match_rust` | 168 | 0 | 74,562 lines; 155,599 register locals; 3,555 graphs with loops; 0 not encodable |
| `vesper_decisions_match_rust_on_pass_mutants` | 84 | 0 | two mutants per sampled program |

One difference was found and fixed on the way: Rust's place-ir encoder
keeps bounds hoisting's `SeqOp::Hoisted` and `SeqOp::BoundsGuard` as `seq`
payloads labelled with their `Debug` text (pool kinds 5 and 6 are only for
payloads a Vesper pass makes), so `calls_out` matches those labels by
prefix as well as by kind.

Cost (`CHECK_HARNESS_SPEED=1 cargo test --release --test
self_host_native_walk -- --ignored native_walk_decision_cost`, best of
five over the same 168 programs, encoding excluded): Rust 1.075 s, Vesper
0.370 s (0.34x). The Rust side is the test's restatement, which clones
names into `BTreeSet<String>`s as src/native.rs does, so this is the cost
of the same algorithm in each language, not a build-time ratio; the
build-time ratio of the swapped walk comes with part 2.


## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/stage1*` or `packages/vesper-compiler/**` changed,
and the change does not reach the compiler (a new package, a test and a
Bazel test entry), so the Stage 1 lock is unaffected.

## Gaps found

- check-data's `World` has no function table, so every Vesper pass that
  asks about a callee's signature needs one passed in. This package reads
  its own signature section and `@vesper/codegen-calls` reads its calls
  section; one shared `world.functions` section in check-data would serve
  the walk, the calls family and the inliner.
- Handler bodies (`Op::Handle`'s `expr`) are opaque payloads in place-ir,
  so their free names come from Rust. The walk port needs them as data
  before it can run without the Rust side.
- The shim lends a `FunctionBuilder`, not a finished `ir::Function`, so
  `native_clif_opt` cannot be ported over the lent handles yet.

## What is left

- Part 2: install the plan in the walk through a swap point (a hook
  asked once per graph for its register locals), with the CLIF and object
  differential at 0, and switch the callee predicates to
  `@vesper/codegen-calls` once #1306 merges.
- Part 3: function setup (entry block, parameters, homes, the failure
  record) and the variable and home maps on the lent module, and the
  bookkeeping around the family dispatch in `ir_graph_at`, coordinated
  with the scalar family (#1280), which owns the walk's arms.
- Part 4: the compile drivers, then `native_clif_opt` once #1426 calls it
  stable and the shim lends a finished function.
- Build-time ratio of the whole walk swapped in the hybrid (acceptance),
  which needs parts 2 and 3.


## Part 2 (`@vesper/codegen-calls` switch & `register_locals` swap hook)

### Changes
- **`packages/vesper-codegen-walk/Loom.toml` & `src/lib.ves`**: Replaced the
  private copies of `is_foreign`, `is_capability`, `is_fail`,
  `contains_function`, `visit`, `any_param_contains_function`,
  `capability_only`, `fail_only` and `specialized` with `@vesper/codegen-calls`
  (`syms_new`, `specialized`, `Conc`, `ParamInfo`), cached per signature index
  in `spec_cache`. Threaded `@vesper/check-data::types::Vars` through
  `is_register_scalar`, `callback`, `in_param_registers` and `register_locals`,
  and added `native_walk_register_locals` for single-graph hook invocations
  (`world_lean`, one-function `Program`, referenced callee signatures, `Vars`,
  `nested` flag and `Op::Handle` free names).
- **`src/codegen_swap.rs` & `src/native.rs`**: Added `RegisterLocalsHook`
  (`register_locals: Option<RegisterLocalsHook>`) on `CodegenHooks` and wired
  `Codegen::ir_graph_at` to consult it when `self.operands_only` is true (`None`
  leaves Rust `register_locals` untouched). Made `specialized_function`,
  `cyclic_blocks` and `register_locals` `pub` in `src/native.rs`.
- **`tests/self_host_native_walk.rs` & `tests/common/ir_words.rs`**: Removed the
  restated Rust half (~295 lines) in favor of calling
  `vesper::native::{specialized_function, cyclic_blocks, register_locals}`
  directly, exposed `ir_words::function_fits`, and added
  `the_native_walk_swap_matches_rust` comparing CLIF text and object bytes via
  `vesper::native::object_with_options` + `codegen_swap::with_hooks` over
  synthetic edge cases (`generic_vars` scalar/aggregate/callback substitution,
  `nested` specialized graphs, `Op::Handle` captures) and corpus worlds.

### Differential & Speed Results (`--release`, `--locked`)
- `the_native_walk_swap_matches_rust` (`NATIVE_WALK_STEP=1`): **0 of 153**
  compiled worlds differ (`1978` calls, `0` declined, `29` with generics,
  `1504` nested, `135` with handles, `968` non-empty register sets).
- `the_native_walk_swap_matches_rust` (default `step=4`): **0 of 40** compiled
  worlds differ (`235` calls, `0` declined, `10` with generics, `119` nested,
  `30` with handles, `135` non-empty).
- `vesper_decisions_match_rust`: **0 of 172** programs differ (`107154` lines,
  `217049` register locals, `5119` graphs with loops, `0` not encodable).
- `vesper_decisions_match_rust_on_pass_mutants`: **0 of 86** programs differ.
- `native_walk_decision_cost` (`CHECK_HARNESS_SPEED=1`, 172 programs, best of
  5): Rust `2.1053204s`, Vesper `557.0868ms` (**0.26x**).
- Rust compiler deletion: 0 lines deleted in `src/native.rs` (Part 2 installs
  the swap point and keeps `register_locals` as the unhooked path/oracle until
  the remaining graph walk and hybrid wiring land in Parts 3-4).


## Part 3a: the entry-block plan (#1876)

Base: origin/main `907e9782`.

Before the walk lowers an instance's graph, `define_instances_observed_with_trace`
splits the entry block's parameters and seeds the body's state. Part 3a
separates those decisions from the emission and ports them.

### Changes

- **`src/native.rs`**: new `pub fn frame_plan` and `pub struct FramePlan`
  (with `FrameResult` and `FramePlan::text`). They make the same decisions
  the function setup made inline:
  - which trailing block parameters are the trace flag, the failure record
    (`fail_only`) and the caller slot (`fip::uses_caller_slot`);
  - where the result is built: a scalar value, the leading result pointer
    (`sret`), a register-result buffer of N words, or N words the body keeps
    (the direct small enum on the optimize path);
  - for each parameter, whether its block parameter is the value or the
    address of an `inout` scalar loaded as a given type (whose address then
    keeps a variable of its own);
  - the function-value parameters the instance binds to top-level functions
    (`function_arguments`);
  - the capability identities the body starts with (the Fail record's exits
    and the trace host).

  `define_instances_observed_with_trace` now emits from the plan. The
  refactor keeps the emission order and the order the capability map is
  filled in (after the function-value bindings, whose evidence reads it).
  `EmittedFunction` gains `frame: Option<FramePlan>` (`None` for outlined
  cold helpers), so observers see the plan each function was lowered with.
- **`packages/vesper-codegen-walk/src/frame.ves`** (`@vesper/native-walk`):
  `frame_plan` in Vesper, on `@vesper/codegen-calls`' discovery, `concrete`,
  `NativeAbi::internal`, `scalar` and `layout_facts` (`scalar` and `internal`
  are now `pub` there). It also has its own `enum_shape` word count and
  `mentions_box` over check-layout, plus `numeric::instance_key`. The dump
  entry is `native_walk_frames`. Its input is `@vesper/codegen-calls`'.
- **`tests/self_host_native_walk_frame.rs`**: the differential. The Rust side
  is `EmittedFunction::frame` from `native::object_with_observer`. The input
  encoder of tests/self_host_codegen_calls.rs moved to
  `tests/common/codegen_calls_words.rs` so both tests share it.

### Results (Linux x86_64, release)

| Test | Worlds | Tasks (entry, optimize) | Plans compared | Differ |
|---|--:|--:|--:|--:|
| `frame_plans_match_rust` (`NATIVE_WALK_FRAME_STEP=1`, 4 entries) | 339 (17 not encodable) | 1,976: 1,404 complete, 1 prefix, 562 refused alike, 9 unverified | 5,629 | **0** |
| `frame_plans_match_rust_on_test_sources` (4 entries) | 154 (12 not encodable) | 562: 422 complete, 138 refused alike, 2 unverified | 756 | **0** |

The corpus plans cover 112 failure records, 923 `sret` results, 60 register
buffers, 21 direct enum results, 314 `inout` scalar loads, 2 function-value
bindings and 380 capability identities. On the test sources they cover 66
failure records, 2 caller slots, 103 `sret`, 7 buffers, 2 direct enums,
4 loads, 4 bindings and 198 identities.

The Vesper half takes 426 ms over the corpus. That includes decoding the
world, program and calls sections for every world. Rust needs 598 s for the
same tasks, but that is whole compiles, so the two times don't compare.
They aren't a speed ratio. The build-time ratio comes once the walk is
swapped (part 3b).

The native feature tests that exercise the setup still pass:
`native_fail_status` 21, `native_block_param_homes` 20, `fip` 14,
`capability_effects` 6, `native_hoist_calls` 11, `checked_arithmetic` 3
(release). tests/self_host_codegen_calls.rs is still at 0 diffs on the shared
encoder.

**Rust lines deleted:** 0. The plan is the oracle and the emission is still
Rust until part 3b.

### Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/stage1*` or `packages/vesper-compiler/**` changed. The
`src/native.rs` change is a refactor that leaves the emitted code the same.

### Gaps found

- Trace entries (`compile_entry`'s `trace_ok`) can't be planned in Vesper
  yet. Their discovery includes the entry, and `@vesper/codegen-calls`
  refuses them (`check_entry` models `EntryEffects::Handled` only). So every
  Vesper plan has `trace=0` and no trace-host identity. The Rust plan covers
  both, and no compared task used them.

### What is left

- Part 3b: emit the entry block from the plan on the lent builder, through a
  walk hook installed in the hybrid (serialized behind the merge captain).
  The CLIF/object differential must be at 0. Then the variable and home maps.
- Part 4: the compile drivers, then `native_clif_opt` once #1426 calls it
  stable. `@vesper/clif` (the #1829 tables) already gives a pass read and
  rewrite access to a finished function.

## Part 3b: entry blocks from the Vesper plan (#1939)

### Changes

- **`src/codegen_swap.rs`**: a `frame_plans` hook (`FramePlansHook`). It
  takes the world, the first lowering, the entry and the optimize flag, and
  answers the entry-block plan of each instance by key.
- **`src/native.rs`**: `compile_entry` asks the hook once per compile
  without a trace host, right after the first lowering.
  `define_instances_observed_with_trace` emits each entry block from the
  hook's plan when the plan covers every parameter, and from
  `native::frame_plan` otherwise. `FramePlan::parse` reads the plan text back
  (it round-trips `FramePlan::text`). The library export path
  (`native_export`) and `define_instances` keep the Rust plans.
- **`@vesper/native-walk`**: `native_walk_frames_bytes`, the byte entry of
  the part 3a dump.
- **`tests/common/native_walk_words.rs`**: `decode_frames`. A task the
  package refuses, or a plan that doesn't parse, answers `None`, so the
  whole compile keeps the Rust plans.
  `tests/common/codegen_calls_words.rs` now reaches its sibling encoders
  through `super::`, so vesper-hybrid can include it.
- **vesper-hybrid**: `modules/codegen_walk` exports `frame_plans_words`.
  `src/swap/codegen_walk.rs` installs `frame_plans` (off with
  `VESPER_HYBRID_WALK=0`, like `register_locals`) and reports its calls
  under `VESPER_HYBRID_STATS`.

### Results (Linux x86_64, release)

`the_frame_plans_swap_matches_rust` compiles every task twice, without and
then with the package at the hook, and compares CLIF text and object bytes.

| Run | Compiles | Hook asked / answered | Vesper plans used | Differ |
|---|--:|--:|--:|--:|
| `NATIVE_WALK_FRAME_STEP=1`, 4 entries | 1,966 | 1,980 / 1,838 | 6,392 | **0** |
| default (step 4, 2 entries) | 614 | 622 / 580 | 1,504 | **0** |

The declined compiles are worlds the calls input can't encode and entries
`@vesper/codegen-calls` refuses. Those keep the Rust plans.

**Build time.** On the default run, the 614 compiles take 61.50 s with Rust
plans and 62.44 s with the hook, which is **1.015x**. The hook accounts for
1.54 s of that (about 2.5 ms a compile): it encodes the world and the
program, runs the package and decodes the plans. That is under the 1.50x
bar. The ratio is a single-run sum, not an ABBA CI.

**Rust lines deleted:** 0. `native::frame_plan` stays as the fallback and the
oracle. It can go once the hook answers every compile (trace hosts and the
library export path), and the swap passes its flip gate.

### Gaps

- Trace-host compiles still use the Rust plan, the part 3a gap.
- `native_export` (library objects) and `define_instances` pass no plans.
- The hook encodes the whole world on every entry compile. A per-session
  cache of the encoding would remove most of the 2.5 ms.

### What is left

- Part 4: the compile drivers (JIT/object), then `native_clif_opt` after
  #1426.
- A flip-gate configuration for `frame_plans` and `register_locals`.

## Part 4a: internal calling conventions from Vesper (#1509)

### Changes

- The part 3b hook is now `entry_plans` (`EntryPlansHook`). It answers
  `native::EntryPlans`: the entry-block plans as before, and each
  instance's internal convention (`FnAbi`, now `pub`: register status and
  register-result words).
- **`src/native.rs`**: `compile_entry` builds its `NativeAbi` from the
  hook's conventions in place of `NativeAbi::plan`, but only when the hook's
  frames cover exactly the instances Rust discovered. It keeps
  `NativeAbi::plan`'s filters: optimized builds only, never the entry, no
  default entries. The library export path still plans in Rust.
- **`@vesper/native-walk`**: `frames_bytes` writes a `B <key> <status>
  <words>` line before each instance's plan, from `@vesper/codegen-calls`'
  `internal`, the same decision part 3a's plans already used.
- `tests/common/native_walk_words.rs`: `decode_entry_plans` replaces
  `decode_frames`. vesper-hybrid's swap row is renamed to `entry_plans`.
  The export stays `frame_plans_words`, so the binding is unchanged.

### Results (Linux x86_64, release)

| Run | Compiles | Hook asked / answered | Differ | Rust | Hooked | In the hook |
|---|--:|--:|--:|--:|--:|--:|
| `NATIVE_WALK_FRAME_STEP=1`, 4 entries | 1,966 | 1,980 / 1,838 | **0** | 725.6 s | 743.8 s (**1.025x**) | 17.2 s |
| default | 614 | 622 / 580 | **0** | 63.5 s | 64.1 s (1.011x) | 1.5 s |

Divergence check: flipping the decoded register-status bit locally gives
115 differing compiles of 587 on the default run, so the compiles really
use the Vesper conventions. (The flip isn't committed.) The ratios are
single-run sums, not ABBA CIs.

**Rust lines deleted:** 0. `NativeAbi::plan` stays as the fallback. It
also serves `native_export` and the compiles the hook declines.

## Part 4b: instance discovery from Vesper (#1509)

### Changes

- **`@vesper/native-walk`**: for each instance, `frames_bytes` now also
  writes its source function (`I <name>`) and type variables (`V <var>
  <index>`). A task that plans every instance ends with a `W` line. It holds
  the variables' type table as decimal words, in check-layout's `Out`
  stream (`1`, the names, the types).
- **`native::EntryPlans`** gains `instances`, the `InstanceMap` of the
  compile.
- **`src/native.rs`**: when the `entry_plans` hook is installed,
  `compile_entry` lowers the world first, asks the hook, and takes the
  instances from its answer in place of `instances_for_entry`. That call
  lowered the whole world a second time. A declined answer, or one with no
  instances, falls back to `instances_for_entry`. Without the hook, the
  order is unchanged.
- **`tests/common/check_words.rs`**: `decode_type_table`.
  `tests/common/native_walk_words.rs`: `decode_entry_plans` reads the `I`,
  `V` and `W` lines.

### Results (Linux x86_64, release)

| Run | Compiles | Hook asked / answered | Differ | Rust | Hooked | In the hook |
|---|--:|--:|--:|--:|--:|--:|
| `NATIVE_WALK_FRAME_STEP=1`, 4 entries | 1,966 | 1,980 / 1,838 | **0** | 726.0 s | 723.4 s (**0.996x**) | 16.6 s |
| default | 614 | 622 / 580 | **0** | 59.8 s | 59.0 s (0.986x) | 1.5 s |

The hooked compiles are now slightly faster than Rust's. The hook's cost
(encoding, the package, decoding) is less than the second lowering that
`instances_for_entry` did. Divergence check: setting every decoded
variable to `i8` locally gives 17 differing compiles (and 4 changed
outcomes) of 610 on the default run, so the compiles use the Vesper
instances. (That change isn't committed.) The ratios are single-run sums,
not ABBA CIs.

**Rust lines deleted:** 0. `instances_for_entry` stays as the fallback
(trace hosts, `native_export`, declined compiles) and is also used by
`capability_abi_functions` and the library path.

### What is left

- Trace hosts (`trace_ok`): `@vesper/codegen-calls`' `check_entry` refuses
  them.
- `native_export` (library objects) still discovers, plans conventions and
  plans frames in Rust.
- With those covered, `instances_for_roots`, `NativeAbi::plan` and
  `frame_plan` can go behind a flip gate. The emission (`Codegen`, the
  graph walk) is still Rust, under the family hooks.

## Part 5: trace hosts and library exports (#2056)

The last two kinds of native compile that still planned in Rust now ask
the package too.

- **Trace hosts.** `compile_entry` asks the `entry_plans` hook for every
  compile, and passes `trace_ok`. The calls task carries a flags word
  (bit 0 optimize, bit 1 trace host). The package checks `trace_ok`
  itself (`trace_entry_ok`). It keeps the entry and every
  `trace_host_direct` instance in discovery (`discover_traced`), gives
  them the trace flag (`trace=1`), and seeds each bare `Trace` with the
  `native-trace-host` identity.
- **Library exports.** `native_export::compile_exports` asks a new
  `export_plans` hook with the export roots. One library task holds the
  first export with flag 4 and the others with flag 8. The package walks
  from all roots at once (`discover_roots`). Every export is a boundary
  of the conventions (`declare_roots`), and the package refuses an export
  that is not a standalone instance, as Rust does.

### Results (Linux x86_64, release)

| Test | Builds | Differ | Rust | Hooked |
|---|--:|--:|--:|--:|
| `the_entry_plans_swap_matches_rust_on_trace_hosts` (trace fixtures, three inline shapes, Beacon corpus worlds, up to 4 entries, opt off and on) | 42 objects, 76 plans with the trace flag | **0** | 145 ms | 140 ms |
| `the_export_plans_swap_matches_rust` (default step, 4 exports per world, opt off and on) | 318 libraries, 292 of 320 asks answered | **0** | 7.68 s | 7.84 s (1.02x) |

Divergence check (local, not committed): the identity `native-trace-hosx`
makes 40 of 42 trace-host objects differ. Planning conventions with only
the first export as the boundary makes 5 libraries differ. So both paths
use the Vesper plans. The ratios are single-run sums, not ABBA CIs.

**Rust lines deleted:** 0. The fallbacks stay for declined compiles until
the flip gate.

### What is left

- The flip gate for `entry_plans` and `export_plans`. After that,
  `instances_for_roots`, `NativeAbi::plan` and `frame_plan` go, except
  for the callers outside the walk (`capability_abi_functions`).

## Flip gate (#2056)

`scripts/ci/flip-gate/native-walk.json` gates the `entry_plans` and
`export_plans` swaps. The differential is the three swap tests. The ABBA
workloads come from the `flip_gate` test of
tests/self_host_native_walk_frame.rs. They time the same entry compiles
(the trace hosts included) and library builds end to end, with the package
at the hook against without it.

`aspect ci flip-gate -- native-walk` (Linux x86_64, `-C target-cpu=native`):

| Check | Result |
|---|---|
| Tests | 1/1 pass |
| Differential | 0 differences on 978 cases |
| ABBA `entries` (114 compiles an arm) | 1.00x [0.98x, 1.03x] n=12 |
| ABBA `exports` (26 libraries an arm) | 0.99x [0.93x, 1.03x] n=12 |
| Verdict | **PASS** |

Both swaps join the hybrid's `gated` set. **Rust lines deleted:** 0. This is
a compiler module, so `instances_for_roots`, `NativeAbi::plan` and
`frame_plan` go when the Rust compiler retires, and nothing goes at this
flip.

## Part 6a: the block plan (#2094)

Part 6 ports the walk's emission (`Codegen`), one decision at a time, the
way parts 3 to 5 ported its setup. The first decision `ir_graph_at` makes
for a graph is its block plan, now `native::block_plan` behind the new
`block_plan` swap hook (`codegen_swap::BlockPlanHook`). The plan has three
parts:

- the order the blocks are lowered in: petgraph's topological order of the
  reachable blocks, or the reverse postorder from the entry when there is a
  loop, then the unreachable blocks in index order;
- the blocks on a cycle;
- the blocks sealed as soon as they are lowered. A block is sealed early
  when it is not the entry, not on a cycle, and every predecessor came
  before it.

The input is each block's successor list (`native::block_targets`).

`@vesper/native-walk` answers it in packages/vesper-codegen-walk/src/blocks.ves
(`native_walk_block_plan_bytes`). The order has to be petgraph 0.8's exactly,
so the port follows `GraphMap` closely:

- nodes are kept in insertion order, and each adjacency list in edge
  insertion order;
- a repeated edge is ignored;
- a self loop gets only an outgoing entry, but it still counts as its own
  predecessor;
- `remove_node` does swap removals on the node list and on each
  neighbour's list, and these reorder both before `toposort` runs.

The hybrid driver installs the package at the hook: `block_plan` in
crates/vesper-hybrid/src/swap/codegen_walk.rs, an open row.

tests/self_host_native_walk_blocks.rs:

| Check | Result |
|---|---|
| Plans, Rust against Vesper | 0 of 5,024 graphs differ: the 1,024 graphs the walk lowers in the sampled corpus compiles, plus 4,000 seeded random graphs (2,728 with loops, 3,583 with unreachable blocks) |
| Swap, CLIF and objects | 0 of 82 compiles differ (1,024 graphs planned by Vesper, 0 declined). A perturbed plan changed 51 of them, so the hook is live |
| ABBA, plans alone (encoding included) | 1.16x [1.12x, 1.19x] |
| ABBA, whole optimized compiles | 1.01x [0.99x, 1.03x] |

**Rust lines deleted:** 0. `block_plan` goes when the Rust compiler
retires, and the petgraph dependency goes with it.

Next (6b and later): the other per-graph plans `ir_graph_at` makes before
it dispatches instructions (split sources, homes, early binds, placed
writes). After those, the emission skeleton with Vesper driving each graph,
then the JIT and object driver steps (G3).

## Unoptimized bodies in the `body` hook (#2094)

The `body` hook (`codegen_swap::body`) lets @vesper/codegen-scalar-emit
emit a whole plain function. Part 6b (#2253) added direct calls, and parts
6c to 6g widened the plan's instructions. This change opens the hook to
unoptimized builds, which are the largest group the frame gate turned
away. Counted over half the corpus in both modes, at 6b:

| Gate reason | Functions |
|---|---|
| unoptimized | 217 |
| struct result through a result pointer | about 120 |
| failure record with capabilities | 62 |
| generic instance | 36 |
| small register results | about 30 |

On that path `ir_graph_at` keeps no local in a register. The emitter
follows it:

- Every local, parameters included, gets its value stored to a fresh,
  zeroed stack slot (one word, two for `i128`) right after its variable is declared. The
  slot's address goes in a variable.
- A read loads through that address.
- A bind stores once, and an assignment stores twice, the second time
  after `coerce` to the local's kind (`def_local`, then `store`). Each
  local's kind is now in the plan, after its entry.
- A graph of at most `FULL_REFRESH_VALUES` (256) values reads every value
  from its register before each instruction and each terminator, not just
  the operands.

crates/vesper-codegen-swap-test/tests/self_host_native_walk_body.rs, full
corpus (`CODEGEN_SCALAR_STEP=1`):

| Check | Result |
|---|---|
| CLIF, objects and JIT runs | 0 differences over 952 functions (306 worlds, both modes, 82 runs) |
| Functions emitted from Vesper | 405, up from 140 on main; 96 of them make 172 direct calls |

**Rust lines deleted:** 0. Next are struct results through a result
pointer (1,350 functions in the port libraries), then failure records
(1,215), pointer locals (509) and generics.

## Struct results in the `body` hook (#2411)

A function that returns a struct or tuple gets a result pointer (`sret`)
as its first parameter. The frame gate turned every such function away.
This change lets the `body` hook emit the ones whose result is a product
of scalar fields.

The Rust hook now appends two things to the request after the callees:

- the result pointer's value, or `NO_VALUE` when the result is in
  registers;
- the values that `aggregates::plan` builds in that pointer
  (`Home::Result`), by their index in the graph.

On the optimize path, a body declines when any value has another home
(a field of a parent or a written place) or a local is split.

The plan (`body.ves` in @vesper/codegen-scalar) gives a struct or tuple
whose fields are all scalars a lane of its own (`LANE_PRODUCT`), whose
register holds a pointer. Such a value may only be built (`INST_DATA`,
from `Op::Data` or `Op::Tuple`) and returned (`TERM_RETURN_SRET`). Any
other instruction, block parameter, jump argument or match scrutinee of
that lane leaves the body to the walk. So do nested aggregates and enums.

The emitter follows `Codegen::data_into`:

1. Every field's value goes in a temporary.
2. The storage is chosen. A result home writes straight into the result
   pointer. Otherwise the optimize path takes a fresh slot with no
   zeroing (`alloc_uninit`), and the unoptimized path takes a slot zeroed
   a word at a time (`alloc`).
3. On the optimize path, the words that no field overwrites whole are
   zeroed. An `f32` field writes only the low half of its word.
4. Each field is read back, offset (`iadd_imm`, even at 0), coerced to
   its type and stored.

A struct result that was not built in the result pointer is copied there
a word at a time on return. The function then returns the word 0.

@vesper/native-lib declares no result pointer in its signatures, so its
test refuses a library whose function returns a struct.

crates/vesper-codegen-swap-test/tests/self_host_native_walk_body.rs, full
corpus (`CODEGEN_SCALAR_STEP=1`):

| Check | Result |
|---|---|
| CLIF, objects and JIT runs | 0 differences over 952 functions (306 worlds, both modes, 82 runs) |
| Functions emitted from Vesper | 446; 21 of them return a struct, which the hook turned away before |
| New `struct_results` test | 0 differences; 10 struct-result bodies from Vesper over both modes |

**Rust lines deleted:** 0. The larger share of struct results reads
struct parameters or fields, builds nested aggregates, or joins structs
at block parameters. Those come next, then pointer locals and generics.
