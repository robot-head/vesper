# Self-hosting SH2 lane 9: regions, sequences, strings, arrays and vectors

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1181. Base: `main` at `e43a56f` (SH2 lane 4's first PR, #1217).
Lane prompt:
[`docs/agent-prompts/self-host-sh2/09-regions-seqs.md`](../agent-prompts/self-host-sh2/09-regions-seqs.md).
Rust stays the default checker everywhere, and no Rust code was deleted.

This is the first PR of the lane (part of #1181). It ports every rule of
the six modules that is a function of types and of the `World` tables. The
rules that walk the typed AST, and the hybrid swap, are the next part (see
"What is left").

## Summary

- **`@vesper/check-regions-seqs`** (`packages/vesper-check-regions-seqs`),
  on `@vesper/check-data`, with the census data design (`u32` type ids,
  interned names, diagnostics as rows of one text buffer):
  - `src/regions.rs` type half: region rows and handles, `normalize`,
    `validate_type` (E0797), `display`, `source_label`, `mismatch` (E0200,
    E0792). The census sample, moved onto the shared package.
  - `src/seq.rs`: sequence and slice types, `holds_sequence`, `resolve` and
    every property of the operation it returns (`fallible`, `mutates`,
    `receiver_pass`, `byte_scan`, `has_receiver`, `place_primitive`,
    `with_arity`, `name`, `Debug`, the growing operations of
    `fip_allocation`), and `check_world` (E1520).
  - `src/strings.rs`: text types, `mentions_str`, `resolve`.
    `src/arrays.rs`: `element`.
  - `src/tiled.rs`: `TiledSeq` types, `seq_adt`, hot column scalars and
    vector shapes, and `check_world_adts` (E1501, E1502, E1503).
  - `src/vector.rs`: lane types, shapes, `is_vector_spelling`, `shape_of`,
    `type_error`, `layout_words`, profiles and `isa_row_entry` (E1404).
- **Differential: 0 differences on all 282 corpus worlds** in every task,
  with mutants that make each of the eight ported diagnostic codes fire.
- **Speed: the rules run at 1.12x (types) and 1.17x (world) of Rust's
  time** once the input decode is subtracted (ABBA, best of six).

## What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-check-regions-seqs/{Loom.toml,README.md,src/*.ves}` |
| Test | `tests/self_host_check_regions_seqs.rs` |
| Visibility for the test (no behavior change) | `src/seq.rs` (`check_world`), `src/tiled.rs` (`check_world_adts`), `src/vector.rs` (`isa_row_entry`): `pub(crate)` to `#[doc(hidden)] pub` |
| Bazel data and timeout for the test | `BUILD.bazel` |
| Scoreboard row | `docs/maintenance/self-hosting-plan.md` |
| Report | `docs/parallel-results/self-host-check-regions-seqs.md` |

The three visibility changes are outside the lane's exclusive paths. They
let the differential call the real Rust rules instead of a copy of them in
the test; they are one line each and change no behavior.

`@vesper/check-data` is unchanged. This lane's input (function signatures,
ADT spans and field temperatures) is a section of its own task words,
decoded by `read_sigs` in the package, so no other lane's `World` layout or
input moves.

## The differential

Corpus: lane 4's, every single-file program under `tests/fixtures` and
`examples` and every Loom package under `packages` and `crates` that
parses and checks with no diagnostics: **282 worlds** on this base. The
clean corpus fires none of these diagnostics, so each task adds mutants.

| Task | Rust | Mutants | Lines compared | Diagnostics | Worlds differing |
| --- | --- | --- | --: | --- | --: |
| `regions_seqs_types` | every type rule above on every type the world mentions, `normalize` with a source-label map, and `mismatch` of each normalized type against the previous one | 24 types built around each of the world's first 24 types: region handles with wrong arities, source labels and capability labels (`$capN`, `$fncap:f:i`), `TiledSeq` with and without region rows, `Vec`/`Mask` spellings (valid, wrong lane type, wrong width, non-digit and overflowing counts, leading zeros), nested slices, arrays, and function and row types with `Isa` entries | 812,442 | E0200 34,088; E0792 11,196; E0797 22,380; E1404 14,978 | 0 |
| `regions_seqs_world` | `seq::check_world`, then `tiled::check_world_adts` | the world itself; `Slice[T]` moved into parameters (kept pass and `sink`), results, nested slices and fields; two cycles of tile widths (8, 12, 16, none, 64) and field temperatures over every ADT | 16,113 | E1501 2,011; E1502 3,056; E1503 634; E1520 10,412 | 0 |
| `regions_seqs_resolve` | `seq::resolve` of 79 callees (every method and free callee, qualified and malformed names) on 11 receivers, each result's properties and `fip_allocation` | | 869 | | 0 |

The table is from the base `e43a56f`. On `main` at `a64f537` the corpus is
287 worlds, and SH2 lane 1 (#1221) made `try_get_mut` an alias of `try_get`
in `seq::resolve`; the port mirrors it and the resolve task now has 80
callees. Still 0 of 287 worlds differ (types 841,018 lines, world 20,173).

Every diagnostic is compared by code, message, help and span (none of
these has related labels; the test asserts that). The JIT with the
optimizer off returns the same text as with it on, for each task. The
harness is sensitive: changing one word of two messages makes 278 and 282
worlds differ.

The test takes about 7 minutes in an unoptimized build on a 4-core host,
so Bazel runs it with the `long` timeout.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
`bootstrap-stage1` workflow is dispatched on the branch; the PR links the
run.

## Numbers

ABBA, best of six runs per side, release build, every corpus world (the
test `speed_abba`, ignored, with `CHECK_HARNESS_SPEED=1`). Vesper runs
through the optimizing JIT and includes decoding lane 4's input; the
decode alone is timed on the same inputs and subtracted for "rules alone".
Wall clock on these hosts moves by up to 30% between runs.

| Task | Rust | Vesper | Vesper / Rust | Decode | Rules alone |
| --- | --: | --: | --: | --: | --: |
| types (812k lines) | 230 ms | 274 ms | 1.19x | 16 ms | **1.12x** |
| world (4 mutants per world) | 21.1 ms | 55.7 ms | 2.64x | 31 ms | **1.17x** |

The census measured the region type rules alone at 0.87x; with the other
five modules' rules in the same pass and their text output, the type task
is 1.12x. The world task's input is dominated by the decode, which encodes
the whole world four times; ABI v4 (lane 4's follow-up) removes most of it.

Callgrind instruction counts are not in this PR: the harness runs both
sides in one process, and the JIT code has no symbols to toggle collection
on. They come with the hybrid swap, where `vesper check` runs each build
in its own process.

## Compiler gaps

None of these blocked the port; each cost a workaround.

1. **A non-constant divisor is a `Fail` effect even when every value the
   divisor can take is non-zero.** `128 / elem_bits(e)` where `elem_bits`
   returns 8, 32 or 64 is `E0401` (unhandled `Fail`). Workaround: a branch
   per value (`lanes_per_chunk` in `src/vector.ves`). Repro:

   ```vesper
   fn bits(x: u8) -> u64 { if x == 1 { 8 } else { 64 } }
   fn lanes(x: u8) -> u64 { 128 / bits(x) }   // E0401
   ```

   Bounds facts (#1088) do not cover a callee's result.
2. **A `const` cannot be imported from another module** (lane 4's gap 1):
   the `OP_*` operation codes live in `src/seqs.ves` only and the kind
   constants are repeated per module.
3. **No string slicing of an interned name.** Rust's
   `callee.rsplit("::").next()` becomes a byte loop that copies the last
   segment into a `ChunkSeq[u8]` and interns it (`last_segment` in
   `src/seqs.ves`).

## What is left

- **The expression rules.** The checker methods of
  `src/regions/check.rs` (region operations, E0790-E0798), `seq/check.rs`
  (sequence calls, indexing, slice views and loans, E1520-E1524),
  `strings/check.rs`, `arrays/check.rs`, `tiled/check.rs` and
  `vector/check.rs` (intrinsics and slice I/O), and the expression passes
  of `regions::check_world` (rewind flow, E0796), `strings::check_world`,
  `arrays::check_world` and `vector::check_world` (the ISA walker). They
  walk the typed AST and call back into the core checker (`self.expr`,
  `self.mode`, `require_effect`), so they need the AST in the input (ABI v4
  owned trees, or the `@vesper/ast` package with `World::expr_type`) and
  lane 7's core typing for the parts that recurse.
- **The hybrid swap.** The rules need a function table in the compiler
  (like `vesper::layout::WordLayout`) for `crates/vesper-hybrid` to
  replace; adding it touches `src/regions.rs`, `src/seq.rs`,
  `src/tiled.rs` and `src/vector.rs` outside this lane's paths. It comes
  with the expression rules, with the full-suite run of the hybrid build,
  callgrind numbers, and the port harness shard 7 entry (this PR adds no
  crate or library object for the harness to build).
- **`tiled::layout_of_adt`** needs `Layout::of` for cold fields: it ports
  once SH2 lane 5 commits the layout module on `@vesper/check-data`.
- **Element views** (SH2 lane 1, #1173): nothing here needs them yet; the
  expression rules' environment will.
