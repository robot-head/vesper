# Place IR data package, pass harness and IR swap point

> Self-hosting wave SH3, lane 1. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh3/place-ir-data`. Claim issue: #1240. Stacks on: #1232.

Every SH3 IR lane reads a `Program`, and lanes 9-13 also write one back. SH2 lane 5 (#1232) already encodes Place IR as words for `@vesper/ir-check` (Rust to Vesper only). This lane makes that a shared package with both directions, a harness every IR lane uses, and the one swap point the IR passes plug into.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1232) in a NEW cloud checkout or a dedicated git worktree (`git fetch origin pull/<n>/head`). Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh3/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane", which all apply), docs/parallel-results/self-host-sh2-scoreboard.md (the SH2 numbers and gaps this wave comes from), docs/parallel-results/self-host-census.md, docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: #1232's tests/common/check_words.rs (the IR encoder) and packages/vesper-ir-check (its decoder), packages/vesper-check-data, crates/vesper-hybrid/** (the parse and layout swaps), src/ir.rs (read only: the Place IR types, `Program`, `FunctionIr`, `Op`, `Terminator`, `SeqOp`), src/native.rs lines around the IR pipeline (`ffi_lowering::drops::lower_world`, `ir_opt::optimize`, `ir_inline::inline`, `fip::elaborate_non_fip_drops`, `bounds_hoist::run`), src/native_export.rs and src/native_session.rs (the same pipeline), docs/parallel-results/self-host-tree-abi.md (export ABI v4).

When to start: now, stacked on #1232. Land a first PR quickly with the tables, the reader and the canonical dump (lanes 11 and 12 switch to it; lane 13 starts on it), then the writer, the harness and the swap point.

Claim: GitHub issue #1240 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1240", or "Part of #1240" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh2/README.md and the additions in docs/agent-prompts/self-host-sh3/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**; never route the verified backend's pipeline through a swap point; and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths. Depend on unmerged work only through the PR named in "Stacks on".
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Parser mirror rule: any change to crates/vesper-syntax/src/parser.rs or lexer.rs must be mirrored in packages/vesper-front (and packages/vesper-ast where it lowers the construct) in the same PR, or tests/self_host_front_v2.rs and tests/self_host_ast.rs fail.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only, never a root `**`); new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. A new port joins Linux port shard 7.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1240. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: `@vesper/place-ir`, the IR pass harness and `src/ir_swap.rs`.

Exclusive ownership:
- new packages/vesper-place-ir/**
- new tests/common/ir_words.rs (the encoder and decoder of `Program` words, both directions; move the IR half of #1232's check_words.rs here only if #1232 has merged, otherwise import it)
- new tests/self_host_place_ir*.rs
- new src/ir_swap.rs and its call-site hunks in src/native.rs, src/native_export.rs, src/native_session.rs and src/ffi_lowering/drops.rs (native path only)
- the `ir_swap` install hunk of crates/vesper-hybrid/src/hybrid.rs
- the Linux port shard 7 entry

Implement:
1. Tables: `Program` as flat Vesper tables (functions, blocks, instructions with an operand pool, places with a path pool, terminators, sequence ops), types as `@vesper/check-data` ids and names interned. Keep the census data design; no `Box` trees.
2. Words both ways: Rust `Program` to words to Vesper tables, and Vesper tables to words to a Rust `Program` that is `Debug`-equal to the input. A canonical text dump on both sides for readable diffs.
3. Round trip over every corpus world, before and after each native-path pass (lowering, `ir_opt`, inlining, drop elaboration, hoisting), so every IR shape the later lanes see is covered.
4. The pass harness: run a Rust pass and a Vesper pass on the same input `Program`, compare the outputs (dump and decoded words), run `@vesper/ir-check` on the Vesper output, and run the program through the JIT (optimize on and off) to compare results. Lanes 9-13 call it with their pass.
5. `src/ir_swap.rs`: a table of optional hooks (`lower`, `opt`, `inline`, `hoist`) that the native path calls instead of the Rust pass when installed. Default installs nothing, so the default compiler is byte-identical. The verified backend, wasm, eBPF, Lean and the evaluator do not go through it. An identity hook in the hybrid (decode then re-encode) proves the round trip inside `vesper check`/`build` over the corpus.
6. Measure the encode and decode cost per corpus world (ABBA, callgrind), so the IR lanes can subtract it.

Acceptance:
- Round trip 0 differences on every corpus world at every pass boundary; identity hook in the hybrid build 0 differences against the default compiler over the hybrid differential corpus.
- Tests pass on all four platforms and the Stage 1 guard holds; the default compiler's objects are unchanged.
- Report: docs/parallel-results/self-host-place-ir-data.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
