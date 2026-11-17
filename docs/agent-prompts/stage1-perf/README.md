# Stage 1 performance wave: closing the port gaps in the native backend

> Plan written 2026-11-17 against `main` at 943d901. The
> [manuscript](../../manuscript/README.md) is the sole specification. The gaps
> come from the P1 port reports (`docs/parallel-results/port-p1-*.md`, PRs
> #924-#930). Confirm live ownership on the issue board before acting.

The P1 ports run 4x to 16x slower than the Rust builds they replace:
warp-wheel (#924) 8x, BBR (#925) 11-16x, Prism (#926) 4.5x, warp-tcp (#927)
10-14x, Aura (#928) 3.8-5.2x, Ignis (#929) 5-13x, Quarry (#930) 4.3-4.9x. Under
the owner's standing rule, Rust stays the default build for every port and is
not deleted until the Vesper build matches it in both functionality and
performance. The ports are the test suite for this wave: each lane reports how
far it moved them.

## Why this work fits in Stage 1

Stage 1 is the current Rust-hosted compiler with the verified backend, locked by
the strict DDC gate (`bootstrap/stage1.lock`, #891). None of the seven gaps
needs Stage 2, and none of them needs to touch the lock or the Lean proofs:

- **The lock corpus is built with the Place IR optimizer off.**
  `bootstrap/stage1.rs` compiles the 11 programs with
  `native::object_with_options(.., false)` and `verified::Options { optimize:
  false, .. }`. The ports build native code with `optimize = true`. A pass
  that runs only under `optimize` on the native path never reaches the corpus.
- **The verified backend's subset has no sequences, slices, strings or export
  ABI** (`src/verified_codegen.rs`, module docs). Work on those reaches only
  Cranelift.
- **The lock tolerates Cranelift changes.** `Stage1Lock::compare_outcome`
  reports a changed `cranelift_artifact` or `result` as stale (re-record later),
  and fails only when a verified build is lost. The strict gate still requires
  equal CST digests from the seed, Cranelift and the verified backend, so a
  wrong native optimization that does reach the corpus fails it: the corpus is a
  differential test, not an obstacle.
- **The Lean proofs cover the verified backend from Place IR down** (instruction
  selection, composition, control flow, encoding). The Rust lowering into
  Place IR is already outside them. So the one rule that keeps the proofs intact
  is: no lane changes what the verified backend is given or how it lowers it.

## Lanes

Claims are #931-#937 (lane n is issue 930+n).

| Lane | Prompt | Claim | Gap | Starts | Exclusive boundary |
|---|---|---|---|---|---|
| 1 | [Inlining and Cranelift opt level](01-inlining.md) | #931 | no inlining; Cranelift default opt level | now | NEW `src/ir_inline.rs` (+ `src/ir_inline/**`), the `if optimize` branches that call `ir_opt::optimize` in `src/native.rs`, `src/native_export.rs` (the library path ports use), `src/native_session.rs` and `src/native_batch.rs`, the Cranelift `settings::builder()` sites in `src/native*.rs` and `src/bench_commands.rs`, `tests/native_inline*.rs` |
| 2 | [Sequence access and drops](02-seq-access-drops.md) | #932 | `try_get` + `Option` per read; local and by-value sequences and strings never freed | now | `src/seq/{native,native_runtime,aot_runtime,ir}.rs`, `src/strings/native.rs`, the drop elaboration in `src/fip.rs`, the by-value refusal hunk in `src/native_export.rs` (own commit, coordinate with lane 4), `tests/native_seq_drop*.rs` |
| 3 | [Byte loops](03-byte-loops.md) | #933 | per-byte bounds-checked loops; no block copy, wide reads, check hoisting or 64x64->128 multiply | after lane 2 (shares `src/seq/**`); hoisting and the multiply can start now | `src/seq/check.rs`, `src/seq/eval.rs`, `src/seq/lean.rs` (builtin typing and reference semantics), NEW `src/bounds_hoist.rs`, the new-builtin hunks in `src/seq/native.rs` after lane 2 merges, `packages/vesper-std/bytes/**`, `tests/native_bytes*.rs` |
| 4 | [Export ABI v3](04-export-abi-v3.md) | #934 | state copied across the Vesper/Rust boundary every call | now | `src/native_export.rs`, `src/native_export/**`, `src/lib_commands.rs`, `crates/vesper-port-abi/**`, `crates/vesper-port-pilot/**`, `tests/native_export*.rs`, `tests/fixtures/native_export/**`, `docs/maintenance/porting-a-crate.md` (ABI sections) |
| 5 | [Fail across inout and Fail lowering](05-fail-lowering.md) | #935 | `Fail`/`inout` force a Result plus a copy per call | language change now (owner chose keep-writes, 2026-11-17); codegen after lanes 4 and 1 | the E0402 check in `src/check.rs`, the Tier-1 inout lowering in `src/lean.rs` (coordinate with #593), the manuscript §4.3/§4.5/§5.2/§8.2 hunks, the native `Fail` row lowering in `src/native.rs` (`fail_record_size`, `fail_exit` and call-site status handling), the `Fail` hunks of `src/native_export/glue.rs` after lane 4 merges, `tests/native_fail*.rs` |
| 6 | [Vectorized byte builtins](06-simd-bytes.md) | #936 | no SIMD | after lane 3 | `src/native_vector.rs` (byte-builtin hunks), the builtin hunks lane 3 adds, `tests/native_simd_bytes*.rs` |
| 7 | [Formatter cap](07-fmt-cap.md) | #937 | `vesper fmt` refuses files over 8,192 tokens (4·N² work) | now | `crates/vesper-fmt/**`, `src/fmt_commands.rs`, `tests/fmt*.rs` |

**Start now:** lanes 1, 2, 4 and 7, lane 3's hoisting and multiply, and lane 5's language change.
**Recommended merge order:** 1, 2, 3, 4, 5, 6, with 7 whenever it is green.

### Not in this wave

- Source-level scalable vectors (`VVec`, F11) stay with wave-nine lane 10
  (#587). Lane 6 adds no syntax.
- (Decided 2026-11-17.) The owner approved relaxing E0402 for stationary
  `inout` arguments with keep-writes semantics; lane 5's prompt has the rules.
- Re-recording `bootstrap/stage1.lock`. Lanes keep the verified artifacts
  identical; a later owner-approved PR re-records the Cranelift digests once the
  wave lands.
- Deleting any port's Rust code. That follows the owner's parity rule, per
  port, after this wave.

## Rules for every lane

- **Claim first.** Search open issues and PRs for overlapping work. The lane's
  claim issue already exists; link it from the PR ("Closes #NNN"). Branch from
  `origin/main` and open the PR against `main`.
- **Stage 1 guard.** Do not edit `src/verified_codegen/**`, `src/ir.rs`
  (`lower_world` and the Place IR types), `src/ir_opt.rs`'s `optimize` (the
  verified backend runs it when its own `optimize` is on), `lean/**`,
  `bootstrap/**` or `packages/vesper-compiler/**`. A new pass runs only on the
  native path, and only when `optimize` is on, unless it is a correctness fix
  (lane 2's drops). The required Bazel `stage1_test` must stay green, and the
  report must show every `programs.verified.artifact` digest of a fresh
  `bootstrap/run.sh stage1 --reproduce --outcome` equal to the committed lock
  (take it from the `bootstrap-stage1` workflow's uploaded locks; run that
  workflow on your branch with `workflow_dispatch`). List any stale Cranelift
  digests and explain them.
- **Semantics first.** Every new builtin or lowering keeps the interpreter
  (`src/runtime.rs`), the JIT and AOT native paths in agreement. Add
  differential tests that run each new form through all three.
- **Measure on the ports.** Report before/after Vesper/Rust ratios on the ports
  the lane targets, with the benchmarks they already have: BBR
  `crates/vesper-warp-bbr/examples/ack_path_abba.rs` (on `main`), warp-tcp
  `examples/abba.rs` (#927), Aura `examples/abba.rs` (#928), Quarry
  `examples/planner_abba.rs` (#930), and the benchmarks in the warp-wheel,
  Prism and Ignis reports. Measure an unmerged port from its PR branch with
  your compiler change merged in locally; do not push to port branches. Report
  only what you measured, with the setup. Do not change a port's default
  build.
- **No unsafe in generated-code contracts that the tests cannot see.** A new
  runtime helper gets a leak/double-free test with the live-allocation counter
  (`port_live_allocations()` pattern).
- **Spec.** If a lane adds a builtin or changes the export ABI, write the
  smallest manuscript change into the same PR and flag it in the PR body.
- **Before pushing:** `cargo +1.100.0 clippy --all-targets -p vesper -- -D warnings`
  and the lane's own new tests. Long runs go to GitHub CI from the pushed
  branch; the cloud disk fills up if you build every integration test of the
  root package locally.
- **Merging.** Merge when the four required checks (`Test all targets`,
  `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`,
  `Windows (x86_64 MSVC)`) are green and review comments are addressed. If
  `main` is red and your PR fixes it, cancel the other in-progress Actions runs
  and merge the fix first.
- **Report.** `docs/parallel-results/stage1-perf-<lane slug>.md`: what changed,
  the Stage 1 guard evidence, the port ratios before and after, and what is
  left.

## Merge order and collisions

| Shared file | Lanes | Rule |
|---|---|---|
| `src/native.rs` | 1 (opt level, `compile_entry` `optimize` branch), 2 (seq drop hooks), 5 (Fail lowering) | Each lane edits only its named hunks. Merge `main` before merging. 5 goes after 1. |
| `src/seq/native.rs` | 2, then 3, then 6 | Serialize in that order. |
| `src/native_export.rs`, `src/native_export/**` | 4 owns; 1 (inliner call, Cranelift flags), 2 (by-value refusal) and 5 (`Fail` glue) add hunks | 1's and 2's hunks in their own commits; 5 after 4. |
| `packages/vesper-std/bytes/**` | 3 only | Others ask lane 3. |
| Root `Cargo.toml`/`Cargo.lock` | any lane adding a dependency | Serialize. |
