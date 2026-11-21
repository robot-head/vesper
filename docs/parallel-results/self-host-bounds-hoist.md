# Self-hosting SH3 lane 12: bounds hoisting in Vesper

Claim #1251. Stacks on #1232 (branched from its head `b90b4ca`; `origin/main`
was `c8ac321`). Part of the [SH3 plan](../agent-prompts/self-host-sh3/README.md).

`src/bounds_hoist.rs` and `src/bounds_hoist/facts.rs` (about 3,000 lines) version
counted loops so their element accesses run unchecked behind one guard,
version loops bounded by a compared end, drop checks that a comparison or a
checked access dominates, and find the sequence headers a loop or the whole
function can read once. Part 1 ports every decision the pass makes. Part 2
ports the rewrite that applies them (loop copies, `SeqOp::Hoisted`, the
guards) on lane 1's `@vesper/place-ir` tables and swaps the whole pass into
the hybrid build through lane 1's `hoist` hook; it stacks on lane 1.

## Part 1: the analyses

### What changed

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

### Results

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

### Numbers

Not measured in this part. The ignored `timing` test is written: it runs
ABBA of the whole Rust pass against the Vesper decisions on pre-encoded
inputs, and of `stable_views` plus `header_hoists` alone. It needs a release
build, and this session's disk allowance could not hold one next to the
debug build. Part 2's scoreboard row measures the full Vesper pass (decisions
plus rewrite) against Rust, which is the comparison that matters for the
swap.

### Stage 1 guard

`src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`,
`bootstrap/**` and `packages/vesper-compiler/**` are untouched. The pass runs
only on the native optimize path and its output is unchanged (the traced
test compares it), so no verified artifact can move.

### Compiler gaps

None blocked the port. Two shapes cost workarounds:

1. A call's result used as a receiver (`natural_loop(in g, h).len()`) is
   E0202/E0206: bind it first.
2. `u64` division is a `Fail` effect even by a non-zero constant divisor
   (`NONE64 / trips` under `trips != 0`): the cost model compares without a
   product.

Constants are not importable across modules yet (#1239), so each module
repeats the op, pass and type-flag codes.

## Part 2: the rewrite and the swap

### What changed

- **`packages/vesper-bounds-hoist`**, about 2,100 new lines:
  - `encode.ves` writes a `@vesper/place-ir` function in the words of `tests/self_host_bounds_hoist/words.rs`. After each change, `read_fun` and every part 1 decision run unchanged on the function as it now stands.
  - `rewrite.ves` holds the rewrite pieces: unchecked accesses, fresh values and tokens in the order the Rust code allocates them, and the loop copy. The copy maps values, blocks, tokens, join slots, call slots and the drop maps of the copied blocks.
  - `pass.ves` is the driver, mirroring `bounds_hoist::run`: compared checks, counted loops in order (with their receiver guards), compared loops, then dominated checks. Its entries are `hoist_bytes` and `hoist_words`, and the root entry is `hoist_pass`.
  - `plans.ves` now returns each decision as a structure. The part 1 text is printed from those structures, so the part 1 differential still covers them.
- **New pool kinds.** A pass can now make the two sequence operations that bounds hoisting creates. They are flagged for lane 1, which owns `tests/common/ir_words.rs`; each sits in its own section.
  - Pool kind 5 is `SeqOp::Hoisted(access)`. Its label is the access: 0 is `Get`, 1 `Set`, 2 `TryGet`, 3 `TrySet`.
  - Pool kind 6 is `SeqOp::BoundsGuard`. Its label packs `inclusive` (bit 0), `form.negated` (bit 1), `form.plus` (bits 8 to 15) and `form.minus` (bits 16 to 23).
  - In `tests/common/ir_words.rs`, `made_payload` builds these and `decode_program` maps them after the input pool's entries.
- **`tests/self_host_bounds_hoist_pass.rs`** runs the pass harness (`ir_words::compare_pass`). It compares Rust `bounds_hoist::run` with the Vesper pass and checks that the canonical dump matches, the programs are `Debug`-equal, and the IR check agrees.
  - It samples every 24th program by default (CI runs it in a debug build); `BOUNDS_HOIST_PASS_STRIDE=1` runs them all.
  - It also holds ignored `timing` and `callgrind` tests.
- **`tests/self_host_bounds_hoist_jit.rs`** runs every sixth corpus `main` (`BOUNDS_HOIST_JIT_STEP`) through the native JIT, with optimize on and off. It runs once with Rust hoisting and once with the Vesper pass installed as the `hoist` hook, each in a child process on a large stack, and compares the results.
- **`tests/self_host_bounds_hoist/corpus.rs`** holds the corpus at the hoisting boundary, shared by all three tests.
- **The hybrid build**:
  - `crates/vesper-hybrid/modules/bounds_hoist` (`@vesper/hybrid-bounds-hoist`, entry `hoist_words`) and its blessed binding `port/bounds_hoist.rs`.
  - `hybrid.rs` installs it as the `hoist` hook in place of "Rust pass, then the place-ir round trip". A program the words cannot carry falls back to Rust.
  - `VESPER_HYBRID_STATS` reports how many programs the module hoisted, and `pilot_module_runs_from_vesper` checks that the count is above zero.
- **Bazel**: `//:bounds_hoist_package`, the hybrid module's dependencies, and the tests' data and size entries.

### Results

| Run | Differences |
|---|--:|
| pass differential, whole corpus, release build (294 programs, 1,830,999 instructions; Rust versions 1,225 loops and changes 63 programs) | 0 |
| pass differential, every 24th program, debug build (13 programs, 93 loops versioned) | 0 |
| JIT through the `hoist` hook, every third corpus `main`, optimize on and off (118 runs, 92 return a value, 50 programs hoisted in Vesper) | 0 |
| hybrid `vesper build` of a counted `try_get` loop against the default compiler | byte-identical executable |

The hybrid differential harness (`crates/vesper-hybrid/tests/differential.rs`) covers the whole corpus in CI. It now runs with the Vesper pass in the build, and it compares object bytes.

### Numbers

These are release builds on the cloud host, best of six ABBA runs per side, over every corpus program at the hoisting boundary (294 programs, 312 MB of input words).

| Measure | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| the pass with its encoding (Rust program to bytes, pass, words to Rust program) | 6.85 s | 14.05 s | 2.05x |
| the pass on pre-encoded input bytes (tables read, pass, words written) | 6.97 s | 10.48 s | 1.50x |
| place-ir identity read and write of the same input | | 0.45 s | |
| **the pass alone** (identity subtracted) | 6.97 s | 10.03 s | **1.44x** |

Callgrind was not measured. Under valgrind the corpus harness checks and lowers every corpus world before the first measured call: after 50 minutes it had checked 53 worlds. The Ir comparison needs the programs at the hoisting boundary saved first, which the harness cannot do yet (`Program` has no reader).

Most of the Vesper pass's time goes to re-encoding the function after each rewrite (`encode_fun`, then `read_fun`), which the Rust pass does not need. A function with k versioned loops is encoded k + 2 times. This is the main cost to remove, by keeping the tables up to date in place.

### Stage 1 guard (part 2)

Part 2 changes no Rust compiler source. The default compiler installs no hook. Only the hybrid installs `hoist`, which runs on the native path only.

### Compiler gaps (part 2)

1. **`sink` and `copy` are keywords** and cannot be names (E0002). A helper named `sink` and a local named `copy` had to be renamed.
2. **`ChunkSeq` has no `clone`** (E1525). A sequence of `u32` has to be copied element by element.
3. **Bitwise operators on `u32` with a literal** infer `i64` for the literal, so `(if c {1} else {0}) | wrapping_shl_u32(..)` fails with E0200. The workaround is a `u32` local updated one bit at a time.
4. **`x >> wrapping_mul(..)`** is E0401 (`>>` is checked). The workaround is to use `wrapping_shr_u64`.
5. **A hybrid module cannot return a pass's count beside its words.** The hook counts versioned loops on the decoded result instead; no caller reads the count.

## What is left

- Part 2's PR waits on lane 1. It needs #1259, and also lane 1's pass harness (`compare_pass`, `through_vesper`, the hybrid `place_ir` module), which is not pushed yet.
- Speed: update the tables in place instead of re-encoding the function after each rewrite.
- SH3 lane 14's single-build scoreboard measures the whole corpus through the hybrid with every SH3 swap installed.
