# Self-hosting SH3 lane 11: `@vesper/ir-inline`

Claim #1250.
- Part 1 (#1274, merged as 411d30f) ported the pass with a differential of decisions and inlined programs.
- Part 2 (this branch, from main at 411d30f) has two pieces:
  - The package writes the inlined program back as words, which decode to a `Debug`-equal `ir::Program`.
  - The hybrid Stage 2 build runs the Vesper inliner through `ir_swap`'s `inline` hook.

Rust stays the default and nothing is deleted. No file under `src/` changes in either part: the swap point is lane 1's `src/ir_swap.rs`.

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

## Part 2: the inlined program back in Rust, and the hybrid swap

- **Program words.** `inline_program_words` returns the report's length and words, then the inlined program:
  - the package's name table, which comes first;
  - per function, its locals, its value types and its blocks.

  Blocks use the input's own instruction encoding (`put_program_words`, mirroring `read_function`). `inline_entry` gets mode 3 for it.
- **`decode_program`** (tests/common/inline_words.rs) rebuilds Rust's `Report` (its public fields) and the `Program`:
  - Each function starts from its input copy. The inliner only ever changes blocks, locals and values; parameters, bounds, captures and the `fip` tables never change.
  - The operands the package rewrites (value ids, renamed locals, places, patterns, block targets, handler aliases and moves) come from the words.
  - The parts it copies without reading (operators, literals, call types, sequence, vector, tiled and `fip` operations, handled expressions) come from the first input instruction with the same opaque index.
  - State tokens are threaded again with `ir::thread_tokens`, as `ir_inline::inline` does.
- **Hybrid module `modules/ir_inline`** (`@vesper/hybrid-ir-inline`, ABI v4) has one function, `inline_words(in Slice[u8]) -> ChunkSeq[u64]`.
  - `src/hybrid.rs` puts `vesper_inline` in the `inline` slot of lane 1's `VESPER_IR_PASSES` table (#1320). The program then still goes through lane 1's `@vesper/place-ir` round trip, as every other hook's does. The later SH3 lanes fill the other slots of that table.
  - `VESPER_HYBRID_STATS=1` reports `ir-inline answered N calls`, and `pilot_module_runs_from_vesper` requires calls.
  - The `divergent-ir-inline` feature makes the module return an empty report, so nothing is inlined. The hybrid workflow checks that the differential harness catches it.
- **Decision: the package keeps its own tables instead of moving onto lane 1's `@vesper/place-ir`.** Lane 1's tables carry the same program. Rewriting the 4,300-line pass over them would change no result. The words above already round-trip exactly, and lane 1's payload pool plays the same role as this package's opaque indexes. A shared table is worth adopting when several SH3 passes run back to back without going through Rust: lanes 9-13 chained in one module.

### Part 2 numbers

| Check | Differences |
|---|--:|
| `decoded_programs_equal_rust`: the decoded `Report` (public part) and `Program`, `Debug`-equal to Rust's, every 20th corpus world in a quick local run (CI runs the default every 8th) | 0 of 15 |
| Hybrid vs default compiler, `crates/vesper-hybrid/tests/differential.rs` (538 files × check, IR and object; objects are optimized, so every one goes through the swapped inliner) | 0 of 1,614 runs |
| `pilot_module_runs_from_vesper`: `ir-inline` answers calls from Vesper | yes |

The differential's whole run took 64.36 s with the default compiler and 64.55 s with the hybrid (1.003x; both debug builds, the harness's own timing).

Callgrind, `port-compiler` profile, `vesper build --lib packages/vesper-check-data`, counting inside `vesper::ir_swap::inline` only:

| Compiler | Ir |
|---|--:|
| Default (Rust) | 423,940,407 |
| Hybrid (encode, Vesper inliner, decode, re-thread) | 321,100,583 |
| Ratio | **0.76x** |

The two objects are byte-identical. The whole build takes 4.98 s with the default compiler and 4.96 s with the hybrid.

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

Part 2's hybrid numbers below include the decode.

## Stage 1 guard

Untouched: `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`, `bootstrap/**`, `packages/vesper-compiler/**`. No file under `src/` changes at all; the hybrid hook goes through lane 1's `ir_swap`, which the verified backend never calls. Part 1's bootstrap-stage1 run passed ([run 44853720355](https://github.com/robot-head/vesper/actions/runs/44853720355)); part 2's passed too ([run 44898113914](https://github.com/robot-head/vesper/actions/runs/44898113914)).

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
3. **A sequence-owning struct couldn't be read out of a sequence when part 1 was written:**
   ```vesper
   struct Row { xs: ChunkSeq[u64] }
   // rows: ChunkSeq[Row]
   match rows.try_get(0) { Some(r) => r.xs.len(), None => 0 }   // was E1522: use pop
   ```
   Rust's `Function { blocks: Vec<Block { insts: Vec<Inst> }> }` therefore became flat pools. A list that grows is moved to the end of its pool, and the dead copy stays behind.

   Element views (#1221) now check this repro on main. Nested rows are therefore a possible follow-up, measured against the pools.
4. **World queries stay in Rust** (census C10, as in #1232). Layout, function types and generic instance maps arrive as precomputed bits and maps.
5. **Harness: `ir::lower_bit_builtin_calls` is `pub(crate)`.** The test input therefore skips that step of the native pipeline. Inlining is the same either way, because the step only rewrites calls to bit builtins, but the input is not byte for byte the native program.

## What is left

- Chain the inliner with the other SH3 passes on one table (lane 1's `@vesper/place-ir`) once lanes 9-13 swap in.
- Answer the world questions in Vesper (census C10).
- The single-build SH3 scoreboard re-measurement (SH3 plan) picks up this lane's row.
