# Self-hosting SH2 lane 12: fip checks in Vesper

Claim #1184. Base `e43a56f` (main after SH2 lane 4's #1217). Prompt:
[12-fip.md](../agent-prompts/self-host-sh2/12-fip.md). Rust stays the
default checker: nothing here replaces or deletes Rust code.

## What changed

This is part 1 of the lane: the checker half of `src/fip.rs`.

- **`packages/vesper-check-fip`** (new, on `@vesper/check-data`):
  `fip::check_world` and everything it calls, function by function:
  `caller_slot_functions`, `unsafe_non_fip_functions`, `FipChecker`
  (`check_fip_fn`, `check_non_fip`, `check_expr`, `bind_pattern`,
  `pop_matching_slot`, `consume_caller_slot`, `merge_states`,
  `slot_multisets_match`, `non_fip_call_uses_box`), the `Box` predicates
  (`is_box_type`, `box_inner`, `is_box_pattern`, `type_contains_box`,
  `pattern_has_box_pattern`), `remaining_box_paths`, `slots_match`,
  `slot_size_label`, `seq::fip_allocation`, `World::constructor` on a
  qualified name, and the size of `Layout::of`.
  - `src/ast.ves`: the function bodies as flat node tables (expressions,
    patterns, one list pool, functions with their generics and parameters)
    and their decoder. Names are check-data symbols, types check-data ids.
  - `src/layout.ves`: the size half of the census layout sample, memoized
    per type id. It goes once SH2 lane 5's layout package can be imported.
  - `src/state.ves`: `FipState` as rows. `bindings` (a `BTreeMap<String,
    BindingState>` in Rust) is a name-ordered row table, moved paths are
    (name, path id) rows over a per-function path table.
  - `src/check.ves`: the checks. Diagnostics are text lines (code, span,
    message) that the harness compares with Rust's.
- **`tests/self_host_check_fip.rs`** (new): the fip task encoder
  (`FipEnc`, appended after lane 4's world section) and the differential.
- **`packages/vesper-check-fip/tests/cases.txt`**: the fip checker test
  sources, extracted from `tests/fip.rs`, `tests/borrowed_box_match.rs`,
  `tests/loop_boxes.rs` and the `@fip` cases of `tests/seq.rs`, plus four
  hand-written cases for paths those tests do not reach (`&&` merges, a
  dropped statement temporary, overwriting an owned box, a match on a
  borrowed box, receiver and argument temporaries, `ChunkSeq`/`String`
  allocation, `handle Alloc`, an effect op producing a box, generic and
  sized slot labels, caller-slot forwarding, loops and closures). It is a
  `.txt` file so the `.ves` walkers of other tests skip it.
- `BUILD.bazel`: the test's data (the same anchored globs as lane 4's
  tests) and the `long` tag.
- One scoreboard row in `docs/maintenance/self-hosting-plan.md`.

`check-data` needed no new fields: the world section already carries
`fip::active`, the ADTs, the expression types and the effect declarations.

The decoder reads a fixed section after lane 4's input; the encoder interns
names and types through the harness's `Enc`, so the decoder maps them with
`nmap`/`tmap` (pool words carry a tag: index, name or type).

## Differential

`cargo test --test self_host_check_fip` (debug, Linux x86-64):

| Test | Worlds | Diagnostics compared | Differences |
|---|--:|--:|--:|
| `checker_cases_match_rust` (cases.txt) | 37 | 61 (all eight codes E0801-E0809 fire) | 0 |
| `corpus_and_mutants_match_rust` (corpus + cases, each with 3 mutants) | 1,260 | 57,504 | 0 |
| `unoptimized_jit_agrees` (cases + mutants, optimizer off) | 132 | 335 | 0 |

Each diagnostic's code, span start and end, message, help and related
labels are compared as text (the fip checks attach no help or labels; the
Rust side prints them if they ever appear). The clean corpus has no fip
diagnostics, so the mutants make them fire: (1) every function `@fip`,
(2) every function `@fip` with every parameter `sink`, (3) `@fip` flipped
on every function (so `Box::new_in_caller` outside `@fip` is E0802).
Diagnostic counts over the corpus run: E0801 41,616, E0806 15,628, E0804
166, E0805 43, E0809 24, E0802 18, E0808 6, E0807 3.

Divergence check: changing one E0809 message in `check.ves` makes 3 of the
37 checker cases differ, each reported with its line.

## Numbers

Release build, Linux x86-64 (4 vCPU cloud host; wall clock moves by up to
30% between runs). `ABBA` is best of six per side over pre-encoded inputs.
The Vesper side includes decoding its input; "checks only" subtracts the
decode-only entry.

| Inputs | Rust `check_world` | Vesper (with decode) | ratio | Vesper checks only | ratio | Rust encoding |
|---|--:|--:|--:|--:|--:|--:|
| corpus + cases (315 worlds) | 93.5 ms | 86.3 ms | 0.92x | 52.6 ms | 0.56x | 232 ms |
| with the 3 mutants (1,260 worlds) | 399.6 ms | 401.6 ms | 1.01x | 293.5 ms | 0.73x | 628 ms |

Callgrind (release, `CHECK_HARNESS_STRIDE=8`: 288 worlds with mutants,
8,725 diagnostics), instructions of each side over the same run with
neither side (`CHECK_FIP_SIDE=none`, 81,098 M Ir of loading, encoding and
compiling) subtracted:

| Side | Ir | ratio |
|---|--:|--:|
| Rust `check_world` | 277.5 M | 1.00x |
| Vesper `check_fip_count` (with decode) | 393.7 M | 1.42x |

So the Vesper side retires more instructions than Rust and still runs in
about the same wall time: its loops are over flat `u32` tables, where
Rust's `BTreeMap<String, _>` lookups and `Type` clones miss the cache. The
deltas are 0.3% to 0.5% of each run, so take them to about 10%. On the
ABBA share of decoding (27%), the checks alone are about 1.0x of Rust's
instructions.

The checks themselves are faster than Rust, for the census's reasons: a
type is an id, so `type_contains_box` and the layout size are memoized per
id and `slots_match` compares ids where Rust compares and clones trees.
What is not at parity is getting the input there: the Rust encoder of the
world and the function bodies costs 1.6x to 2.5x of Rust's whole check.
That is census gap C10 (compiler data cannot cross the export ABI yet,
lane 4's follow-up with ABI v4); in the hybrid build it decides the
module's speed.

## Stage 1 guard

No guarded path is touched (`src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**`,
`packages/vesper-compiler/**`). The bootstrap-stage1 workflow run on the
branch is linked from the PR.

## Compiler gaps

1. **A `const` cannot be imported from another module** (lane 4's gap 1).
   `use self::layout::{NO_SIZE}` is E0111, so `check.ves` repeats
   `NO_SIZE` and every module its kind constants. Repro: `pub const A: u8
   = 1;` in `src/a.ves`, `use self::a::{A};` in `src/lib.ves`.
2. **No sequence of states** (census C5, SH2 lane 1). `merge_states` takes
   a `Vec<FipState>`, and a struct that owns sequences cannot be read out
   of a `ChunkSeq` (E1522). The port merges as it goes: the first live
   branch becomes the accumulator, each later one is compared with it and
   its E0808/E0809 bits are kept in a `ChunkSeq[u8]`, reported after the
   last arm, where Rust reports them. Repro: `struct S { v: ChunkSeq[u8] }`
   and `fn f(in s: ChunkSeq[S]) -> u64 { match s.try_get(0) { Option::Some(x)
   => x.v.len(), Option::None => 0 } }`.
3. **No map with non-copy values** (census C6, SH2 lane 1). `bindings` is a
   name-ordered row table with moved paths as rows, instead of
   `BTreeMap<String, BindingState>`.
4. **The AST cannot cross the export ABI** (census C10). The harness
   encodes every function body as words and the package decodes them;
   encoding is the larger part of the cost above. ABI v4 owned trees
   (#1189) carry `Box` trees, which this flat design does not use; a
   lane-4 `CheckInput` handle of `ChunkSeq` tables would remove the copy.

## What is left

- **Part 2: the hybrid swap.** A swap point for `fip::check_world` in
  `src/fip.rs` (a function table like `layout::install_word_layout`), the
  module `crates/vesper-hybrid/modules/fip` with its binding, the encoder
  moved where the hybrid crate can use it, the divergence check, the full
  suite on the hybrid build, and the swap's scoreboard numbers.
- **Part 3: the drop elaboration.** The rest of `src/fip.rs` works on the
  Place IR rather than the AST (`elaborate_non_fip_drops`,
  `loop_drops_elaborated`, `handle_drop_plan`, `seq_drop_plan`,
  `temporary_drops`, `replace_drops`, `borrowed_scrutinees`): about 1,750
  lines, with the IR cases of lane 4's harness (`ir_cases`).
- Element views (SH2 lane 1) once merged: `FipState` in a `ChunkSeq` and the
  plain `merge_states`.
- The layout size from SH2 lane 5's package once it merges.
- The port harness shard 7 entry, with lane 4's ABI v4 follow-up (the
  package has no library object yet).

## Regenerating cases.txt

The checker cases are the raw and plain string literals of the four test
files that mention `Box` or `@fip` and define a function, with `{CONST}`
placeholders expanded and `{{`/`}}` unescaped; the `lane12:*` cases at the
end are written by hand. A source that does not parse is skipped by the
test (2 of 39 today).
