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

## What is left

- Swap atomics and the control rules into the hybrid, once the checker's
  AST crosses the boundary (this goes with core typing's spine, #1179).
- `read_expr` still costs about 360 instructions per node, mostly in
  generated code that moves 17-word rows (each `u8` and `bool` field
  takes a word). A packed row layout or narrower fields would cut it
  further.
