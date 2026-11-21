# Compiler capacity (self-hosting SH3, lane 7)

Claim #1246. Branch `claude/project-thread-v0hm5j`, stacked on #1231
(head `1e0ca05`, origin/main `c8ac321` when the lane started). #1231 merged as
`ec19a4c`, and main was merged into this branch.

The self-hosted checker links every SH2 checker package into one program.
This lane removes the fixed native instance cap, widens word entries, and
measures the twelve checker packages compiled together. Combining them
found a bug that blocked the measurement: past 1 MiB of assembled source,
the builtin preludes parsed to nothing.

## What changed

1. **No instance cap** (`src/native.rs`, `src/native_export.rs`). The fixed
   `MAX_NATIVE_INSTANCES = 4096` is gone, so a program has as many
   instances as it reaches. Only polymorphic recursion makes that number
   unbounded, and it does so by growing the type arguments. Native
   monomorphization now stops an instance whose type arguments pass
   `MAX_NATIVE_INSTANCE_TYPE_SIZE = 256` type nodes. That is the checker's
   `pact_frontend::mono::MAX_TYPE_SIZE`. The error names the cycle:

   ```text
   native monomorphization of 'grow' creates unboundedly many instances
   (polymorphic recursion through grow -> hop -> grow): its type arguments
   passed 256 type nodes
   ```

   The old cap only said "exceeds 4096 function instances", and only after
   building all 4,096 instances. The walk also records the parent of each
   instance so that it can report the chain. `native::instance_count`
   exposes the count.

   `compile_exports` used to call `instances` once per export, and each call
   lowered the whole world again, so the cost was exports × program size.
   It now walks all exports once (`instances_for_roots`).

2. **Word entries take up to eight words** (`native::WordFunction`,
   `MAX_WORD_FUNCTION_ARGS = 8`). Six words travel in registers on SysV
   x86-64, eight on AArch64 and four on Windows x64. The rest go on the
   stack, where Cranelift's default convention and the host's `extern "C"`
   agree. Export entries (`vesper build --lib`, ABI v3) and `@case` test
   rows already took eight and seven words before this change. I checked
   both on the base binary, so neither needed a change. Ports can stop
   packing options into a `flags` word.

3. **Preludes no longer vanish past 1 MiB** (`integer_widths::parse_at`,
   used by the byte-order/bit/checked, float, string, atomics and secret
   preludes, by handle drop glue, and by `@export instance` wrappers). Each
   of these used to parse its generated source behind `base` spaces, so
   that its spans landed after the module's. Once a package's assembled
   module passed the parser's 1 MiB input limit, the padded text parsed to
   an empty module (the `debug_assert` that would say so is off in
   release). Every builtin those preludes declare then went unknown:
   `String::new`, `wrapping_shl_u64`, `wrapping_neg_i64` and others. The
   twelve checker packages together are 1.2 MB of `.ves` (plus `@vesper/ast`
   and `@vesper/front`), and combining them gave 285 errors. `parse_at`
   parses the unpadded source and shifts its spans. It uses the same JSON
   span walk as `pact_frontend::mono`. In debug builds it also runs the
   padded parse whenever that still fits and asserts the two modules are
   equal, so every debug test run checks the two parses agree.

4. **Drop-callee lookup is indexed** (`ffi_lowering::drops::DropCallees`).
   This was the superlinear hot spot. Instance discovery called
   `drop_callees` once per instance, and each call scanned every function
   of the lowered program. With 20,800 functions that was 7 of 9 sampled
   seconds of an unoptimized JIT build. The index is built once per walk.
   `drop_callees` stays as a one-shot wrapper.

The checker's own `pact_frontend::mono::MAX_INSTANCES = 4096` is unchanged.
The same reasoning would apply to it, because its depth (64) and type-size
(256) guards already bound polymorphic recursion. But it counts only
bounded-generic instances per package, nothing measured here comes near it
(the combined set has 2,697 native instances in all), and the file is not
this lane's. The fix is one line once a port hits it.

## Repros (tests/compiler_capacity.rs)

- `more_than_five_thousand_instances_compile`: a generated package of ten
  modules with 520 chained functions each (one file would pass the parser's
  65,536-token limit). It reaches more than 5,000 instances, and the JIT
  result equals the closed form. The base refuses it with "exceeds 4096
  function instances".
- `polymorphic_recursion_reports_its_cycle`: `grow[T]` calls `hop[(T, bool)]`,
  and `hop` calls `grow` back. The checker accepts this and the evaluator
  runs it (`vesper run` prints 7). Native code reports the cycle.
- `word_entries_take_up_to_eight_words`: `f0` to `f8` with weighted sums,
  optimized and not, called through `WordFunction`. `f9` is refused.
- `measure_package` (ignored) prints the check time, the instance count and
  the JIT compile times for one package (`COMPILER_CAPACITY_PACKAGE`).

## Numbers

Measured on the 4-core Linux cloud host with release builds of the CLI,
optimized (the default). "Base" is #1231's head. "After" is this branch.
Each row is a wrapper package whose `check_all(in input: Str) -> u64` calls
every entry of the named checker packages, plus a `main` that calls it on an
empty input. The timings are wall time of `vesper jit` and
`vesper build --lib`, including parse and check, and peak RSS. The packages
come from main plus the open PRs' heads: effects #1238, core-typing #1237,
layout and ir-check #1232, tensor-types #1230, fip #1228, regions-seqs
#1226, pact #1225, loops-atomics #1223.

| Package set | JIT base | JIT after | `--lib` base | `--lib` after | Peak RSS after (JIT / lib) |
|---|---:|---:|---:|---:|---:|
| check-data | 3.81 s | 3.83 s | 4.26 s | 4.05 s | 69 / 81 MB |
| check-core-typing | 13.57 s | 13.73 s | 15.68 s | 15.53 s | 112 / 143 MB |
| check-dyn | 65.95 s | 61.92 s | 75.76 s | 78.86 s | 380 / 467 MB |
| check-effects | 15.73 s | 15.95 s | 18.10 s | 17.65 s | 140 / 174 MB |
| check-fip | 10.02 s | 10.15 s | 10.62 s | 11.39 s | 113 / 136 MB |
| check-layout | 6.02 s | 5.99 s | 6.13 s | 6.21 s | 86 / 101 MB |
| ir-check | 14.81 s | 14.74 s | 16.28 s | 16.20 s | 119 / 146 MB |
| check-loops-atomics | 15.80 s | 16.31 s | 18.24 s | 18.89 s | 142 / 172 MB |
| check-names-packages | 47.44 s | 48.67 s | 56.04 s | 56.47 s | 364 / 448 MB |
| check-pact | 10.98 s | 11.08 s | 11.91 s | 11.81 s | 131 / 157 MB |
| check-regions-seqs | 7.05 s | 7.50 s | 7.52 s | 7.88 s | 136 / 154 MB |
| check-tensor-types | 60.31 s | 59.60 s | 69.45 s | 68.16 s | 414 / 482 MB |
| **sum of the parts** | 271.5 s | 269.5 s | 310.0 s | 313.1 s | |
| **all twelve, one program** | refused (285 errors) | **170.8 s** | refused | **189.1 s** | 886 / 1,050 MB |

The combined program takes 63% of the parts' sum for the JIT build and
60% for the library build, because `@vesper/check-data`, `@vesper/ast`
and `@vesper/front` compile once. Runs of the same binary vary by about 5%,
so the per-package rows are unchanged within noise. The tensor-types and
combined `main` fault at run time on the empty input. That fault comes
after compilation, so the timings still hold.

Phases of the combined program, from `measure_package` in a release test
binary: check 25.9 s, 2,697 native instances (counted in 0.35 s), JIT
`--no-opt` 13.9 s, JIT optimized 122.8 s. Stack samples of the optimized
JIT run (40 samples) show `ir_opt::optimize` in 24, which is egglog's
`run_program` in 18 and `ir_check::validate` in 4. The checker (`check_core`) is in 11, and
`ir_check::validate` for the world in most of those. Cranelift itself is in
3. Package (instances): check-dyn 1,032, check-tensor-types 999,
check-names-packages 933, check-data 133.

Scaling of the generated package (chains of 520 functions per module,
`vesper jit`). `--no-opt` was quadratic before the `DropCallees` index:

| Functions | `--no-opt` before | `--no-opt` after | optimized before | optimized after |
|---:|---:|---:|---:|---:|
| 2,600 | 0.44 s | 0.41 s | 2.15 s | 2.06 s |
| 5,200 | 1.04 s | 0.84 s | 3.10 s | 3.14 s |
| 10,400 | 2.98 s | 1.74 s | 5.72 s | 5.05 s |
| 20,800 | 10.60 s | 3.77 s | 15.27 s | 8.56 s |

("Before" here is this branch without the index. The base binary refuses
everything past 4,096 instances.)

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
prelude modules are equal to the padded parses (asserted in debug), so
checked worlds, and with them every verified artifact, are unchanged. The
bootstrap-stage1 workflow run on the branch is linked from the PR.

## Compiler gaps

1. **Optimization dominates large builds (egglog).** 72% of the combined
   JIT build is `ir_opt::optimize`, mostly egglog `run_program`, which is
   linear but heavy. Repro: `measure_package` on the combined wrapper
   (13.9 s unoptimized, 122.8 s optimized). In a debug compiler this is
   the 11.5 minutes #1231 reported for `@vesper/check-dyn`. That code is
   off limits to this wave (Stage 1 guard), so the fix is an owner call:
   per-function egglog budgets, or skipping saturation for functions the
   inliner leaves untouched.
2. **The checker is 15% of the combined build.** `ir_check::validate` over
   the whole world formats strings in `World::mode` for every place (25.9 s
   check on 1.2 MB of source). Repro: the same wrapper with `vesper check`.
3. **One file holds at most 65,536 tokens** (`crates/vesper-syntax`
   `LexLimits::default`), so a generated package must split its source
   across modules. Packages are unaffected, because the limit applies per
   file.
4. **The parser's 1 MiB limit was reachable through span padding** (fixed
   here). Repro before the fix: a wrapper package depending on check-data,
   core-typing, dyn, effects, fip, layout and ir-check gives `E0201 unknown
   name 'wrapping_shl_u64'` and 76 more errors, while each pair checks.

## What is left

- An owner decision on optimizing large programs (gap 1).
- `pact_frontend::mono::MAX_INSTANCES` can go the same way when a port
  reaches it.
- Combining the hybrid build's checker swaps (SH3 lanes 1-6) into one
  program, once they exist, should rerun this table. The wrapper
  generator is in the PR description.
