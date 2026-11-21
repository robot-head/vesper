# Self-hosting SH3 lane 12: bounds hoisting in Vesper, part 1 (analyses)

Claim #1251. Stacks on #1232 (branched from its head `b90b4ca`; `origin/main`
was `c8ac321`). Part of the [SH3 plan](../agent-prompts/self-host-sh3/README.md).

`src/bounds_hoist.rs` and `src/bounds_hoist/facts.rs` (about 3,000 lines) version
counted loops so their element accesses run unchecked behind one guard,
version loops bounded by a compared end, drop checks that a comparison or a
checked access dominates, and find the sequence headers a loop or the whole
function can read once. This part ports every decision the pass makes. The
rewrite that applies them (loop copies, `SeqOp::Hoisted`, the guards) waits on
lane 1's IR writer (#1259) and is part 2.

## What changed

- **`packages/vesper-bounds-hoist`** (`@vesper/bounds-hoist`, about 2,600
  lines): the function's Place IR as flat tables (`fun.ves`), the graph
  queries (`graph.ves`: reachability, predecessors, Cooper-Harvey-Kennedy
  dominators, natural loops, optionally with `Never`-calling blocks cut), the
  per-instruction rules (`rules.ves`: `stable_place`, `stable`,
  `invariant_local`, `single_binding(s)`, `changes_len`), the bounds facts
  (`facts.ves`: terms in a pool memoized per value, edges, `same`,
  `unchanged_by`, `below_len`, `at_most_len`, `length_place`), the decisions
  (`plans.ves`: compared checks, the counted-loop order, each counted loop's
  plan with its linear index forms and cost model, compared loops, dominated
  checks) and `stable_views`/`header_hoists` (`hoists.ves`). Entry
  `decide_text`.
- **`src/bounds_hoist.rs`, `src/bounds_hoist/facts.rs`**: the pass is split
  into decide and apply, with no change to what it does.
  `hoist_function_traced` (doc-hidden, tests only) reports each decision
  (`Step`: kind, header, enclosing loop, tried headers, decision text) on the
  function as it stands before the pass applies it; `hoist_function` is the
  same driver with no trace. `version` became `counted_plan` plus
  `clone_loop`; `drop_compared_checks` became `compared_checks` plus
  `make_unchecked`; `drop_dominated_checks` became `dominated_checks` plus
  `make_unchecked`. This file is outside the lane's exclusive paths; the
  hunk is flagged in the PR.
- **`tests/self_host_bounds_hoist/words.rs`**: the input encoder (one step and
  one function). Types cross as the flags the pass queries (`u64`, `usize`,
  sequence, slice, text, `Never`) and as interned `Debug` names for the
  header-hoist type; this moves onto `@vesper/place-ir` in part 2.
- **`tests/self_host_bounds_hoist.rs`**: the corpus at the hoisting boundary of
  the native optimize pipeline (`drops::lower_world`, `ir_opt::optimize`,
  `ir_inline::inline`, `fip::elaborate_non_fip_drops`); every step of every
  function, plus `stable_views` and `header_hoists` of the pass's input and
  output, Rust text against Vesper text (JIT optimizer on, and off on every
  9th program); a check that the traced pass equals the pass; an ignored
  timing test.
- BUILD.bazel data and size entries for the test.

## Results

| Run | Differences |
|---|--:|
| every decision, every 25th world (11 programs, 228 functions, 1,943 steps) | 0 |
| every decision, whole corpus, JIT optimizer on (288 programs, 14,316 functions, 122,093 steps) | 0 |
| every decision, every 9th program, JIT optimizer off (32 programs, 1,535 functions, 13,237 steps) | 0 |
| traced pass vs pass (`Debug` of the output, versioned count), every 7th program; the test samples every 14th | 0 |

Steps with something decided in the whole-corpus run: 1,395 accesses that
comparisons prove, 767 counted loops versioned, 188 compared loops versioned,
2 dominated checks, 6,812 non-empty `stable_views` and 8,878 non-empty
`header_hoists` answers. The whole-corpus run takes about an hour in a debug
build, so the test samples every 12th program by default
(`BOUNDS_HOIST_STRIDE=1` runs them all).

## Numbers

Not measured in this part. The ignored `timing` test is written: it runs
ABBA of the whole Rust pass against the Vesper decisions on pre-encoded
inputs, and of `stable_views` plus `header_hoists` alone. It needs a release
build, and this session's disk allowance could not hold one next to the
debug build. Part 2's scoreboard row measures the full Vesper pass (decisions
plus rewrite) against Rust, which is the comparison that matters for the
swap.

## Stage 1 guard

`src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`,
`bootstrap/**` and `packages/vesper-compiler/**` are untouched. The pass runs
only on the native optimize path and its output is unchanged (the traced
test compares it), so no verified artifact can move.

## Compiler gaps

None blocked the port. Two shapes cost workarounds:

1. A call's result used as a receiver (`natural_loop(in g, h).len()`) is
   E0202/E0206: bind it first.
2. `u64` division is a `Fail` effect even by a non-zero constant divisor
   (`NONE64 / trips` under `trips != 0`): the cost model compares without a
   product.

Constants are not importable across modules yet (#1239), so each module
repeats the op, pass and type-flag codes.

## What is left

- Part 2: the rewrite (loop versioning, `SeqOp::Hoisted`, the guards) on lane
  1's `@vesper/place-ir` tables, whose writer needs a structured payload kind
  for the sequence operations the pass creates; the pass harness (programs,
  IR check, JIT runs); the `hoist` hook in the hybrid build; the scoreboard
  row (pass alone and whole corpus, ABBA and callgrind).
