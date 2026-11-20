# Self-hosting SH2 lane 4: checker data package and differential harness

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1176. Base: `main` at `c9f228b`. Lane prompt:
[`docs/agent-prompts/self-host-sh2/04-check-data.md`](../agent-prompts/self-host-sh2/04-check-data.md).
Rust stays the default checker everywhere, and no Rust code was deleted.

This is the first PR of the lane, merged early because lanes 5-14 build on
it. Follow-ups are listed under "What is left".

## Summary

- **`@vesper/check-data`** (`packages/vesper-check-data`): the census
  tables, committed as a Loom package with the accessors the census used:
  interned names, the hash-consed type arena with `Display`, `Debug`,
  `substitute` (row splicing and first-wins dedup) and `default_type`, the
  `World` tables (ADTs, generics, variants, fields) with `constructor`,
  `mode`, `field` and `vector::layout_words`, and the diagnostics table.
- **World queries the census precomputed in Rust, now in Vesper** (task
  item 2): the effect declarations and `effectful_row` (whether a call with
  a row is effectful), the expression types (`World::expr_type`), and
  `partial_ops` / `overflow_ops` membership (`is_partial`,
  `may_overflow`), as sorted span tables with binary search.
- **The shared harness** (`tests/common/check_harness.rs`): the corpus, the
  encoder, package entries taking one `Str`, text and digest comparison,
  the census's five IR mutants, a stride for callgrind, and ABBA timing.
  The package README documents it for lanes 5-14.
- **Differential** (task item 5, except layout): 0 differences on all 281
  corpus worlds.

## What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-check-data/{Loom.toml,README.md,src/*.ves}` |
| Harness module | `tests/common/check_harness.rs` |
| Tests | `tests/self_host_check_data.rs`, `tests/self_host_check_harness.rs` |
| Bazel data and timeouts for the two tests | `BUILD.bazel` |
| Report | `docs/parallel-results/self-host-check-data.md` |

The harness is a module under `tests/common/`, not the test binary
`tests/self_host_check_harness.rs` the plan named: Cargo makes every
`tests/*.rs` its own crate, and Bazel leaves other test roots out of a
test's sources, so a lane could not `#[path]`-include a test root under
Bazel. `tests/self_host_check_harness.rs` is the harness's own tests.

No port-harness shard entry was added: the package has no Rust crate or
`vesper build --lib` object yet, so nothing runs in the port harness. It
joins shard 7 with the ABI v4 follow-up, which adds the library build.

## The differential

Corpus: every single-file program under `tests/fixtures` and `examples`
and every Loom package under `packages` and `crates` that parses and checks
with no diagnostics: **281 worlds** on this base (272 at the census base).

| Task | Rust | Lines compared | Worlds differing |
| --- | --- | --: | --: |
| `check_data_types` | `Display`, `Debug`, `numeric::default_type` of every type each world mentions; `ast::substitute` of every ADT field under every instance's arguments, and of every open-row function type with its tail bound to a row (its own instances again plus one) | 22,631 | 0 |
| `check_data_world` | `World::mode` (no bounds and under each ADT's generic bounds), `vector::layout_words`, `World::field` for each type's field names, a segment, tuple indices and a missing name, effectful rows, `World::constructor` (bare, qualified, wrongly qualified, missing), and for every expression span and its neighbour the expression type and partial/overflow membership | 1,571,515 | 0 |

The JIT with the optimizer off returns the same text as with it on, on
every eighth world. `tests/self_host_check_harness.rs` checks that the
decoder rebuilds every table the encoder wrote (sizes per world), that one
changed name changes the answer, and that the IR mutants change programs.

Both tests take about 4.5 minutes in an unoptimized build on a 4-core host
(most of it checking the corpus), so Bazel runs them with the `long`
timeout. Under Bazel the corpus is what the root package can declare
(`crates/*` are their own Bazel packages), so it is smaller there.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed; no file
under `src/` or `crates/` changed at all. The `bootstrap-stage1` workflow is
dispatched on the branch; the PR links the run.

## Numbers

The census measured this design at 0.34x (layout), 0.87x (regions) and
0.57x (IR check) of Rust's time. This PR's tasks are correctness tasks
(text output); the ABBA timing test (`speed`, ignored, with
`CHECK_HARNESS_SPEED=1`) and the encode cost before and after ABI v4 come
with the follow-up below.

## Compiler gaps

1. **A `const` cannot be imported from another module.** `use
   self::types::{K_FN}` is `E0111` (module has no item `K_FN`), so each
   module repeats the kind and mode constants it uses. Repro: a package
   with `pub const A: u8 = 1;` in `src/a.ves` and `use self::a::{A};` in
   `src/lib.ves`.
2. **`effect` is a keyword** (census C1, SH2 lane 3): the effect lookup is
   `World::effect_index`.
3. **No `i128`** (census C2, SH2 lane 2): an integer literal type is a sign
   and a 64-bit magnitude; the encoder asserts every corpus literal fits.
4. **Effect-instance dedup compares type ids, not `Display` text.** Rust's
   `substitute_effects` keeps the first of instances whose arguments print
   alike; the arena keeps the first of instances with equal ids. Equal ids
   imply equal text; the corpus has no distinct types that print alike.

## What is left

- ABI v4 input (task item 4): a `CheckInput` handle of `ChunkSeq` tables
  encoded once, with the encode cost before and after, the library object
  and its shard 7 entry.
- Layout of every corpus type at 0 differences (task item 5): SH2 lane 5
  commits the census layout on this package; if it has not when the v4
  PR lands, this lane adds the layout differential.
- ABBA and callgrind numbers for the decoder and both tasks.
