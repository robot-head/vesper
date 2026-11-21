# Self-hosting SH2 lane 5: layout and the IR check swapped into the hybrid build (#1177)

> The [manuscript](../manuscript/README.md) is the sole specification. This
> lane needed no change to it. Confirm live ownership on the issue board
> before acting.

Claim: #1177. Base: `main` at `e43a56f` (lane 4's `@vesper/check-data`,
#1217), with main merged again before merging. Lane prompt:
[`docs/agent-prompts/self-host-sh2/05-layout-ir-check-swap.md`](../agent-prompts/self-host-sh2/05-layout-ir-check-swap.md).

The hybrid compiler now answers `Layout::of` and `ir_check::validate` from
Vesper. The census translations are committed as two packages on
`@vesper/check-data`. The Rust modules stay the default, and no Rust code
was deleted.

Both swaps agree with Rust on every input tried: 0 differences. The IR
check is about twice as fast as Rust per call, including the boundary.
The layout swap is 5.4x slower than Rust per call, because every call
rebuilds the world on the Vesper side. Neither swap is at parity on the
whole corpus yet (see the scoreboard below).

## What changed

| Area | Files |
| --- | --- |
| `@vesper/check-layout`: the census `layout.ves`, plus a text entry and a word stream for the hybrid build | `packages/vesper-check-layout/**` (new) |
| `@vesper/ir-check`: the census `ir_check.ves`, `ir_flow.ves` and `ir_flow2.ves`, plus text and word entries | `packages/vesper-ir-check/**` (new) |
| Swap-in points: `LayoutTable`/`install_layout`/`rust_layout_of` and `IrCheckTable`/`install_ir_check`/`rust_validate`. The default build installs nothing. | `src/layout.rs`, `src/ir_check.rs` |
| `fip::handle_drops_active()`: while a `.vsys` handle drop plan runs, `Box` detection reads thread-local handle types the Vesper layout does not see, so those layouts go to Rust | `src/fip.rs` |
| The hybrid modules `@vesper/hybrid-check-layout` and `@vesper/hybrid-ir-check` (export ABI v4) and their blessed bindings | `crates/vesper-hybrid/modules/{check_layout,ir_check}`, `port/{check_layout,ir_check}.rs` |
| Install from the static constructor, with a Rust fallback for inputs the encoder cannot carry; `VESPER_HYBRID_STATS` reports both call counts | `crates/vesper-hybrid/src/hybrid.rs`, `build.rs` |
| The shared encoder and decoders (the hybrid build and the root tests use the same file) | `tests/common/check_words.rs` (new) |
| Tests | `tests/hybrid_layout.rs`, `tests/hybrid_ir_check.rs`, `crates/vesper-hybrid/tests/{check_swaps,check_swaps_leaks}.rs`, `differential.rs` |
| Divergence checks: `divergent` now also breaks the layout swap, and the new `divergent-ir-check` feature breaks the IR check | `build.rs`, `Cargo.toml`, `BUILD.bazel`, `.github/workflows/hybrid.yml` |
| Bazel: `vesper_package` targets for check-data, check-layout and ir-check; `module_deps`; test data | `BUILD.bazel`, `crates/vesper-hybrid/BUILD.bazel` |

**Module names.** The prompt names the modules `modules/layout` and
`modules/ir_check`. `modules/layout` already exists: it is the SH0 pilot
(`install_word_layout`, the word-layout query). So the new one is
`modules/check_layout`.

**Packages, not check-data modules.** The prompt allows either. Separate
packages keep `@vesper/check-data` (lane 4) unchanged and let later lanes
depend on only what they use.

**Port shard 7.** Neither package is a crate port with a `port/`
directory of its own. Their tests are root integration tests
(`tests/hybrid_*.rs`) and hybrid crate tests, so they run in `Test all
targets`, the BuildBuddy `bazel test //...` and the `Hybrid compiler`
workflow on Linux, macOS and Windows. The port harness has nothing to run
for them.

### The boundary

Each call encodes its input in Rust and decodes the Vesper result:

- **Layout.** `layout_input(world, type)` writes the check-data harness
  format: the names, the types in post-order, the world section with only
  the ADTs reachable from the type, and the one type. `layout_of_words`
  returns `1`, the names and types it produced, and the layout in
  pre-order. A layout error is `0` and the message bytes.
  `decode_layout` rebuilds the `Layout`, including its `Type`s.
- **IR check.** `ir_input(world, program)` writes the same header, then the
  non-linear effects and the lowered program: functions, parameters,
  blocks, instructions and terminators. A `Match` terminator carries its
  scrutinee after the arms. `validate_words` returns the kept diagnostics:
  code, message, span, help and related labels. `decode_diagnostics`
  rebuilds the `Diagnostic`s.

Inputs the encoder cannot carry (a few type forms the check-data format
does not have yet) go to the Rust function. `VESPER_HYBRID_STATS=1` prints
how many calls Vesper answered.

### Three fixes to the census translations

1. **Borrowed matching (#1189).** It merged after the census. A match
   scrutinee read from a borrowed slot binds `in` borrows instead of moving
   the value. The census IR check reported "IR mutates a shared parameter"
   on 4 of 245 mutant cases where Rust reports something else. The port
   now records each function's scrutinees (`Flow.scrutinees`) and follows
   Rust's `bind` and `Read` rules for them.
2. **Origin limits and multi-path constructors** (Codex review).
   `Origins::try_build` stops indexing when it runs out of a 65,536-unit
   work budget, a 1,024-byte name limit or a 1 MiB text budget. When it
   stops, the function's diagnostics have no related labels or help. The
   census charged only parameters and checked no text limit. It also
   skipped the fields of kind 3 constructor patterns (the multi-segment
   path form). `origins_build` now charges every node Rust charges and
   clears the index on any limit. The encoder carries kind 3 fields too.
3. **Lent closures and lent function parameters** (#1261, merged while
   this PR was open). Calling a borrowed, non-`copy` function value reads
   it rather than consuming it, and a lent closure's body borrows its
   captures. The port records each function's indirect callees
   (`Flow.callees`) and follows the new `Read` and `Indirect` rules; the
   encoder passes a lent closure's captured parameters as `in`. Before the
   port, 55 of 350 default-step cases had to go to Rust. After it, 8 do
   (the element-view world below), and the other 342 have 0 differences.

## Differential results

All on Linux x86-64 in a debug build unless stated. The corpus is lane 4's
check-data corpus: 285 worlds before main was merged in and 288 after,
from `bootstrap/corpus`, `examples/` and
`tests/`.

| Test | Inputs | Differ |
| --- | --- | --: |
| Layout text (`check_layout_text`) | every type of every world | 0 |
| Layout text, optimizer off | every tenth world | 0 |
| Layout words decoded to `Layout` (`layout_words_decode_to_rust_layouts`) | 6,814 types, 1,281 of them errors (after the merge) | 0 |
| IR check text and words, all cases (`HYBRID_IR_CHECK_STEP=1`) | 1,710 cases (285 worlds, 5 mutants each), 322,908 diagnostics | 0 |
| The same, default step, after the merge | 345 cases, 69,112 diagnostics | 0 |
| IR check text, optimizer off | 245 cases | 0 |
| IR check with every parameter past the name limit (`ir_check_origin_limits_match_rust`) | 17,473 diagnostics, 17,417 without labels (after the merge) | 0 |
| Hybrid differential (`check --json`, `emit ir`, `emit object`) | 537 files, 1,611 runs | 0 |
| `check_swaps` (the linked AOT modules against Rust) | examples and `bootstrap/corpus` | 0 |

The Rust IR check of all 1,710 cases takes about 25 minutes in a debug
build. So by default the optimized test runs every fifth case and the
unoptimized one every 29th. `HYBRID_IR_CHECK_STEP=1` runs them all.

The divergence checks pass: with `divergent` the layout swap answers
garbage and `hybrid_matches_the_default_compiler` fails, and the same
happens with `divergent-ir-check`. So the differential is live for both
swaps.

## Numbers

The host is a 4-core x86-64 cloud machine. All binaries use the
port-compiler profile, which is the one the hybrid is built with.
They were taken at `a89fa91`, before main was merged in.
"Before" is this branch's hybrid with the two `install_*` calls removed:
it has the parse swap and the SH0 layout pilot, as on main.

### Each module alone (ABBA, `CHECK_HARNESS_SPEED=1`)

These run the Vesper entry through the test JIT, with the input encoding
and result decoding the hybrid build does on every call.

| | Rust | Vesper (encode + run + decode) | Ratio |
| --- | --: | --: | --: |
| Layout, every type of every world (6,725 calls) | 15.6 ms | 84.6 ms | **5.43x** |
| IR check, all 1,710 cases | 277.3 s | 146.5 s | **0.53x** |
| IR check, 1 mutant (570 cases) | 90.1 s | 49.7 s | 0.55x |
| IR check, 1 mutant, Vesper alone (input encoded beforehand, no decode) | 86.1 s | 47.1 s | 0.55x |

The census measured 0.34x for layout. It computed all of a world's
layouts in one call, and the hybrid makes one call per type. Each call
rebuilds the reachable part of the world (names, type arena, ADTs) from
the input, computes one layout and writes the result's names and types
back. Rust's `Layout::of` is a short recursion over the world it already
has. The IR check does far more work per call, so the boundary is a small
part of it, and the census ratio holds.

### Inside the hybrid build (callgrind)

`emit --emit object examples/counter.ves`, which makes 99 layout calls and
3 IR checks:

| | Ir | vs before |
| --- | --: | --: |
| default compiler (Rust) | 40,975,523 | |
| hybrid before | 39,681,039 | 1.00x |
| hybrid after | 51,771,043 | 1.30x |
| of which the layout closure (encode, Vesper, decode) | 7,984,554 | |
| of which `layout_of_words` (Vesper) | 6,550,311 | about 66,000 per call |
| of which `layout_input` (encode) | 1,039,008 | |
| of which the IR check closure | 1,578,860 | |
| of which `ir_input` (encode) | 934,423 | |

### The scoreboard rows

`vesper check --json` and `vesper emit --emit object` on each of the 537
differential files in turn. Ir is the sum over the 537 runs.

| | `check` Ir | vs default | `emit object` Ir | vs default |
| --- | --: | --: | --: | --: |
| default compiler (Rust) | 3,514,746,534 | 1.00x | 18,372,609,794 | 1.00x |
| hybrid before (parse swap and pilot) | 3,735,575,514 | 1.063x | 18,659,865,982 | 1.016x |
| hybrid after (plus layout and IR check) | 3,760,205,583 | 1.070x | 24,106,438,917 | 1.312x |

Wall time, ABBA (the default, before and after binaries in the order
default, before, after, after, before, default, four times, each pass over
all 537 files), median of 8 passes each:

| | `check` wall | vs default | `emit object` wall | vs default |
| --- | --: | --: | --: | --: |
| default compiler (Rust) | 2.43 s | 1.00x | 5.90 s | 1.00x |
| hybrid before | 2.53 s | 1.04x | 6.46 s | 1.09x |
| hybrid after | 2.64 s | 1.08x | 6.71 s | 1.14x |

The two swaps are measured together: one hybrid build has both, and the
"before" build removes both. `check` makes many IR checks and almost no
layout calls, so its change is mostly the IR check, and `emit object`'s
is mostly the layout swap (`counter.ves` above: 8.0 M Ir of layout calls
against 1.6 M of IR checks).

So `check` moves by 0.7% in instructions and 4% in wall time. `emit
object` asks for a layout of every type the backend lowers, and the
per-call cost of the layout swap shows: 29% more instructions than
before, though only 4% more wall time, because process start-up, the
Cranelift compile and writing the object dominate the wall time of these
small files.

## Stage 1 guard

The branch changes none of `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**`
(`git diff --name-only origin/main...HEAD` has no match). The
bootstrap-stage1 workflow ran on the branch
([run 44807366019](https://github.com/robot-head/vesper/actions/runs/44807366019),
head `972bfad`): success, so a fresh `bootstrap/run.sh stage1 --reproduce
--outcome` gives every verified artifact digest equal to
`bootstrap/stage1.lock`. The later commits change only the two packages
and the tests.

## Compiler gaps

1. **A `const` could not be imported, from another module or another
   package** (`E0111`, as lane 4 reported). So both packages repeat the
   kind, mode and pass constants they use. #1239 (importable constants)
   merged on main while this PR was open, so a follow-up can replace the
   copies with imports from `@vesper/check-data`.
2. **No state survives between calls into a Vesper module.** Every layout
   call re-encodes and re-reads the world, which is why the layout swap is
   5.4x slower per call than Rust while the census's whole-world layout
   was 0.34x. A Vesper `World` built once per compile and lent to each
   call (an ABI v4 lent handle, or a module-level handle the hybrid keeps)
   would remove the rebuild. Repro: `layout_speed` in
   `tests/hybrid_layout.rs`.
3. **The world queries are still computed in Rust** (census C10). The IR
   input carries per-instruction flags: whether an instruction can abort,
   whether a call or handler is effectful, state actions and evidence.
   They come from the Rust `World` (`ir_input` in
   `tests/common/check_words.rs`). A Vesper `World` that answers them
   would remove that work.
4. **`Option[Str]` help is flattened.** The word format writes help as a
   byte string, and an empty one decodes to `None`. Rust never emits
   `Some("")` today (the decoded diagnostics equal Rust's on every case),
   but the format cannot tell the two apart. An `Option` in the v4 stream
   would fix it.

Two census findings were not compiler gaps but missing translations: the
borrowed-matching rules and the origin limits above. A census sample can
fall behind a Rust module that changes after it, so later swaps should
re-run the differential on fresh mutants before they trust a census
result.

## What is left

- **Layout parity.** Keep the Vesper `World` across calls (gap 2), or
  batch: ask for every layout a function needs in one call. Until then the
  layout swap costs 29% more instructions on `emit object`.
- **The IR check is faster than Rust per call** (0.53x). Its remaining
  cost in the hybrid is the encoder, which could go once a Vesper `World`
  exists (gaps 2 and 3).
- **Element view matches** (#1173, merged while this PR was open) changed
  `ir_check::validate`: a matched `try_get` binds the element as a borrow
  its sequence keeps owning. `@vesper/ir-check` does not have this rule
  yet, so worlds with element views (`World::element_views` not empty) go
  to Rust: one corpus world after the merge (7 of 348 cases at the default
  step, which the differential skips and counts). Porting the rule means encoding `element_views` and following
  the new `Read`/`Bind`/`Seq` cases.
- The constant copies can become imports (gap 1).
- Deleting `rust_of` or `rust_validate` is an owner call at parity, and
  not part of this lane.
