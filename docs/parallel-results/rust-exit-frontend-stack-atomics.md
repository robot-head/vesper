# Rust exit: the stack bound and atomics under the flip bar

Claim #1875 (part of #1874 and #1182). Base `907e978`.

The stack-bound report (`src/stack_bound.rs`) has been swapped into the
hybrid since #1371. It measured 2.54x of Rust then, and 2.35x on `907e978`
after the codegen waves of #1426. Both numbers were mostly per-call setup
and decoding. This change makes the port and the hybrid's crossing cheaper
and leaves the analysis unchanged.

## What changed

- `@vesper/check-data`:
  - `names_sized(slots)`: an empty name table with fewer hash slots.
    `names_new` now calls it with the 4,096 slots it always had.
  - `read_names_ctx`: reads only the names of an input whose types and
    world are empty. The table is sized for those names, and the other
    sections are skipped. For any other input it reports `ok = false` and
    leaves the reader where it was.
  - `put_text_words` (util): writes text as words, eight bytes per
    `try_read_u64_le` rather than one byte per shift.
  - `pack_text` now uses `put_text_words`, so every checker package's text
    answer is packed faster, with the same words.
  - `Names::write` now extends from the table's bytes in one step instead
    of pushing them one by one.
  - `Names::put_words`: one name as words, read in place.
- `@vesper/check-loops-atomics`, `stack.ves` and `lib.ves`:
  - Both stack entries use `read_names_ctx`, falling back to `read_ctx`.
    `read_ctx` built a 4,096-slot name table, a 4,096-slot type table, the
    known names and a world on every call; together that cost more than
    the analysis.
  - The graph, the adjacency and Tarjan's work stacks are sized up front.
  - Each SCC is popped straight into `members` and insertion-sorted there.
    Before, each one allocated its own sequence.
  - The report and the word answer are reserved once. Names go out through
    `Names::put_words`, not a temporary copy.
- `tests/common/stack_words.rs`, which both the hybrid build and the
  differential use:
  - The encoder borrows its names, scanning a list of up to 32 before it
    switches to hashing; before, every name was cloned into a `BTreeMap`.
  - It sizes its buffers.
  - It computes the empty world's `fip` flag once; before, it built
    `World::default()` per call.
  - The decoder copies each text's words in one pass.
- `tests/common/check_harness.rs`: `abba_ci`, paired ABBA rounds with the
  95% interval of the geometric-mean ratio.
- `tests/self_host_check_loops_atomics.rs`: `stack_bound_speed` (ignored),
  which times the text entry, the word entry and the hybrid's whole path.

No Rust compiler code changed.

## Differential results

| Check | Inputs | Differences |
|---|--:|--:|
| `stack_bound_matches_rust` (text report) | 4,000 generated graphs | 0 |
| `stack_bound_words_match_rust` (hybrid words) | 4,000 generated graphs | 0 |
| hybrid object (AOT, `vesper build --lib` of `modules/stack_bound`) against `rust_stack_bound_report` | the same 4,000 graphs | 0 |

The other differentials over `@vesper/check-data` (data, effects, fip,
tensor types, names and packages, the harness itself) and the control and
atomics differentials were re-run on the changed package; results are in
the PR.

## Numbers

Linux x86-64 cloud host (4 vCPU), release, `RUSTFLAGS="-C target-cpu=native"`.
Rust is `rust_stack_bound_report` over the 4,000 graphs (about 12 ms).

| Path | Before (`907e978`) | After | 95% CI (after) |
|---|--:|--:|--|
| Port alone, JIT word entry (`check_stack_words`) | 2.34x | 1.08x | 1.06x..1.11x (20 rounds) |
| Port alone, AOT hybrid object | 1.46x | 0.79x | best of 10 |
| Hybrid path, JIT (Rust encode, Vesper, Rust decode) | 3.27x | 1.51x | 1.48x..1.52x (20 rounds) |
| **Hybrid path, AOT object (what the hybrid runs)** | n/a | **1.20x** | **1.18x..1.26x** (30 rounds) |

Callgrind on the AOT object went from 143M to 96M instructions over the
4,000 graphs. Before, the biggest costs were `report_words`' byte-by-byte
packing, then malloc, free and `seq_grow` from sequences that grew from
empty.

**Flip gate: PASS.** The diffs are 0, and the 95% CI upper bound is 1.26x,
under the 1.50x bar. Rust lines deleted: 0. While the Rust compiler is the
default, `src/stack_bound.rs` is its implementation, and it goes with
`src/` in the Stage 2 retirement PR (#1397).

Review follow-ups (Codex on #1891), both with tests:

- An SCC larger than 16 vertices is heap-sorted through a copy. A
  3,000-function call ring and a 3,000-deep chain match Rust
  (`stack_bound_words_match_rust_on_large_graphs`). Insertion sort alone
  was quadratic on large rings.
- The decoder clamps a text's word range to the stream. A truncated
  stream no longer panics (`stack_words_decode_truncated_streams`).

## Atomics

The atomics task (`needs_prelude`, `needs_workers`, `check_world` and
`check_program`) measured 2.31x, and 2.22x on `907e978`. Callgrind on an AOT
build of the task put half its instructions in decoding function bodies
(`ast.ves` `read_expr`, about 4.8 million expressions over the corpus).
Each node allocated a children sequence, wrote its 17-word row three
times, and had its parent read and rewrite the row to set fields it
already knew.

- `ast.ves`:
  - The children of the nodes being read now share one stack
    (`Exprs.stack`). A node's run moves to `kids` when the node is done.
  - A parent passes what it knows of a child before reading it, in
    `Up`: the argument's `Pass`, a `let` value's type and pattern, a
    match arm's pattern, and a clause the checker skips. Each row is
    written once.
  - When a `for` is a `let` value or an arm, the parent's pattern still
    wins, as before.
  - `ExprRow.arm` marks match arm bodies.
  - `Exprs::subtree_end`: rows are in pre-order, so a subtree is one run
    of rows.
  - The tables are sized from the input left.
- `atomics.ves`:
  - `expr_mentions` and `calls_workers` scan a body's run of rows instead
    of recursing. Both answer whether any node matches, so the order does
    not matter. A `let` value's and an arm's patterns are read from the
    rows that carry them.
  - `read_ir` reserves its tables.
- `tests/self_host_check_loops_atomics.rs`: `atomics_speed` (ignored),
  the atomics task alone with the 95% interval.

| Check | Inputs | Differences |
|---|--:|--:|
| `atomics_rules_match_rust` | 612 worlds (11,790 lines) | 0 |
| `control_rules_match_rust` (shares the decoder) | 1,134 worlds (197,899 lines) | 0 |

| Path | Before (`907e978`) | After | 95% CI (after) |
|---|--:|--:|--|
| Atomics, JIT entry over the corpus (Rust about 300 ms) | 2.22x | **1.25x** | **1.17x..1.26x** (10 rounds) |
| Atomics, AOT object over the same inputs | 1.76x | 1.19x | best of 5 |

**Flip gate for the port: PASS** (0 diffs, upper bound 1.26x). The
atomics rules are not swapped into the hybrid yet: `check_world` runs
inside the checker and needs the AST across the hybrid boundary. Rust
lines deleted: 0.

## Slice 2: `check::check` sequenced in Vesper (#1900)

The hybrid driver used to run the whole semantic check as one Rust pass
(`Step::Check`). Now `check` at the end of
`crates/vesper-hybrid/modules/driver/src/lib.ves` runs `check::check`'s
sequence, and each part is one `hd_step` over the session:

| Step | Part | What it runs |
|--:|---|---|
| 28 | `CheckUntyped` | `arrays::untyped_members`. Repeats while a rewrite applies, and keeps each rewrite's diagnostics. |
| 29 | `CheckTensor` | `tensor_types::check`. When it answers, it holds the whole check. |
| 30 | `CheckFrontend` | The Beacon prelude, Pact elaboration plus the core checker, then the Beacon rules. |
| 31 | `CheckUses` | `E0110` for each `use` outside a package. |
| 32 | `CheckFinish` | Appends the rewrites' diagnostics, innermost first, as the Rust recursion does. |
| 33 | `CheckReport` | The driver's diagnostics decision and rendering, shared with `Step::Check`. |

The checked world stays in the session (`s.checked`), where EmitInterface,
LibBuild and the passes after the check read it. That is the interface
the driver-steps lane (#1893) builds its slice 3 on. Each later slice
replaces one part with its Vesper port.

| Check | Inputs | Differences |
|---|--:|--:|
| `crates/vesper-hybrid/tests/differential.rs` | the `check`/`emit` corpus | 0 |
| `crates/vesper-hybrid/tests/driver.rs` | every CLI option set on `examples/`, the package fixtures and the programs. New programs: untyped members (clean, with errors, with `use`), Beacon, tensor types | 0 |

Each part is one host call, so the change in speed is noise: a
`vesper check` makes 6 more FFI calls than before. Rust lines deleted: 0.
The Rust `Step::Check` stays in place until the driver-steps lane has
moved every caller.

## Slice 3: the checker AST crosses into `@vesper/ast` (#1910)

The dyn, atomics/control and regions ports are all blocked on the same
thing. Their Rust swap points hold an `ast::Module`, often one a Rust pass
has rewritten, while the ports read an `@vesper/ast` `Ast`.

- `tests/common/ast_words.rs` (shared with the hybrid) writes any
  `Module` as the `ast` section of the `parse_words` stream. That is
  format 1: names, 29 arenas, the pool and the five module lists, with
  nodes in preorder.
- `packages/vesper-ast/src/words.ves` (`read_ast_words`, plus the
  `ast_words_dump` and `ast_words_count` entries) reads that section
  back.
- Two things no parse makes also cross. `Type::IntLiteral` is type kind
  6, carrying its literal (the hybrid's reader accepts it too).
  `sugar_origins` is dropped, because no Vesper pass reads it.

| Check | Inputs | Differences |
|---|--:|--:|
| `modules_cross_into_vesper_as_words` | 1,372 repository modules, plus their constant, untyped-member and Beacon-prelude rewrites | 0 |
| `built_modules_cross_into_vesper_as_words` | a built module with integer literal types, wide and negative literals, absent and present children | 0 |

| Cost (repository corpus, release, native CPU) | Time |
|---|--:|
| Crossing: encode plus Vesper decode, 1.55M rows | 0.42-0.49 s |
| `check::check` of the same modules | 16.6-17.7 s |
| Crossing / check | **0.028** (median of 5) |

The crossing adds about 3% to the check it feeds. While making the
differential, I found that the Rust dumper in `tests/self_host_ast.rs`
left out a literal pattern's integer type, which `dump.ves` prints. It
prints it now.

## Slice 4: the dyn lowering swapped into the hybrid (#1976)

`dyn_fn::lower` and `dyn_trait::lower` now run from `@vesper/check-dyn`
in the hybrid, over the checker AST crossing of slice 3.

- `src/dyn_fn.rs` and `src/dyn_trait.rs` each gain a swap-in point
  (`DynFnTable`, `DynTraitTable`). The Rust code is `rust_lower`, the
  default. `dyn_fn::span_base` is the span base, split out so the swap
  computes it as Rust does.
- The writer of the `ast` section moved from `modules/parse` into
  `@vesper/ast` (`put_ast_words` in `words.ves`), so the parse module and
  this swap share it.
- `@vesper/check-dyn` gains `dyn_fn_bytes` and `dyn_trait_bytes` (and the
  `_words` harness entries). Each reads the section, runs its pass and
  writes back the outcome: the diagnostics; then, when the pass lowered
  the module, the lowered `ast` section, then the side tables (origins
  and end for `dyn_fn`; params, forwards and boxed traits for
  `dyn_trait`).
- `tests/common/dyn_words.rs` decodes the outcome, and
  `crates/vesper-hybrid/src/parse_words.rs` gains `read_ast_section` to
  read the lowered module. A hidden generic's trait comes from the bound
  the pass adds for it.
- `crates/vesper-hybrid/modules/check_dyn` and `src/swap/check_dyn.rs`
  install both tables. The gate rows stay `open`, because there is no
  flip-gate configuration yet.

| Check (`crates/vesper-hybrid/tests/check_dyn.rs`, the linked AOT object) | Inputs | Differences |
|---|--:|--:|
| `dyn_swap_matches_rust_on_the_repository_sources` | 1,395 sources, both passes forced | 0 |
| `dyn_swap_matches_rust_on_the_cases` | 8 cases: params, forwards, boxed and failing passes | 0 |

| Timing (`dyn_swap_speed`, both passes, encode plus Vesper plus decode against `rust_lower`) | Ratio | 95% CI |
|---|--:|--:|
| The modules the checker sends (9 that use function objects or `dyn`) | **1.09x** | **1.07x..1.10x** (10 rounds) |
| Every module, forced | 0.83x | 0.80x..0.85x |

Rust lines deleted: 0. `src/dyn_fn.rs` and `src/dyn_trait.rs` go with
`src/` in the Stage 2 retirement PR (#1397).

## What is left

- Port the parts of `check::check` one at a time: the core spine
  (#1179), effects (#1180), regions (#1181), dyn (#1183) and Pact (#1185).
- Swap atomics/control and regions into the hybrid over the crossing
  (#1910). Dyn is done (slice 4).
- `read_expr` still costs about 360 instructions per node, mostly in
  generated code that moves 17-word rows (each `u8` and `bool` field
  takes a word). A packed row layout or narrower fields would cut it
  further.
