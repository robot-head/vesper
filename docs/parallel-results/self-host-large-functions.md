# Self-hosting SH1 lane 8: compile time of large functions and shared values

Claim #1151. Base `e29e63d` (origin/main with the SH1 plan, #1158).
Prompt: [08-large-functions.md](../agent-prompts/self-host-sh1/08-large-functions.md).

**Result.** Unoptimized native compilation of large functions is no longer
superlinear in the native lowering: a 2,000-arm `match` JITs in 0.31 s
with `--no-opt` (was 350 s), the straight-line BLAKE3 compression with
half-round spills in 0.14 s (was 17.5 s) and without spills in 0.08 s (was
1.65 s), all in an optimized compiler (the `port-compiler` profile). The
optimized straight-line compression builds each of its 224 rotates once
(was 3,352) and runs 2,420 instructions per call (was 30,030; the committed
`g` shape went from 8,240 to 4,912). What is left of optimized compile time
on these inputs is 80-85% `ir_opt::optimize`, which the Stage 1 guard
covers; its design is written below as a guard call. The Stage 1 corpus
objects are byte-identical, no default build changes and no Rust is
deleted.

## Reproduce first

| Repro (SH0 std-data report) | On `e29e63d` | Scope |
|---|---|---|
| Gap 6: the straight-line compression with half-round spills takes minutes to JIT with `--no-opt` | still fails: 17.5 s release, 222 s debug | fixed here |
| Gap 6, plain straight-line shape | 1.65 s release | fixed here |
| A generated 2,000-arm `match` (new) | 350 s release with `--no-opt` | fixed here |
| Gap 1: optimized code rematerializes shared values at each store | still fails: 3,352 `ror` for 224 | fixed here |

The repros are `tests/large_function_compile.rs`, committed before the fix
(9c0d729): on the base the spill shape misses its 60 s budget (222 s) and
the rotate count is 3,352.

## What changed

All in the native lowering, outside the Stage 1 guard:

- **`src/native.rs`, `Codegen::ir_graph_at`: seal blocks early.** A graph
  block is sealed as soon as every predecessor is lowered (acyclic blocks
  other than the entry; blocks on cycles still wait for
  `seal_all_blocks`), and each `next` block of a `match` arm chain is sealed
  right after its only branch. A block left unsealed until the end got a
  parameter for every variable read in or below it, and
  `remove_block_param` on the trivial ones was quadratic: 58% of the
  250-arm `match` profile.
- **`src/native.rs`, `Codegen::ir_graph_at`: operand-only reads in large
  unoptimized graphs.** Before each instruction the unoptimized path read
  every IR value of the graph from its frontend variable (the optimize path
  stopped doing that in #931). That is values x instructions work and gives
  every join a parameter per value, which then dominated register
  allocation. Graphs with more than `FULL_REFRESH_VALUES` (256) values now
  read only each instruction's operands, as optimized code does; smaller
  unoptimized graphs keep the full refresh, so the Stage 1 corpus code is
  unchanged.
- **`src/native.rs`, `define_instances_observed_with_trace`: no CLIF text
  for the JIT.** The CLIF listing was printed for every function and thrown
  away by every JIT caller; it cost 7.5% of the `match` profile. Object
  builds still return it.
- **`src/seq/native.rs`, `TrySet`: branchless store of a plain word.** On
  the optimize path a `try_set` of a scalar element that needs no drop
  selects between the element's address and a private 16-byte stack slot
  (`Codegen::discard_slot`, one per function) and stores unconditionally,
  instead of branching around the store. Cranelift's egraph places a pure
  value at its first use, so behind the branch each stored value was built
  inside its own store block, which the next store's block does not
  dominate: every store rebuilt all the work it shared with earlier stores.
  The result is still the `bool` of the bounds check; out-of-range writes
  still change nothing in the slice. Passing the value as a branch argument
  instead does not work: Cranelift removes the single-predecessor parameter
  again.
- **`src/seq/native.rs`, `seq_hoist_header`: a private receiver.** Header
  words hoisted before a loop are cached by receiver value. Unsealed blocks
  used to hand every block a fresh parameter for the receiver, so the cache
  only ever matched inside the loop; with early sealing a frontend variable
  can return the same value in a sibling block, where the cached words do
  not dominate (two sibling loops over one `in` slice in
  `vesper-grammar`'s `der::primitive` failed the Cranelift verifier). The
  hoist now caches under a copy (`iadd_imm 0`, folded away by the
  optimizer) that only the loop's reads use. All 70 `vesper_port_object`
  libraries build.

Owned paths: `src/native.rs` (`ir_graph_at`, `define_instances_observed_with_trace`,
the `Codegen` struct), `src/native_session.rs` (the new field only),
`src/seq/native.rs` (`TrySet`), `tests/large_function_compile.rs`, this
report.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt.rs`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changes. Every
`bootstrap/corpus/*.ves` object built with `--no-opt` is byte-identical to
the base compiler's, so the Cranelift artifacts as well as the verified ones
stay at the lock. The bootstrap-stage1 workflow result on the branch is in
the PR.

## Scoreboard

Compiler: `--profile port-compiler` (optimized, debug assertions on), x86-64,
4 cores. `vesper jit FILE [--no-opt]` end to end (parse, check, AST
optimizer, IR, codegen, and running 1,000 compressions or 100,000 `pick`
calls, which is under 1% of each). Wall clock is the best of three runs
(one when a run took over 30 s); compiler Ir from callgrind, skipped where
the run is too long. Wall clock on this host moves by up to 30%.

| Input | Mode | Wall before | Wall after | Compiler Ir before | Compiler Ir after |
|---|---|---|---|---|---|
| straight-line BLAKE3 | `--no-opt` | 1.65 s | 0.076 s | 19.58 G | 0.81 G |
| straight-line BLAKE3 | optimized | 1.55 s | 1.50 s | 14.37 G | 13.12 G |
| straight-line BLAKE3, spills | `--no-opt` | 17.5 s | 0.14 s | (not run) | 1.37 G |
| straight-line BLAKE3, spills | optimized | 5.18 s | 3.02 s | 26.13 G | 25.64 G |
| 2,000-arm `match` | `--no-opt` | 350 s | 0.31 s | (not run) | 3.82 G |
| 2,000-arm `match` | optimized | 1.44 s | 1.44 s | 21.65 G | 21.58 G |
| committed `g` shape (`compress.ves`) | `--no-opt` | 0.19 s | 0.031 s | 2.50 G | 0.29 G |
| committed `g` shape | optimized | 0.20 s | 0.18 s | 1.84 G | 1.61 G |

Growth with the number of arms (`--no-opt`, before): 250 arms 0.68 s, 500
arms 4.7 s, 2,000 arms 350 s, about cubic. After: 2,000 arms 0.31 s.

Generated code, optimized executables (`vesper build`), per compression:

| Shape | `ror` before | `ror` after | Ir/call before | Ir/call after |
|---|---|---|---|---|
| straight-line | 3,352 | 224 | 30,030 | 2,420 |
| straight-line, spills | 672 | 224 | 7,489 | 3,733 |
| committed `g` shape | 432 | 144 | 8,240 | 4,912 |

The `ror` counts are for the whole executable (`compress` inlined into
`main` where it is). The SH0 report put the Rust scalar reference at about
1,000 instructions per block; the straight-line shape is now 2.4x that.

## Compiler gaps

1. **`ir_opt::optimize` is superlinear in block size (Stage 1 guard
   call).** Repro: the spill shape above, optimized: 25.6 G compiler Ir,
   84% of it in `ir_opt::optimize` (75% in egglog's `rebuild`); the plain
   shape 78%, the 2,000-arm `match` most of its 21.6 G. For each block the
   pass (a) declares `(let v (Ref v))` for every value of the *function*,
   so blocks x values facts; (b) emits a `disjoint` fact for every pair of
   places in the block, places squared; (c) gives every scalar read a
   candidate list of every earlier scalar value of the same type, and asks
   egglog for each candidate's e-class, reads x values queries. Design for
   the guard owner: declare only the values the block's reads, writes and
   candidates name; emit `disjoint` only for pairs of a read place and a
   written place; look up each value's e-class once and match candidates
   by class; and, since the two rules only forward a read through a chain
   of writes on one token, replace the e-graph with a linear forward map
   from place to last written value with a kill on each overlapping write
   (`PlaceGraph::overlaps`) and on barriers. That is the same relation the
   rules compute in 12 rounds, at linear cost, and should be checked
   against the egglog version on the corpus before it replaces it.
2. **Pure values shared by two branch arms are built in each arm.**
   Repro:
   ```vesper
   pub fn f(x: u32, y: u32, c: u64) -> u32 {
       let mut a = x;
       let mut b = y;
       a = rotate_right(a ^ b, 16);
       b = rotate_right(b ^ a, 12);
       a = rotate_right(a ^ b, 8);
       b = rotate_right(b ^ a, 7);
       if c > 3 { wrapping_add(a, b) } else { a ^ b }
   }
   ```
   The optimized `f` has 8 rotates for 4: Cranelift's elaborator places a
   pure value at its first use in each dominator scope, and sibling blocks
   do not share scopes. This lane removed the branch for `try_set`, where
   it hurt most; a general fix needs either a select for small arms or an
   anchor for values that are live into several successors (Cranelift has
   no pinning instruction for pure values), so it is left open.
3. **Unoptimized graphs of up to 256 values still read every value before
   each instruction.** That keeps the Stage 1 corpus code identical; with
   a re-recorded Stage 1 lock (a guard decision), the threshold could go
   and every graph would read operands only.

## Left

- The optimized compile time of these inputs is the guard call in gap 1.
- `try_get` (an `Option`) and aggregate-element `try_set` keep their
  branches; their loads sit in the branch, which the elaborator does not
  duplicate.
