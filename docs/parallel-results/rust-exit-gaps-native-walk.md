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
