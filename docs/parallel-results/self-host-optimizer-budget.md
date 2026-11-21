# Optimizer time on large programs (SH4 lane 5, #1279)

> Self-hosting wave SH4, lane 5. Prompt:
> [05-optimizer-budget.md](../agent-prompts/self-host-sh4/05-optimizer-budget.md).
> Base: `main` at e73cfd6 (2026-11-21). The owner chose option A: a policy
> around the unchanged `ir_opt::optimize`, on the native paths only.

## Result

On the six SH2 checker packages that have merged, compiled as one program,
the policy cuts `ir_opt::optimize` from 54.6 s to 5.2 s (10.5x) and the whole
`vesper build --lib` from 111.5 s to 64.7 s (1.72x). The library object is
byte-identical with the policy on and off. Over the corpus, the policy's
program equals `ir_opt::optimize`'s for every world, so run-time code, and
with it run-time Ir, does not change for any program.

The whole build does not reach 2x because what is left is mostly not the
optimizer: with the optimizer at zero the build would take about 59.5 s
(1.87x). The remaining time is the checker, chiefly `World::mode`
(gap 1 below).

## What changed

- **`src/ir_opt_policy.rs` (new).** `ir_opt_policy::optimize` is a drop-in
  for `ir_opt::optimize` that gives the same program and the same count:
  1. **Screen.** `ir_opt::optimize` runs one egglog program per block that
     has a scalar sink read of a local, and in that program a read can only
     be replaced through an earlier write of its place or an earlier sink read
     of it from a shared state (state terms in a block are a chain from one
     `Start`, so no two are equal). `block_may_improve` checks for exactly
     that, so a block that fails it cannot lose a read. It is a necessary
     condition, not a heuristic. On the combined program 1,231 of 20,106
     blocks pass it, and 1,302 of 1,845 functions have no block that does.
  2. **Surrogates.** A function with candidate blocks goes through the
     unchanged `ir_opt::optimize` as a surrogate: a renamed copy that holds
     only its candidate blocks, each ending in a jump to itself whose
     arguments are its sink reads, and only the values those blocks name.
     The pass rewrites terminator operands with its replacements, so the
     jump arguments say what each read was replaced by. A block's egglog
     program depends only on that block, and a value it never names is a
     `Ref` node nothing points to, so the replacements are the ones the pass
     makes in the real function. The policy then removes the replaced reads
     and rewrites operands in the real function, as the pass does after
     saturation. (The pass's own `operands` helper is private, so the policy
     mirrors it; the corpus test checks the result.) The renamed surrogate is
     skipped by `fip_reuse_pass`, and its validation errors are ignored.
  3. **Cache (opt-in).** Saturation does not depend on the world, so its
     result can be cached in-process by a 128-bit content hash of the
     function, and a hit skips the surrogate. The cache is off by default
     (`VESPER_OPT_POLICY_CACHE=on` or `ir_opt_policy::set_cache` turns it
     on): with it on, a compilation's work and allocations depend on what
     the process compiled before, and tests/dyn_trait.rs
     `objects_allocate_nothing`, which compares the allocations of two
     compilations, failed (21,257 vs 15,194). The numbers below are with the
     cache off, except the "second call" row.
  4. **The rest of the pass, unchanged.** Every function then gets
     `fip_reuse_pass`, `ir::thread_tokens` and `ir_check::validate`, exactly
     as `ir_opt::optimize` runs them after saturation.
  5. **Threads.** Every step of the pass works on one function at a time, so
     surrogate saturation and validation run on contiguous parts of the
     program on up to 8 scoped threads (64 MiB stacks, like the CLI thread).
     `VESPER_OPT_POLICY_THREADS=n` sets the count. Results are combined in
     name order, so the output does not depend on the thread count.
- **Native call site.** The default `opt` path of `src/ir_swap.rs` (#1259),
  which `src/native.rs`, `src/native_export.rs` and `src/native_session.rs`
  call, now calls `ir_opt_policy::optimize` instead of `ir_opt::optimize`
  (one line). An installed `opt` hook still replaces it. The verified backend (`src/verified_codegen.rs`,
  `verified_codegen/coverage.rs`), the wasm backend, the evaluator
  (`src/runtime.rs`) and `vesper emit --emit ir` still call
  `ir_opt::optimize` directly.
- **`VESPER_OPT_POLICY=off`** makes the native paths call `ir_opt::optimize`
  directly, for measuring.
- **`tests/optimizer_budget.rs` (new)**, with its BUILD.bazel data and
  timeout entries:
  - `policy_matches_optimize_on_the_corpus`: every corpus world's native
    input, through `ir_opt::optimize` and through the policy twice (the
    second call is served by the cache). The programs must be `Debug`-equal,
    the counts equal, and both must accept or both reject.
    The same test checks that no screened-out function loses an instruction
    under `ir_opt::optimize`.
  - `profile_package` (ignored): the per-function profile and the policy's
    phase timings for one package (`OPTIMIZER_BUDGET_PACKAGE`).

  Release builds check every world; debug builds every 12th
  (`OPTIMIZER_BUDGET_STEP=n` overrides it).

## Numbers

Measured on the 4-core Linux cloud host with a release CLI. The combined
program is a wrapper package whose `check_all(in input: Str) -> u64` calls
every `in input: Str` entry of the six checker packages on `main`
(check-data, check-core-typing, check-dyn, check-fip, check-names-packages,
check-regions-seqs; the other six SH2 packages are still in open PRs). It
has 1,845 native functions and 20,106 blocks.

### The whole build

| `vesper build combined --lib` | Wall time |
|---|---:|
| policy off | 111.5 s (110.7 s on a second run) |
| policy on | 64.7 s |
| speed-up | 1.72x |

The two library objects compare equal with `cmp`.

### The optimizer alone (`profile_package`, release test binary)

| Version | `ir_opt::optimize` time | |
|---|---:|---|
| unchanged pass, whole program | 54.6 s | (50.7-57.1 s over five runs) |
| screen whole functions only | 49.9 s | the functions that improve hold 92% of the time |
| + surrogates of the candidate blocks | 19.7 s | validation is now 11 s of it |
| + surrogates name only their blocks' values | 14.9 s | single-threaded: 3.7x |
| + parts on 4 threads (final) | **5.2 s** | 10.5x |
| final, second call with the cache on | 3.7 s | |

Phases of the final cold call: screen and hash 0.11 s, surrogate saturation
1.23 s, `fip_reuse_pass` 0 s (not a fip program), `thread_tokens` 3 ms,
`validate` 3.84 s.

Profile of the unchanged pass, one function per call (53.5 s in total):

| Functions | Count | Time |
|---|---:|---:|
| screened out (no candidate block) | 1,302 | 4.31 s |
| saturated, lost a read | 434 | 47.46 s |
| saturated, lost nothing | 109 | 1.73 s |

1,017 reads are removed. The slowest functions are large ones with a few
candidate blocks, for example `vesper__front__recover__parse_1recovery_1tables`
(276 blocks, 1,150 values, 16 candidate blocks, 3.8 s) and
`vesper__check_2dyn__fnobj__coerce` (148 blocks, 1,212 values, 18 candidate
blocks, 3.2 s). The pass builds an egglog program for nearly every block, and
every block's program has a `Ref` node for each of the function's values.

### The corpus (`policy_matches_optimize_on_the_corpus`, release)

287 worlds, 15,812 functions: 11,290 screened out, 3,458 lost at least one
read. Every program is `Debug`-equal to `ir_opt::optimize`'s and every count
equal, on the first call and on the cached second call; 4,522 functions were
served from the cache on the second calls (the cache is shared across worlds,
so functions like the std preludes hit it on the first call too). No world is
rejected by either. The run took 497 s (before the screen check was folded into this test; it then ran as a second test).

### Run-time cost

None. The policy's programs equal the pass's over the whole corpus, and the
combined library is byte-identical, so emitted code is unchanged: no program
gets slower and run-time Ir of the ports does not move. The port harness on
this branch runs with the policy on (the default).

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `src/ir_opt.rs`
(`ir_opt::optimize` included), `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**` is touched. The verified backend keeps calling
`ir_opt::optimize` directly. The bootstrap-stage1 workflow run on this branch
is linked from the PR.

## Compiler gaps

1. **`World::mode` dominates what is left of the build.** Stack samples of
   the policy-on `vesper build combined --lib` land mostly in
   `check::World::mode` and its recursive `mode::visit`, called from
   `ir_check::Flow::mode`/`merge` (both in the checker's
   `check_core` -> `ir_check::validate` and in the optimizer's validation)
   and from `Checker::mode`. Validation is 3.8 s of the policy's 5.2 s even
   on 4 threads. Memoizing a type's mode per world (it is a pure function of
   the type and the world) would speed up checking and validation alike.
   `src/check.rs` is outside this lane. Repro: the wrapper package above,
   `gdb -p <pid> -batch -ex "thread apply all bt 200"` sampling.
2. **The pass does work it cannot use.** `ir_opt::optimize` runs egglog for
   every block with a scalar sink read (94% of them cannot change) and puts
   a `Ref` for every function value into every block's program
   (O(blocks x values)). Fixing it inside the pass is option B (a Stage 1
   re-lock) or SH3 lane 9's Vesper `ir_opt`. Minimal repro: any function with
   a few hundred blocks that each read a local once, for example
   `fn f(x: u64) -> u64 { let mut a = x; if a > 1 { a = a + 1 } ... }`
   repeated; `profile_package` prints per-function times.
3. **`ir_opt`'s `operands` is private**, so the policy mirrors it. If a new
   `Op` gains operands, both must change; the corpus test catches a miss
   only when a corpus program replaces a read used by that op.
4. **Validation errors are reported per part.** On a program that fails
   validation, the policy's error lists the same diagnostics as the pass's,
   grouped by part, so the text can differ in order. Accepted programs are
   unaffected.

## What is left

- The 2x target for the whole build needs the checker (gap 1); the optimizer
  part of it is at 10.5x.
- Re-measure with all twelve SH2 checker packages once the six open ones
  merge (effects #1238, tensor-types #1230, layout and ir-check #1232, pact,
  loops-atomics).
- The cache is opt-in and lives for one process. A disk cache keyed by the same content
  hash would make repeat builds skip saturation (1.2 s here); validation
  would still run.
