# Self-hosting SH3 lane 11: `@vesper/ir-inline`

Claim #1250. Part 1 of the lane. The branch stacks on #1232 (head b90b4ca); origin/main was c8ac321 when it started.

Rust stays the default. Nothing in `src/` changes: this part adds a package, a word encoder in the tests and a differential test. The swap through lane 1's `ir_swap` `inline` hook, the hybrid module and the scoreboard row are part 2.

## What changed

- **`packages/vesper-ir-inline`** (`@vesper/ir-inline`, on `@vesper/check-data`) ports all of `src/ir_inline.rs` and `src/ir_inline/site.rs`. That covers:
  - `size`, `cost` and `Budget`;
  - the call graph, including the exact Tarjan order;
  - the recursion, `fip`, caller and callee exclusions, and every refusal reason;
  - the per-site rules: site-cost folding of known arguments, the single-site rule, growth caps, handler sums and partial inlining;
  - splicing, folding of literal constants, and the whole `Report`.
- **Splicing is ported too.** Later decisions read the program after the earlier splices, so exact decisions need exact splicing. The package keeps `ir::Program` as its own flat tables:
  - copy-struct rows for functions, blocks and instructions;
  - pools of operand words and patterns.

  Part 2 moves it onto lane 1's `@vesper/place-ir` tables (#1259).
- **World questions are answered on the Rust side as fact bits.** `tests/common/inline_words.rs` precomputes them:
  - per type: layout aggregate or scalar, holds a sequence, slice, `Box`, function type, plain, `Never`;
  - per function: world, fip, generic, dyn, slots, captures, borrowed match;
  - the generic instance type maps for each call site (callee and span).

  The same file holds the input encoder, the report decoder and the canonical text dump.
- **One entry.** `inline_entry(in input: Str) -> ChunkSeq[u64]`. The first word picks the mode: report, inlined-program dump, or input dump.
- **`tests/self_host_ir_inline.rs`**:
  - decoder round trip;
  - the whole `Report` plus every function's size and cost after inlining;
  - the inlined program as canonical text;
  - two tight budgets: a small one, and `Budget::none()`;
  - two ignored tests: ABBA speed and compile time.

  The input is the native path's program, `drops::lower_world` followed by `ir_opt::optimize`.
- **`BUILD.bazel`**: a data entry and a `long` timeout for the new test.

## Numbers

Corpus worlds go through `drops::lower_world` and `ir_opt::optimize`, every 4th world (72 programs), in a debug build:

| Check | Differences |
|---|--:|
| Decoder: input program read back as text | 0 of 72 |
| Decisions: whole `Report` plus per-function size and cost (9,726 sites inlined, 1,426 refused) | 0 of 72 |
| Inlined program, canonical text | 0 of 72 |
| Tight budgets (small, and `none`), every 3rd of those | 0 |

The test defaults to every 8th world, which is a subset of the run above. Preparing the inputs (`ir_opt` in a debug build) takes most of the time: the every-4th run took 1,001 s on two cores. `SELF_HOST_IR_INLINE_STEP=1` runs the whole corpus.

Speed, ABBA, `port-compiler` profile, `ir_inline::inline` against the package:

| Run (36 programs, every 8th world) | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| Pass alone, input encoded beforehand | 601 ms | 156 ms | 0.26x |
| Pass with the input encoding | 688 ms | 196 ms | 0.28x |

These numbers flatter Vesper in two ways:
- The Rust side includes the `Program` clone that `inline` needs.
- The Vesper side returns the report words, not the inlined program as Rust values.

The decode back to `ir::Program` comes with part 2, through lane 1's writer. Lane 1 measured that decode at 0.32x of lowering. Callgrind numbers also come with part 2, where the hybrid build runs the pass.

## Stage 1 guard

Untouched: `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`, `bootstrap/**`, `packages/vesper-compiler/**`. No file under `src/` changes at all. The bootstrap-stage1 workflow run on this branch is linked from the PR.

## Compiler gaps

1. **`ir_opt::optimize` makes optimized JIT compiles of large packages slow.** This package has about 4,300 lines.
   - In a debug build, `entry("inline_entry", true)` takes about 171 s. With the optimizer off it takes about 5 s.
   - gdb shows all of the difference in egglog, which runs per block, so the cost grows with roughly blocks × values per function.
   - Splitting the largest functions (`fold` became seven) took it from 220 s to 171 s. The rest is spread across functions: the per-function `ir_opt` total is 159 s.
   - Repro: `COMPILE_TIME_SIZES_ONLY=1 cargo test --test self_host_ir_inline -- --ignored compile_time --nocapture`.
   - The fix is in `ir_opt`, so it is Stage 1 territory. The test compiles the entry once per binary (`OnceLock`).
2. **Variable shifts raise `Fail`.** Shift totality is the same gap the bit-builtins lane found:
   ```vesper
   pub fn f(x: u64, n: u64) -> u64 { x << n }   // E0401: unhandled effect 'Fail'
   ```
   The port uses `wrapping_shl_u64(x, n)`.
3. **A sequence-owning struct can't be read out of a sequence:**
   ```vesper
   struct Row { xs: ChunkSeq[u64] }
   // rows: ChunkSeq[Row]
   match rows.try_get(0) { Some(r) => r.xs.len(), None => 0 }   // E1522: use pop
   ```
   Rust's `Function { blocks: Vec<Block { insts: Vec<Inst> }> }` therefore becomes flat pools. A list that grows is moved to the end of its pool, and the dead copy stays behind.
4. **World queries stay in Rust** (census C10, as in #1232). Layout, function types and generic instance maps arrive as precomputed bits and maps.
5. **Harness: `ir::lower_bit_builtin_calls` is `pub(crate)`.** The test input therefore skips that step of the native pipeline. Inlining is the same either way, because the step only rewrites calls to bit builtins, but the input is not byte for byte the native program.

## What is left (part 2)

- Move onto lane 1's `@vesper/place-ir` tables (#1259). Its payload pool replaces this package's opaque `Debug` keys.
- Install through lane 1's `ir_swap` `inline` hook, as hybrid module `ir_inline`, and run the full suite on the hybrid build.
- JIT run results with the swapped inliner.
- The SH3 scoreboard row: the pass alone and the whole corpus, ABBA and callgrind.
