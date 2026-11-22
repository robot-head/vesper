# Optimizer build time in debug compilers and the other callers (SH5 lane 9, #1332)

> Self-hosting wave SH5, lane 9. Prompt:
> [09-optimizer-build-time.md](../agent-prompts/self-host-sh5/09-optimizer-build-time.md).
> Stacks on #1312 (SH4 lane 5) at `be6db84`; `main` was `bbda89e` when the
> lane started (2026-11-21). `src/ir_opt.rs` is unchanged.

## Result

With #1312's policy, `ir_opt::optimize` was no longer the 27 minutes and
460 s the SH4 lanes reported. This lane makes saturation 4-5x cheaper again,
so the optimizer now takes **10.0 s on `@vesper/backend-checks` and 16.7 s on
`@vesper/ebpf-backend` in a debug compiler** (20.0 s and 26.6 s with #1312
alone). Under `port-compiler` it takes **2.8 s and 3.6 s**, against 27.8 s
and 37.6 s for the unchanged pass on the same input. That is well under the
2-minute target.

The new screen is close to exact: on both packages every function it sends
to saturation loses at least one read, and on the corpus all but 2 of 4,630
do. What
is left of the optimizer's time is `ir_check::validate` (75-90% of it), which
is lane 8's `World::mode` work, not saturation.

The wasm backend and `vesper emit --emit ir` now call the policy too. Over
the corpus the policy gives their input exactly what `ir_opt::optimize`
gives, so wasm modules and emitted IR are byte-identical.

## What changed

- **`src/ir_opt_policy.rs`**
  - **Reach screen (`block_can_improve`).** In a block's egglog program, a
    sink read `d` of place `p` at state `s` is unioned with `Read(s, p)`.
    The pass has two rules: `Read(Write(s', p, v), p) = v`, and
    `Read(Write(s', q, v), p) = Read(s', p)` when `p` and `q` are disjoint.
    Nothing rewrites a `Read` of a `Barrier`, of a `Start`, or of a write to
    an overlapping other place. So `d` can only become equal to an earlier
    value if the walk back from `s` across disjoint writes ends at a write of
    `p`, or meets a state from which an earlier scalar sink read of `p`
    reaches the same term. The screen follows those walks. It uses the
    pass's own place table and `PlaceGraph`, and it classifies
    token-changing instructions as `Write` or `Barrier` the same way the
    pass does (`op == "="` and a scalar value, or a `Bind` of a name with a
    scalar value). It ignores the 12-round bound and the type filter, so it
    can only keep more blocks than the pass improves, never fewer. A sink
    read with no state token, a walk through a state the block does not
    define, a cycle of states, or two steps from one state (where congruent
    writes could merge states) all keep the block, so the pass still decides
    and reports what it reports. Lane 5's `block_may_improve` runs first as
    the cheap pre-filter.
  - **One surrogate per candidate block.** #1312 built one surrogate per
    function that held all its candidate blocks and every value those blocks
    name. Each block's egglog program had a `Ref` node for each of those
    values. Each candidate block now gets its own surrogate, named
    `<fn>\0ir_opt_policy\0<block, zero-padded>` so that surrogates sort and
    report errors in the pass's order. Its program holds only its own
    values. The replacements of a function's blocks are merged and applied
    as before. This also balances the parallel parts better.
  - `VESPER_OPT_POLICY_SCREEN=basic` restores #1312's policy (screen and
    surrogates) for measuring.
  - `profile_blocks` times each candidate block saturated alone, for this
    report.
- **Call sites.** `src/wasm_backend.rs` (`options.optimize`) and
  `src/main.rs` (`vesper emit --emit ir` without `--no-opt`) call
  `ir_opt_policy::optimize` instead of `ir_opt::optimize`. They are one line
  each. The verified backend (`src/verified_codegen.rs`,
  `verified_codegen/coverage.rs`) and the evaluator (`src/runtime.rs`, not
  this lane's) still call the pass directly.
- **`tests/optimizer_build_time.rs`** (new), with BUILD.bazel data and
  `long` entries:
  - `policy_matches_optimize_on_lowered_worlds`: every corpus world's
    `ir::lower_world` program (the input of wasm and `emit --emit ir`)
    through the pass and through the policy. The serialized JSON (what
    `emit --emit ir` writes), the `Debug` text and the counts must be equal,
    and both must accept or both reject. Lane 5's
    `tests/optimizer_budget.rs` still covers the native input with the new
    screen.
  - `measure_package` (ignored): the policy's phases for one package,
    optionally the unchanged pass with a program equality check
    (`OPTIMIZER_BUILD_TIME_PASS=1`) and the per-block profile
    (`OPTIMIZER_BUILD_TIME_BLOCKS=1`, which also asserts that no block the
    reach screen drops loses a read).

## Numbers

4-core Linux cloud host. The input is `ir_opt_policy::native_input` of the
whole package world (`packages::load`, `check`, `drops::lower_world`, bit
builtins), as on the native paths. The packages come from the open PRs:
`@vesper/backend-checks` from #1319 (935 functions, 5,984 blocks) and
`@vesper/ebpf-backend` from #1308 (1,454 functions, 12,043 blocks). They are
not part of this PR.

### Optimizer time

| | backend-checks | ebpf-backend |
|---|---:|---:|
| unchanged pass, debug (reported by #1319 / #1308) | 1,599 s ¹ | 460 s |
| unchanged pass, `port-compiler` | 27.8 s | 37.6 s |
| #1312's policy, debug | 20.0 s | 26.6 s |
| this lane, debug | **10.0 s** | **16.7 s** |
| #1312's policy, `port-compiler` | 3.5 s | 4.4 s |
| this lane, `port-compiler` | **2.8 s** | **3.6 s** |

¹ `WordFunction::compile(.., true)` in #1319's debug test binary, which
also includes lowering and Cranelift.

Under `port-compiler` the policy's program equals the pass's on both packages
(`OPTIMIZER_BUILD_TIME_PASS=1`), with equal counts (524 and 706 reads).

### Phases (debug compiler)

| | backend-checks, #1312 | backend-checks, now | ebpf-backend, #1312 | ebpf-backend, now |
|---|---:|---:|---:|---:|
| candidate blocks | 618 | 416 | 869 | 598 |
| functions saturated (improved) | 204 (160) | 160 (160) | 348 (268) | 268 (268) |
| saturation | 12.51 s | **2.25 s** | 12.04 s | **2.97 s** |
| `validate` | 7.47 s | 7.71 s | 14.57 s | 13.72 s |
| screen | 9 ms | 21 ms | 19 ms | 34 ms |

The two changes split the saturation gain. In `port-compiler`, the reach
screen drops 202 of 618 blocks on backend-checks and 271 of 869 on
ebpf-backend. Those are exactly the candidate blocks that lose nothing, and
none of them loses a read. Per-block surrogates cut the remaining work
further: summed one-block saturation of the kept blocks is 0.69 s and
0.95 s, and the policy's parallel saturation phase is 0.31 s and 0.36 s.

### Where saturation time goes now

Each egglog run costs about 2-5 ms in `port-compiler` even for a four-instruction
block (`vesper__check_2data__names__Names::spells` block 11: 4
instructions, 5 values, 5 ms). That is the pass's fixed per-run cost
(building `EGraph::default()`, parsing the rules, 12 rounds). The block size
matters much less. Every remaining run removes at least one read, so the
screen cannot drop more without changing output.

### Corpus

Every corpus world (297 worlds, 21,402 functions), `port-compiler`:

- `policy_matches_optimize_on_the_corpus` (lane 5's test, native input, now
  with the reach screen and per-block surrogates): every program
  `Debug`-equal to the pass's, counts equal, none rejected. 16,772 functions
  are screened out, 4,628 improve and 2 are saturated without a change
  (#1312's screen left 3,458 improved and 1,064 saturated for nothing
  on its 287 worlds). 679 s for both of that file's tests.
- `policy_matches_optimize_on_lowered_worlds` (wasm and emit input): every
  program's JSON and `Debug` text equal to the pass's, none rejected. 602 s.

## Tests

- `port-compiler`, every world: the two corpus tests above pass, and
  lane 5's `screen_never_drops_an_improvement` passes.
- `measure_package` with `OPTIMIZER_BUILD_TIME_PASS=1` on both packages
  passes (policy == pass).

## Stage 1 guard

Nothing under `src/verified_codegen/**`, `src/ir.rs`, `src/ir_opt.rs`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed, and no
verified-backend call site moved. The wasm backend is not part of the
Stage 1 lock. The bootstrap-stage1 workflow runs on this branch once #1312
has merged and `main` is merged in, under the CI rule. Its run is linked from
the PR.

## Compiler gaps

1. **`ir_check::validate` is now 75-90% of the optimizer's time.** It takes
   7.7 s and 13.7 s in a debug compiler, and 2.5 s and 3.2 s under
   `port-compiler`. The policy must run it on every function to reject what
   the pass rejects. This is the checker's `World::mode` recursion, which is
   SH5 lane 8's (#1331). Repro:
   `OPTIMIZER_BUILD_TIME_PACKAGE=packages/vesper-ebpf-backend cargo test --test optimizer_build_time measure_package -- --ignored --nocapture`
   prints `validate_micros`.
2. **egglog's fixed cost per run.** The pass builds a fresh e-graph and
   parses its rules for every block, which costs 2-5 ms per block in
   `port-compiler` and roughly 5-8x that in debug. Only `ir_opt.rs` can
   change that, and it is Stage 1 guarded. A debug-only fix outside the guard
   is a dev-profile override that optimizes egglog
   (`[profile.dev.package.egglog] opt-level = 3` and its hot dependencies).
   That is a root `Cargo.toml` change outside this lane, so it is not made
   here.
3. **Load, check and lower dominate the debug build of ebpf-backend**
   (28-29 s against 16.7 s for the optimizer). That is outside this lane.

## What is left

- The SH4 differentials (#1319, #1308) can turn their optimized runs on by
  default. Their JIT path goes through the native `opt` swap, which is the
  policy. That is a change in their own tests, so it is left to those lanes.
- `validate` time (gap 1) is lane 8's.
