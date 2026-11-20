# Self-hosting wave SH2: the checker, and the gaps the census found

> Plan written 2026-11-20 against `main` at `bb5cee6` (claim #1130, SH1 lane
> 15). The [manuscript](../../manuscript/README.md) is the sole specification.
> The crate-by-crate inventory and later waves are in
> [self-hosting-plan.md](../../maintenance/self-hosting-plan.md). Confirm live
> ownership on the issue board before acting.

The [census](../../parallel-results/self-host-census.md) translated five
pieces of real compiler code to Vesper and ran them against the Rust code on
the compiler's own inputs: layout, the IR check, the type half of the region
checks, the `vesper-core` type table with the `World` lookups, and a native
lowering family through the Cranelift shim. All five match Rust exactly (0
differences on 272 corpus worlds; 190,875 IR-check diagnostics byte-identical
on mutants; lowering objects byte-identical). The checker samples run at
0.34x to 0.87x of Rust's time, because they use a hash-consed type arena and
interned names instead of `Box` trees and `String`-keyed maps. The lowering
costs 1.06x of Rust's instructions with Cranelift's compile included.

So the checker can be written in Vesper today, and fast, if it keeps that
data design. What it costs is workarounds: no element borrows of sequences
that own sequences, std collections that hold only copy values, no 128-bit
integers, `eff` reserved as a name, and hand-written encoders because
compiler data cannot cross the export ABI yet. SH2 fixes the first four in
gap lanes, builds the shared checker data package and harness, swaps the two
modules the census already finished, and ports the rest of the checker in
nine lanes (about 36k lines of Rust with the swap lane).

Every port here keeps Rust as the default build under the standing owner rule.

## Lanes

Claims: lanes 1-15 are #1173-#1187 (lane n is issue 1172+n).

### Compiler gaps found by the census

| Lane | Prompt | Claim | Gap | Census gap | Starts | Exclusive boundary |
|---|---|---|---|---|---|---|
| 1 | [Element borrows and owning collections](01-element-borrows.md) | #1173 | a struct that owns a sequence cannot be read out of a `ChunkSeq` (E1522); std collections take only copy values | C5, C6 | now | element-access hunks of `src/seq/{check,ir}.rs` and their lowering, `packages/vesper-std/{collections,arena}/**` (non-copy values), manuscript hunks (flagged), `tests/element_borrows*.rs`, `tests/owning_collections*.rs`, report |
| 2 | [128-bit integers](02-wide-integers.md) | #1174 | no `i128`/`u128` for `IntLiteral(i128)` | C2 | now | `i128`/`u128` hunks of `src/integer_widths/**`, `crates/vesper-core/src/numeric.rs` and lowering, front-end mirror hunks, manuscript hunks (flagged), `tests/wide_integers*.rs`, report |
| 3 | [`eff` as a name](03-contextual-eff.md) | #1175 | `eff` cannot be a field, method or binding name (E0002) | C1 | now | keyword hunks of `crates/vesper-syntax/src/{lexer,parser}.rs` and their `packages/vesper-front` mirror, manuscript hunk (flagged), `tests/contextual_eff*.rs`, report |
| 4 | [Checker data package and differential harness](04-check-data.md) | #1176 | types, names, `World` and diagnostics in Vesper; a corpus harness for every lane; world queries the census precomputed | C7, C8, C10 | now (ABI v4 when SH1 lane 6 lands) | new `packages/vesper-check-data/**`, `tests/self_host_check_harness.rs`, `tests/self_host_check_data*.rs`, shard 7 entry, report |

### Checker ports

| Lane | Prompt | Claim | Rust replaced (lines) | Starts | Exclusive boundary |
|---|---|---|--:|---|---|
| 5 | [Swap layout and the IR check](05-layout-ir-check-swap.md) | #1177 | 1,400 (`layout.rs`, `ir_check.rs`) | after 4 and SH1 lanes 6 and 12 | new `packages/vesper-check-layout/**`, `packages/vesper-ir-check/**`, `crates/vesper-hybrid/**` modules `layout` and `ir_check`, `tests/hybrid_layout*.rs`, `tests/hybrid_ir_check*.rs`, two scoreboard rows, report |
| 6 | [Name resolution, packages, interfaces](06-names-packages.md) | #1178 | ~3,000 | after 4 and SH1 lane 9 | new `packages/vesper-check-names-packages/**`, its fields in `@vesper/check-data`, hybrid module `names_packages`, `tests/self_host_check_names_packages*.rs`, one scoreboard row, report |
| 7 | [Core typing and modes](07-core-typing.md) | #1179 | ~6,500 (several PRs) | after 4 and SH1 lane 9; literal typing after 2 | new `packages/vesper-check-core-typing/**`, its fields, hybrid module `core_typing`, `tests/self_host_check_core_typing*.rs`, one scoreboard row, report |
| 8 | [Effects and capabilities](08-effects.md) | #1180 | ~3,600 | after 4 and SH1 lane 9 | new `packages/vesper-check-effects/**`, its fields, hybrid module `effects`, `tests/self_host_check_effects*.rs`, one scoreboard row, report |
| 9 | [Regions, sequences, strings, arrays, vectors](09-regions-seqs.md) | #1181 | ~5,200 | after 4 and SH1 lane 9 | new `packages/vesper-check-regions-seqs/**`, its fields, hybrid module `regions_seqs`, `tests/self_host_check_regions_seqs*.rs`, one scoreboard row, report |
| 10 | [Loops, try/Option, atomics, stack bound, IO sessions](10-loops-atomics.md) | #1182 | ~2,300 | after 4 and SH1 lane 9 | new `packages/vesper-check-loops-atomics/**`, its fields, hybrid module `loops_atomics`, `tests/self_host_check_loops_atomics*.rs`, one scoreboard row, report |
| 11 | [dyn traits and function objects](11-dyn.md) | #1183 | ~2,300 | after 4 and SH1 lane 9 | new `packages/vesper-check-dyn/**`, its fields, hybrid module `dyn`, `tests/self_host_check_dyn*.rs`, one scoreboard row, report |
| 12 | [fip checks](12-fip.md) | #1184 | ~3,100 | after 4 and SH1 lane 9 | new `packages/vesper-check-fip/**`, its fields, hybrid module `fip`, `tests/self_host_check_fip*.rs`, one scoreboard row, report |
| 13 | [Trait resolution through the Pact port](13-pact.md) | #1185 | ~5,100 | after 4 and SH1 lane 9 | new `packages/vesper-check-pact/**`, its fields, hybrid module `pact`, `tests/self_host_check_pact*.rs`, one scoreboard row, report |
| 14 | [Tensor types](14-tensor-types.md) | #1186 | ~3,600 | after 4 and SH1 lane 9 | new `packages/vesper-check-tensor-types/**`, its fields, hybrid module `tensor_types`, `tests/self_host_check_tensor_types*.rs`, one scoreboard row, report |

### Planning

| Lane | Prompt | Claim | Starts | Exclusive boundary |
|---|---|---|---|---|
| 15 | [SH2 scoreboard and SH3 plan](15-scoreboard-sh3-plan.md) | #1187 | when lanes 5-14 have each merged a first swap | `docs/maintenance/self-hosting-plan.md` (SH2 results, SH3 sections, gap table), new `docs/agent-prompts/self-host-sh3/**`, `docs/parallel-results/self-host-sh2-scoreboard.md` |

**Start now:** lanes 1-4.
**After lane 4's first PR and SH1 lane 9 (#1152):** lanes 6-14 (each swaps
into the hybrid build once SH1 lane 12, #1155, has set the pattern).
**After lane 4 and SH1 lanes 6 (#1149) and 12 (#1155):** lane 5.
**Last:** lane 15.

**Recommended merge order.**
- Lane 4 first among the ports: lanes 5-14 build on `@vesper/check-data`.
  Each later lane adds only its own fields there; merge `main` right before
  merging and re-run the harness.
- Lane 2 before lane 7's literal-typing PR.
- Lane 1 before the ports adopt element views; until then they may use the
  census state-pool pattern and switch in a follow-up.
- Lane 3 and SH1 lane 10 (#1153) both edit `packages/vesper-front`: merge
  `main` between them.
- Lanes 1 and 2 both touch native and JIT lowering: merge `main` right
  before merging each and re-run its differential tests.
- Lanes 5-14 each add a module to `crates/vesper-hybrid` and a row to the
  scoreboard in `docs/maintenance/self-hosting-plan.md`: merge `main` right
  before merging each, and serialize merges that touch root `Cargo.toml`
  (no merge queue on this account).
- Lane 15 last.

## Rules for every lane

- **Claim first.** Each lane's issue is its claim. Check open issues and PRs
  for overlapping work before starting; stop and report on overlap. Link the
  claim from the PR.
- **Stage 1 guard.** Never edit `src/verified_codegen/**`, `src/ir.rs`
  (`lower_world` and the Place IR types), `ir_opt::optimize`, `lean/**`,
  `bootstrap/**` or `packages/vesper-compiler/**`. Show every verified
  artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome`
  equal to `bootstrap/stage1.lock` (run the bootstrap-stage1 workflow on your
  branch).
- **Standing owner rule.** Rust stays the default build of the compiler and
  of every port the owner has not already moved, and no Rust code is deleted
  until the Vesper build matches it in both function and performance. Every
  Vesper replacement in this wave is a non-default build.
- **Keep the census data design.** Types are `u32` ids in a hash-consed
  arena, names are interned, and tables are flat with sorted rows. Do not
  port `Box` trees or `String`-keyed maps line by line: that is neither
  possible today nor faster.
- **Reproduce first (gap lanes).** Commit each census repro as a failing test
  before the fix; drop any that already pass on current `main`.
- **Parser mirror rule.** Any change to `crates/vesper-syntax/src/parser.rs`
  or `lexer.rs` is mirrored in `packages/vesper-front` in the same PR;
  `tests/self_host_front_v2.rs` fails otherwise.
- **Semantics first.** The evaluator, the JIT (optimize on and off) and an
  executable agree, with differential tests. Runtime helpers get leak tests,
  in their own test binary.
- **Differential over the corpus.** Ports use lane 4's harness: every checked
  corpus world, the checker tests, and mutants that make each diagnostic of
  the module fire. Compare every diagnostic's code, span, text, help and
  labels, not only counts.
- **Measure.** ABBA numbers and callgrind instruction counts against the Rust
  code each lane replaces; wall clock on these hosts moves by up to 30%
  between runs. Each report has a "Compiler gaps" section with minimal
  repros, in the format of the census report.
- **Spec.** Any new std module, builtin or rule gets the smallest manuscript
  change in the same PR, flagged in the PR body.
- **Build.** Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100
  (`cargo +1.100.0 clippy` before pushing). New crates need
  `tools/bazel/sync_build_files.py`; files a test reads from outside its crate
  must be declared in `BUILD.bazel` with anchored globs (a root `**` glob walks
  the `bazel-*` symlinks and times out). Windows and macOS CI use Cargo. Long
  runs go to GitHub CI from the pushed branch. A new package joins Linux port
  shard 7; rebalance when it passes about 35 minutes.
- **Merge.** The owner has authorized merging once the four required checks
  (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`,
  `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments
  are addressed. If `main` is red and your PR fixes it, cancel the other
  in-progress Actions runs and merge the fix first.
- **Report.** `docs/parallel-results/self-host-<lane slug>.md`.

## Spec calls flagged for the owner

- **Element views (lane 1).** How a borrowed element of a sequence is
  written and how long the borrow lasts is a manuscript change.
- **`i128` and `u128` (lane 2).** Two new scalar types, their literals,
  builtins and the export ABI's refusal of them.
- **Contextual `eff` (lane 3).** Removing a reserved word.

## Not in this wave

- The Place IR lowering, inliner, optimizer, bounds hoisting and e-graphs
  (SH3), except the IR check, which the census already finished (lane 5).
  Lane 15 plans SH3.
- `forge_frontend` (SH1 lane 14) and the evaluator and runtimes of the
  modules above (SH4 and SH5).
- Deleting any Rust module or seed copy: an owner call per module at parity.
