# P3b gaps lane 3: bounds facts

Claim #1088. Prompt:
[03-bounds-facts.md](../agent-prompts/p3b-gaps/03-bounds-facts.md). PR
#1118. The base is `9305b24` (origin/main with the wave plan #1098), and
the branch is merged with origin/main `f9005bb` (it includes #1076,
#1092, #1107, #1110 and #1113).

All four parts are done:

1. Wider loop guards.
2. Dominated checks dropped.
3. Slice headers read once.
4. Hoisting in functions that own boxes.

The work also adds a guard cost model, which the port measurements showed
was needed. Everything runs on the native optimize path only: the pass
runs after `ir_opt::optimize`, inlining and drop elaboration.

## What changed

### 1. Guards for affine indices, `..=`, inner and inlined loops

`SeqOp::BoundsGuard` now carries `{ inclusive, form: IndexForm }`. A
guarded index is `±i + Σplus − Σminus`, where `i` is the loop index and
each term is loop-invariant: a value defined before the loop, an in-loop
integer constant, an invariant local, or a local bound once to such a sum.

- **Covered forms.** `s[i + k]`, `s[i - k]`, `s[k - i]`, and the Bench
  `add_rank` form `s[(max - j) + rank]` are all covered, as are `..` and
  `..=` ranges. The limits are four terms and a depth of six operators.
- **Guard arguments.** The guard takes `[lo, hi, s, p1..pm, n1..nk, ok?]`.
- **Guard semantics.**
  - The evaluator computes both range ends exactly in u128, and a sum past
    u64::MAX fails the guard.
  - Natively, the sums at the largest loop index use `uadd_overflow`.
    Because the index is monotone in `i`, only its two ends need checking.
  - A plain `s[i]` costs one compare (`last < len`).
- **When a guard fails**, the original checked loop runs unchanged. A short
  sequence faults at the same iteration with the same prior effects, and
  checked arithmetic in an index still raises `ArithOverflow`.
- **One guard per receiver.** When a loop has several receivers, each
  guard takes the previous result as `ok`, and the guard tokens are chained
  (E0411).
- **Which loops.** Loops are found as natural loops from dominators,
  versioned inner-first. Loops inlined from callees (`$inl<N>` slots) are
  recognized too.

### 2. Dominated checks

`drop_dominated_checks` makes an access unchecked when it meets three
conditions:

- A checked Get or Set dominates it, either in a dominating block or
  earlier in the same block.
- That access is on the same place, and the place is stable across the
  function.
- It used the same index value (following single-binding locals), or a
  constant at least as large.

### 3. Slice and sequence headers read once

`bounds_hoist::stable_views` selects these parameters:

- Every `in` or `inout` slice parameter. Slices are second-class and never
  change their view, even when lent `inout` to a callee.
- Sequence parameters that the function never resizes, rebinds, or lends
  other than `in`.

Native codegen reads them once at entry and loads their header words with
`readonly + notrap + can_move`. Cranelift then shares those loads and LICM
moves them out of loops, so a store through one slice no longer reloads
another slice's header. A callee that receives the slice `inout` can write
its elements but cannot change its view, so the elements are still read
after the call.

Loop header hoists (#990) also apply to versioned loops now. A loop and its
fast copy share their preheader and value ids, so the reads' users span
both copies, and before this change neither copy was hoisted. That matters
since main's `inout` slice inlining (#1089): an inlined kernel's slices are
caller locals, not parameters. Without loop hoists the Resonance repro went
from 247M back to 521M after the merge.

### 4. Functions that own boxes

#1076 merged, so `version` and `header_hoists` no longer refuse functions
that have box drop bookkeeping:

- The fast copy reclaims the loop's boxes at the same instruction points
  (`box_drops_*` copied with block parameters remapped).
- A sequence at or inside a place whose box a drop in the loop reclaims is
  never hoisted.
- `@fip` reuse slots are still refused.

### Cost model

The first port sweep found regressions, so versioning now has three limits:

- **Short constant trips.** A loop with a constant trip count is versioned
  only when trips × unchecked accesses ≥ 7 × guards. A guard is about 20
  instructions and an access check about 3. Kinetic's 3-axis
  `sweep_overlaps`, with four index forms over three iterations, had made
  `sap` 83% slower.
- **Changing inner ranges.** An inner loop whose range changes with the
  loop around it is versioned only when it drops more checks than it adds
  guards. Examples are a triangle `for j in i + 1..n`, as in Ignis's
  `check_pairs`, and a per-pass use list. An inner loop with a fixed range,
  such as a matrix row, is versioned like a top-level loop.
- **Short ranges at run time.** Natively, a range shorter than 4 takes the
  checked loop without evaluating the guard, and a guard after a failed one
  is skipped. The guard may always say no.

## Stage 1 guard evidence

- **Forbidden paths.** Nothing under `src/verified_codegen/**`, `src/ir.rs`,
  `ir_opt::optimize`, `lean/**`, `bootstrap/**` or
  `packages/vesper-compiler/**` changed.
- **Bootstrap workflow.** The `bootstrap-stage1` workflow passed on the
  branch: [run 44448346023 on `276356c`](https://github.com/robot-head/vesper/actions/runs/44448346023),
  and the "Stage 1 reproducibility and cross-backend ratchet" job on
  `cac9154`. Its digests equal `bootstrap/stage1.lock`. The final head is
  re-run before merging.

## Measurements

Instruction counts are callgrind Ir on the cloud container (Linux x86_64).
Ports are built with `VESPER_PORT_COMPILER` set to a compiler from origin/main
`f9005bb` ("main") or from this branch (the final head), with the committed
port sources and release builds.

### Ports

| Workload | main | branch | change | Rust |
|---|--:|--:|--:|--:|
| Bench estimators (20 batches of 32) | 6,559M | 4,586M | −30.1% | 3,313M |
| Bench statistics (20,000 samples) | 32.20M | 31.31M | −2.8% | 7.59M |
| Resonance kernel (2,000 blocks) | 4,753M | 4,428M | −6.8% | |
| Resonance graph (400 blocks) | 1,954M | 1,752M | −10.3% | |
| Prism H3 decode (6 batches) | 220.2M | 209.3M | −5.0% | |
| Prism H3 encode | 105.0M | 100.6M | −4.2% | |
| Prism H3 frames | 82.80M | 82.77M | −0.0% | |
| Prism H3 messages | 130.2M | 128.9M | −1.0% | |
| Kinetic colorer | 14.84M | 10.40M | −29.9% | 7.21M |
| Kinetic multi-colorer | 9.03M | 6.46M | −28.5% | 4.04M |
| Kinetic lbvh | 35.79M | 27.67M | −22.7% | 8.63M |
| Kinetic sap | 40.45M | 34.32M | −15.1% | 14.59M |
| Kinetic scene | 91.97M | 90.96M | −1.1% | 70.40M |
| Kinetic pile-lbvh | 134.2M | 108.5M | −19.2% | 58.69M |
| Kinetic morton, fastmath | 1.39M, 1.34M | same | 0.0% | |
| Ignis build (256 passes, 20 ops) | 22.38M | 22.55M | +0.7% | |
| Ignis plan | 355.0M | 342.0M | −3.7% | |
| Ignis lower | 35.13M | 34.03M | −3.1% | |
| BBR ack path (20,000 steps) | 46.61M | 46.61M | 0.0% (byte-identical port object) | |

How to read the table:

- **Rust column.** It is the seed build's arm, from the same binary
  (`*_ONLY=rust`, or `one rust`), and includes process start-up. Resonance's
  Rust arm runs a different amount of work under `ABBA_ONLY`, so it is left
  out.
- **Commands.**
  - Bench: `BENCH_ABBA_ONLY=port BENCH_ABBA_BATCHES=20` (estimators) and
    `BENCH_ABBA_WORKLOAD=statistics BENCH_ABBA_SAMPLES=20000`.
  - Resonance: `ABBA_ONLY=port ABBA_WORKLOAD=kernel|graph
    ABBA_KERNEL_BLOCKS=2000 ABBA_GRAPH_BLOCKS=400`.
  - Prism H3: `abba once <workload> 6`.
  - Kinetic: `abba one port <workload> 1`.
  - Ignis: `abba profile <workload> 256 20`.
  - BBR: `ack_path_abba measure 20000`.
- **Bench and Resonance (the two gaps the prompt names).**
  - Bench estimators went from 1.98x to 1.38x of the Rust instruction
    count with committed sources. The subset-sum `add_rank` loop is
    guarded (`s[(max - j) + rank]`).
  - With the counted-loop rewrite of `add_rank` (the patch below), the
    inner loop runs 16 instructions per iteration. The local rewrite
    measured 255.2M → 153.0M on the extracted repro.
- **Ignis build.** It is still +0.7% (+0.17M): `graph_add_pass`'s helper
  grew from 178 to 260 instructions per call before the triangle rule, and
  most of that is gone.

### Repros

| Repro | main | branch |
|---|--:|--:|
| Resonance header repro: two `inout` slices written in turn, 1M calls of 16 (`tests/native_slice_headers.rs` shape) | 693.3M | 242.3M |
| Resonance `k_render` kernel (from PR #1082) | 1,865M (base `9305b24`) | 1,006M |
| Bench `add_rank`, counted-loop form, 200k × 1001 | 5,006M (base `9305b24`) | 3,210M |
| Kinetic `pair_total` + `sweep_overlaps` | 18.63M | 14.57M |
| Dominated checks (`dom.ves`) | 82.26M (base) | 51.26M |

- **Results.** Every repro gives the same result in the evaluator, the
  JIT, and the built executables.
- **Resonance header repro baseline.** Main moved from 550M to 693M when
  the merge brought in `inout` slice inlining. The branch before the merge
  measured 247M.

### Atomics workers

`cargo test --release --test scoped_workers -- --ignored --nocapture
measure` (JIT, sum of 16,777,216 u64, three rounds, wall clock on 4 vCPUs):

| | Vesper 1 thread | Vesper 4 workers | Rust 4 threads | 4-way ratio (Vesper / Rust) |
|---|--:|--:|--:|--:|
| main | 32.1-34.2 ms | 14.0-16.6 ms | 3.9-4.7 ms | 2.98-4.22 |
| branch | 20.4-23.5 ms | 5.6-6.1 ms | 3.9-4.5 ms | 1.24-1.56 |

The single-thread sum is about 35% faster and the four workers about 2.6x
faster. The worker's summing loop over its slice chunk is the kind this lane
guards and reads headers for, but I did not trace which change accounts for
it. Wall clock moves by up to 30% between
runs on this host.

The Bench `add_rank` counted-loop rewrite used for the local measurement is
in `/mnt/project-files/p3b-gaps-bounds-facts/bench-add-rank-for.patch`
(port sources are not committed by this lane).

## Tests

- `tests/native_bounds_facts.rs` (12 tests) covers:
  - affine forms, reverse and `..=` loops, and failing guards at the same
    iteration;
  - checked index arithmetic still raising;
  - inner loops and dominated checks;
  - resized or total accesses establishing nothing;
  - overflowing guard sums;
  - a loop that owns boxes (box counts balance in both JIT modes);
  - short constant loops and triangle loops not being versioned.
- `tests/native_slice_headers.rs` (3 tests) covers the Resonance repro,
  slices lent to callees, and resized or lent sequences.
- Each test checks the plan (guard, unchecked-access and entry-view
  counts). It then checks that these agree: the evaluator, both JIT modes,
  x86-64 and AArch64 objects, and a linked executable.
- `tests/native_hoist_calls.rs`: a loop that calls now gets entry hoists
  for its two slice receivers.

## Left

- **Ignis build** is still +0.7%: its short per-call loops pay for
  versioning that they rarely use.
- **No profile feedback.** The cost model is static. A loop of unknown
  length with as many guards as accesses at the top level is still
  versioned.
- **Inlined slices** are hoisted per loop, not per call. An inlined slice
  is a caller local, so it does not get the read-only entry view a
  parameter gets.
- **Guard forms not covered:** indices through multiplication
  (`i * c + j`, guarded only in the inner loop), and loops with more than
  one induction variable.
