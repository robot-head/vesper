# Self-hosting SH1 lane 15: checker and IR census, and the SH2 plan

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1130 (the second half of SH0 lane 8). Base: `main` at `e29e63d`
(SH1 plan PR #1158). Lane prompt:
[`docs/agent-prompts/self-host-sh1/15-census.md`](../agent-prompts/self-host-sh1/15-census.md).
The plan this census produced is
[`docs/agent-prompts/self-host-sh2/README.md`](../agent-prompts/self-host-sh2/README.md).

## Summary

Five pieces of real compiler code were translated to Vesper, run against the
Rust code they mirror on the compiler's own test inputs, and timed. The
translations are local and not committed, as the prompt asks. A copy is
kept with the project files so the SH2 lanes can start from it.

| Sample | Rust it mirrors | Rust lines | Vesper lines | Differential | Speed against Rust |
|---|---|--:|--:|---|---|
| Type table | `vesper-core` `ast::Type` (`Display`, `Debug`, `substitute`, `substitute_effects`) and `numeric::default_type` | ~450 | 775 | exact (every corpus type, through layout and regions) | (inside the others) |
| World tables | `check::World` lookups, `constructor`, `mode`, `field`, `vector_words` | ~350 | 502 | exact (through layout and IR check) | (inside the others) |
| Layout | `src/layout.rs` (whole file) | 384 | 380 | **0 of 272 worlds differ** (4,441 types) | **0.34x time** (ABBA, best of 6) |
| Region checks, type half | `src/regions.rs` lines 325-600 (labels, `validate_type`, mismatch text) | 276 | 379 | **0 of 272 differ** | **0.87x time** |
| IR check | `src/ir_check.rs` (whole file) with its `check.rs` helpers | 1,010 | 2,183 | **0 of 1,632 differ** (272 worlds plus 5 mutants each: 1,597,638 instructions, 190,875 diagnostics byte-identical with labels and help) | **0.57x time** |
| Native lowering family | `src/native.rs` `alloc`, `data_into`, `offset`, `load`, `store`, `copy_bytes`, `pattern_condition` through `@vesper/cranelift` | ~200 | 278 | **objects byte-identical** (235 corpus layouts, 1 module, with and without `opt_level=speed`) | emission **2.35x Ir**; emission plus Cranelift compile **1.06x Ir** |

"Time" is Vesper time divided by Rust time, so below 1 means Vesper is
faster. Every Vesper number comes from the optimizing JIT (`WordFunction`,
optimize on). The decode of the hand-encoded input is subtracted: each
Vesper run is timed at N rounds and at 0 rounds.

**Why the checker samples beat Rust.** The speed comes from the data
representation, not from code generation. The Vesper samples cannot hold
`Box` trees or `String`-keyed maps (gaps C4 and C6 below), so they use a
hash-consed type arena (a type is a `u32` id) with interned names, flat
tables and sorted rows. The Rust code clones `Type` trees, compares
`Display` strings to dedupe effect rows, and keys `BTreeMap`s by `String`.
That design is what the SH2 ports should keep. A line-by-line port of the
Rust data structures is not possible today and would not be faster.

**What it costs to write.** The checker samples are 1.2x to 2.2x the Rust
line count. Most of the extra lines are the hand-written tables (sorted rows
with binary search, a state pool) that stand in for `BTreeMap`,
`BTreeSet` and `Vec<State>`.

Nothing in the compiler changed. Rust stays the default everywhere, and no
Rust code was deleted.

## How the samples were run

- **Corpus.** Every single-file program under `tests/fixtures` and
  `examples` and every Loom package under `packages` and `crates` that
  parses and checks with no diagnostics: 272 worlds. The IR check runs on
  `vesper::ir::lower_world` of each world (266,273 instructions).
- **Mutants.** To exercise the diagnostics, the IR check also runs five
  mutants of each world's IR (`In` read turned into `Sink`, `Sink` into
  `Inout`, binds into wildcards, tokens dropped, call arguments sunk with an
  early return). The clean corpus has no IR-check diagnostics, so the
  mutants are where the 190,875 diagnostics come from.
- **Input.** Compiler data cannot cross the export ABI yet (gap C7), so a
  Rust encoder (about 200 lines) writes names, types (post-order), the world
  and the task as little-endian words. The Vesper entry takes a `Str` of
  those bytes and returns a digest.
- **Digest.** Each side folds its result with FNV-1a into one word: layout
  sizes, offsets and field names; region labels and diagnostic text; and,
  for the IR check, each diagnostic's code, span, message, help and related
  labels, in order. A one-word change to the Vesper layout (the first enum
  field offset 8 to 16) makes all 272 worlds differ, so the digest is
  sensitive.
- **Timing.** ABBA, best of six runs per side; 20 corpus passes per run for
  layout and regions and 5 for the IR check. Wall clock on these hosts moves
  by up to 30% between runs.

## Numbers

### Wall clock (ABBA)

| Sample | Rust | Vesper | Vesper / Rust |
|---|--:|--:|--:|
| Layout, 20 corpus passes | 100.4 ms | 34.5 ms | 0.34 |
| Region checks, 20 passes | 17.4 ms | 15.2 ms | 0.87 |
| IR check, 5 passes | 72.95 s | 41.81 s | 0.57 |
| Lowering, emission only | 1.82 ms | 21.1 ms | 11.6 (see note) |
| Lowering, emission and Cranelift compile | 57.6 ms | 70.6 ms | 1.22 |

Note on lowering: the Vesper wall time is the JIT run of the full plan minus
the JIT run of an empty plan. The full plan's source embeds a 106 KB hex
literal (gap C8: the FFI JIT takes no arguments), so its JIT compile is
longer than the empty plan's, and that difference lands in the Vesper
column. The instruction counts below subtract the same baseline and are the
better number.

### Instruction counts (callgrind)

Collected only inside the measured call (`--toggle-collect`), over every
tenth corpus world (27 worlds, `CENSUS_STRIDE=10`), one pass. The Vesper
column is the run minus a zero-round run (input decode only); the lowering
rows subtract a run on an empty plan (the JIT compile of the entry).

| Sample | Rust Ir | Vesper Ir | Vesper / Rust |
|---|--:|--:|--:|
| Layout | 3,875,946 | 2,068,424 | 0.53 |
| Region checks | 1,072,686 | 981,709 | 0.92 |
| IR check | 4,874,866,876 | 2,553,014,754 | 0.52 |
| Lowering, emission only (whole plan) | 23,588,735 | 55,420,391 | 2.35 |
| Lowering, emission and Cranelift compile | 557,211,507 | 590,100,153 | 1.06 |

The instruction counts agree with the wall clock that Vesper is faster on all
three checker samples. Layout's wall ratio (0.34x) is lower than its
instruction ratio (0.53x); this census did not look into why.

## Stage 1 guard evidence

None of the guarded paths are touched (the PR changes only
`docs/`). The bootstrap-stage1 workflow ran on this branch at `74910a4`:
[run 44654924275](https://github.com/robot-head/vesper/actions/runs/44654924275).
"Stage 1 reproducibility and cross-backend ratchet" (a fresh
`bootstrap/run.sh stage1 --reproduce --outcome`, compared with
`bootstrap/stage1.lock` by `compare_outcome`) and "Stage 0 two-seed build
and Path B" both passed, so every verified artifact digest equals the lock.

## Compiler gaps

Each repro is the smallest program that shows the gap, checked with
`vesper check` on `main` at `bb5cee6`. Gaps already owned by an SH1 lane
are listed with that lane and get no new lane.

**C1. `eff` cannot be a field, method or binding name.** The compiler has
about 100 uses of `eff` as a name (`check.rs`, `capability_effects.rs`,
`ir.rs`). The sample renamed them (`effect_at`, `effectful`).

```text
struct Row { eff: u32 }
fn main() -> u32 { let r = Row { eff: 1 }; r.eff }
// [E0002] expected a lower-case identifier, found the keyword `eff`, which cannot be a name
```

SH2 lane 3 makes `eff` a contextual keyword (it only starts an effect row
after a signature), with the mirror in `packages/vesper-front`.

**C2. No 128-bit integers.** `Type::IntLiteral(i128)` holds literals up to
`u64::MAX` and their negations, and the corpus has one
(`18408377700990114895`, above `i64::MAX`). The sample stores the literal
as a sign and a `u64` magnitude.

```text
fn big() -> i128 { 18408377700990114895 }
// [E0101] unknown type 'i128'
```

SH2 lane 2 adds `i128`/`u128` (a manuscript change, flagged for the owner).

**C3. Checked `+` raises `Fail`.** Every index and counter in the samples is
`wrapping_add`, `wrapping_sub` or `wrapping_mul` (171 calls), because
`+` on `u64` raises `Fail` and the checker functions have no `Fail` in their
rows.

```text
fn sum(a: u64, b: u64) -> u64 { a + b }
// [E0401] unhandled effect 'Fail'
```

This is the language working as specified. SH1 lane 5 (#1148, merged in
#1163) added the checked arithmetic builtins. The remaining cost is
verbosity only, so there is no lane. If the owner wants one, the sugar
catalog (manuscript §7.6) is where a wrapping operator would go.

**C4. A borrowed enum with `Box` children cannot be matched.** This is why
the type table is an arena of ids.

```text
enum Ty { Int, Seq(Box[Ty]) }
fn depth(in t: Ty) -> u64 {
    match t { Ty::Int => 0, Ty::Seq(inner) => wrapping_add(depth(in inner), 1) }
}
// [E0305] cannot move 't' through a borrowed parameter
```

Owned by SH1 lane 6 (#1149, ABI v4 with borrowed `Box` match). The SH2
lanes keep the arena either way, because it is what made the samples fast.

**C5. A struct that owns a sequence cannot be read out of a sequence.** The
IR check keeps one dataflow state per block, and each state owns sequences.
`Vec<State>` has no Vesper equivalent:

```text
struct State { slots: ChunkSeq[u32] }
fn first_len(in states: ChunkSeq[State]) -> u64 {
    match states.try_get(0) { Some(s) => s.slots.len(), None => 0 }
}
// [E1522] cannot copy 'State' out of a sequence because it is not copy; use pop
```

The sample stores every block state flat in one append-only pool of rows,
with a start and length per block. That works, but the state rows and the pool
are about 340 lines of the IR-check sample (`State` and `Pool` in
`ir_flow.ves`). The same error was found by the export ABI widening
lane (#1087). SH2 lane 1 adds borrowed element views (`in` and `inout`
access to an element in place).

**C6. Std collections hold only copy keys and values.** `CacheBTree[K, V]`,
`OrderedSet[K]`, `FlatTable[K, V]` and `IdxMap[K, V]` in
`@vesper-std/collections` and `@vesper-std/arena` require `K: mod copy`
(and `Ord` or a hash) and `V: mod copy`. The checker's maps are
`BTreeMap<String, Type>`, `BTreeMap<String, Vec<...>>` and
`BTreeMap<Place, State>`. With interned names the keys become `u32`, but the
values still own sequences, so the samples hand-write sorted rows with binary
search (slots, values and the origin sets in the IR check). This depends on C5 and is in SH2
lane 1.

**C7. Compiler data cannot cross the export ABI.** The harness encodes the
world, the types and the IR by hand (about 200 lines of Rust, and about 200
lines of Vesper to decode). Owned by SH1 lane 6 (#1149, tree ABI v4). SH2
lane 4 replaces the hand encoder with v4 once it lands.

**C8. Test entries take few arguments.** `WordFunction` entries take at most
three words and no effects; an `Ffi` entry under the JIT
(`ForeignChecked::jit`) takes none, so the lowering sample embeds its 106 KB
input in the source as a hex literal. This only affects test harnesses, but
every SH2 lane needs one. SH2 lane 4 builds a shared differential harness
that passes a `Str` input to any entry.

**C9. The expression half of the region checks needs the AST.** Lines
325-600 of `src/regions.rs` are type-level; the rest walks `ast::Expr`. It
waits on SH1 lane 9 (#1152), as SH2 already planned.

**C10. World queries had to be precomputed.** The IR check asks the world
whether a call is effectful, whether an operation can overflow and which
operations are partial. Those answers need the whole checker, so the harness
computed them in Rust and passed them as instruction flags. In SH2 the
checker data lane (lane 4) owns a Vesper `World` that answers them.

**C11. Name and range copies are byte loops.** `Names.write` and the
diagnostic text copies use per-byte `push`. Owned by SH1 lanes 3 and 4
(#1146, #1147).

**C12. Emitting through the shim costs about 2.35x Rust in instructions.**
Each wrapper call crosses the C ABI (the SH0 lane 4 report measured the
per-call overhead). When Cranelift's own compile is included the module
costs 1.06x Rust, so emission cost does not block SH4. SH4 lanes should use
the batched `emit` path for long straight-line runs. No new lane.

## What is left

- The SH2 plan is in
  [`docs/agent-prompts/self-host-sh2/`](../agent-prompts/self-host-sh2/README.md)
  with claims #1173-#1187 (lane n is issue 1172+n). `docs/maintenance/self-hosting-plan.md` has the
  S19 row and the SH2 wave filled in.
- The samples are not committed. SH2 lane 4 promotes the type table, names,
  world and diagnostics tables into a package; SH2 lane 5 promotes layout
  and the IR check.
- #1130 closes when this plan merges.
