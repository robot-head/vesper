# `World::mode` speed toward the 2x build target (SH5 lane 8, #1331)

> Self-hosting wave SH5, lane 8. Prompt:
> [08-world-mode-speed.md](../agent-prompts/self-host-sh5/08-world-mode-speed.md).
> Stacks on #1312 (SH4 lane 5) at `be6db84`; `origin/main` was `bbda89e`
> (2026-11-21).

## Result

The combined-checker `vesper build --lib` takes **42.6 s**, against
**122.0 s** before #1312 (policy off) and 67.3 s with #1312 alone: **2.86x**
faster than before #1312, past the 2x target. The library object is
byte-identical in all eight runs (base and this branch, policy on and off).
`World::mode` gives the original algorithm's answer for every query: 868,714
corpus queries over 295 worlds and 400 generated worlds of mutually
recursive generic ADTs agree with the unchanged algorithm.

## Why `World::mode` was slow

Stack samples of the policy-on base build (25 samples, 2.5 s apart, every
thread) put 19 of the 22 samples on the main thread in
`ir_check::validate` -> `Flow::function`; 12 of those were in
`Flow::merge` -> `Flow::mode` -> `World::mode` and its recursive `visit`,
mostly in `format!`, `BTreeSet::insert` and `collect` for the `seen` key.
The other 6 cloned and dropped `ir_check::State` maps.

Three things multiplied:

1. **Every query starts from scratch.** `World::mode` expands an ADT
   instance `n[args]` into all of its fields, recursively, with no memory
   between queries. A checker type such as an AST node reaches dozens of
   ADTs, and the same handful of types is asked about at every join of every
   function.
2. **The cut set is the path.** A key already on the current expansion path
   is cut at the ADT's declared mode, and then removed when its expansion
   returns. So an instance reachable along many paths is expanded once per
   simple path: exponential in the depth of a recursive family. The
   generated-world test had to keep its worlds small (at most 5 ADTs, type
   depth 2) because the reference took minutes on 8-ADT worlds of depth 3.
3. **`Flow::merge` asks first.** At every CFG join it computed the mode of
   every slot and every value in both states, then tested `pass == Sink`,
   `!moved.contains(&[])` and `owned`. Most slots are not sink-owned and most
   values are not owned, so the answer was thrown away.

## What changed

- **`src/check/world_mode.rs` (new), `src/check.rs`.** `World::mode` now
  memoizes ADT expansions in the world (`World::mode_cache`). An expansion
  of key `K` (name and argument modes) depends only on `K`, the world, and
  which of the keys it reaches are on the path above it, since those are cut.
  So an expansion that cuts nothing above itself gives exactly what a query
  starting at `K` gives; the memo records it with the set of keys it
  reached. A later expansion of `K` uses the entry only when none of those
  keys is on its own path, so every cut decision, and with it the result, is
  the one a fresh expansion makes. An expansion that cut a key above itself
  (a member of a recursive cycle below the cycle's entry point) is not
  recorded and runs as before. Types that need no ADT expansion (scalars,
  bounds, strings, functions) return before any allocation.
  - The memo is a `RwLock` map, so the 4-thread validation of #1312's policy
    shares it. A clone of a world starts with an empty memo, and the memo
    never makes two worlds unequal (`PartialEq` ignores it). A world whose
    `adts` change after its first `mode` query must call
    `World::clear_mode_cache`; nothing in the compiler does (the `adts`
    writes are all in `World::collect` and in tests that build a world before
    asking).
  - The original algorithm is kept unchanged as `World::mode_reference`
    (`world_mode::reference`) for the differential test and the bench.
- **`src/ir_check.rs` (`Flow::merge`).** The cheap tests come before
  `self.mode(..)` in the three join conditions. `mode` is pure, so this
  changes no answer and no diagnostic.

Nothing under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`src/ir_opt_policy.rs` (lane 9's), `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**` changed.

## Numbers

4-core Linux cloud host, release CLIs (`be6db84` for the base, this branch
for "lane 8"), nothing else running. The combined program is the wrapper
package of the #1312 report: `check_all(in input: Str) -> u64` calls every
`in input: Str` entry of check-data, check-core-typing, check-fip,
check-names-packages and check-regions-seqs, and `check_dyn_count` of
check-dyn. Wall time of `vesper build combined --lib --lib-name combined`,
two runs each, interleaved.

| Build | Policy (`VESPER_OPT_POLICY`) | Run 1 | Run 2 | vs. before #1312 |
|---|---|---:|---:|---:|
| base (#1312 head) | off (= before #1312) | 122.0 s | 122.2 s | 1.00x |
| base (#1312 head) | on | 67.3 s | 70.6 s | 1.77x |
| lane 8 | off | 91.9 s | 93.7 s | 1.32x |
| lane 8 | on | **42.6 s** | 46.2 s | **2.86x** |

All eight objects compare equal with `cmp`. The lane alone saves about
25-30 s whether the policy is on or off, since both the checker's
`check_core` -> `ir_check::validate` and the optimizer's validation call
`World::mode`.

The memoized function on its own (`bench_world_mode`, release, every
corpus query of `corpus_modes_match_the_reference` once per round, a fresh
memo per world):

| Round | Queries | `mode_reference` | `mode` (memo) |
|---|---:|---:|---:|
| 0 | 868,714 | 523 ms | 332 ms |
| 1 | 868,714 | 514 ms | 321 ms |
| 2 | 868,714 | 509 ms | 317 ms |

Corpus worlds are small programs whose types are mostly scalars and short
ADTs, so the memo gains 1.6x there. The combined checker build gains far
more because its types (the checkers' state structs, ASTs and tables) reach
dozens of ADTs each and are asked about at every join.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `src/ir_opt.rs`,
`src/ir_opt_policy.rs`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**` changed, and the verified backend's pipeline
is not routed through any swap point. It still calls `ir_opt::optimize`,
whose `ir_check::validate` now uses the memoized `World::mode`, so the
artifacts were compared:

- `bootstrap/run.sh stage1 --outcome` passes on this branch (corpus
  unchanged, every verified build kept, cross-backend gate ok).
- The same run on the base (`be6db84`) writes a fresh lock that differs
  from this branch's fresh lock **only in `seed_binary`** (the compiler
  binary itself). All 22 artifact digests (11 Cranelift artifacts and 11
  verified artifacts), every result and `frontend_sources` are equal. Both
  fresh locks differ from the committed bootstrap/stage1.lock in the same
  fields (it was recorded at an older commit), so this lane moves no
  Stage 1 artifact.
- The bootstrap-stage1 workflow runs on this branch once #1312 merges (CI
  rule: no Actions runs while stacked).

## Tests

- `tests/world_mode_speed.rs`:
  - `corpus_modes_match_the_reference`: for every corpus world, every type
    the checker recorded, every signature type (under declared bounds and
    under each uniform bound), every ADT field type under each uniform bound,
    and every ADT applied to copy, affine, linear and mixed arguments: the
    memoized answer equals `mode_reference`. The queries run forwards on one
    world, again from its memo, and backwards on a fresh clone, so that
    different expansions get recorded first. 295 worlds, 868,714 queries,
    4,205 memoized expansions.
  - `random_worlds_match_the_reference`: 400 generated worlds of 2-5
    mutually recursive ADTs with 0-2 type parameters, declared modes and
    field modalities, plus random queries under random bounds, forwards and
    backwards. 31,856 queries.
  - `bench_world_mode` (ignored): the timing above.
- The checker's own tests and the SH2 differentials: release runs of `language` (108), `closure_modes` (8),
  `hybrid_ir_check` (4), `optimizer_budget` (2, `policy == pass` on the
  corpus), `self_host_check_harness` (5), `self_host_check_data` (3),
  `self_host_check_core_typing` (4), `self_host_check_core_typing_exprs`
  (1), `self_host_check_fip` (3) and `self_host_check_regions_seqs` (4) all
  pass, so the check-data differential and the checker's diagnostics do not
  change.

## The Vesper checker ports mirror the slow shape

`@vesper/check-data`'s `mode_of`/`mode_visit` (packages/vesper-check-data/src/world.ves)
is a line-for-line port of the original: a per-query `seen` path of names
and argument-mode hashes, no memo. `@vesper/check-core-typing` (`mode` in
src/exprs.ves) and `@vesper/ir-check` (`Flow::mode`) call it per query, so
the ports have both problems 1 and 2 above. They are not edited here (the
lane prompt says to report it): the port of this memo needs a world-owned
mutable table that `in wd: World` callers can update, which the port's
`in` world parameter does not allow, so the port would thread an
`inout` memo next to the world. The reference's cost on the corpus queries
(the "reference" column above) is the cost the ports pay per query today.

## Compiler gaps

1. **`ir_check::State` is cloned per edge.** After the memo (30 samples of
   the policy-on build, 1.2 s apart), `World::mode` frames were almost gone
   and the largest
   part of what is left in `ir_check::validate` is `Flow::function`
   cloning and dropping `State` (`BTreeMap`s of `SlotState` and value
   states with their `Type`s) on every edge and join. A persistent map, or
   moving the state into the single successor, would remove most of it.
   That is not a `World::mode` hunk, so it is left for a later lane. Repro:
   the wrapper package above with gdb stack sampling
   (`gdb -p <pid> -batch -ex "thread apply all bt 80"`).
2. **The reference is exponential on recursive families.** `mode_reference`
   (the original) expands each instance once per simple path. The memo
   bounds this for every key whose expansion does not cut above itself, but
   a query that enters a recursive cycle at a member other than the one
   recorded still walks the cycle's paths once. Minimal repro: 8 mutually
   recursive generic structs `struct A0[T] { f0: A3[A1[T]], f1: ChunkSeq[A5[T]] }`...
   in the shape `random_world` generates with `n = 8`, depth 3.
3. **The ports' `mode_of` has no memo** (above).

## What is left

- `ir_check::State` cloning (gap 1) is the next large piece of the build.
- Port the memo to `@vesper/check-data` once the ports' `World` can carry
  an `inout` memo (gap 3).
- Re-measure with all twelve SH2 checker packages once the open ones merge
  (the #1312 report's list).
