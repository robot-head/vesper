# Closure parameter modes and view callbacks on collections

> Self-hosting wave SH3, lane 3. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh3/closure-modes`. Claim issue: #1242. Stacks on: #1221.

Collections that own non-copy values (#1221) cannot offer `with_ref(k, |in v| ..)`, because closures have no parameter modes. So callers match on positions and views by hand, and `IdxMap` of non-copy values cannot fill gaps (no constructor callback).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1221) in a NEW cloud checkout or a dedicated git worktree (`git fetch origin pull/<n>/head`). Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh3/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane", which all apply), docs/parallel-results/self-host-sh2-scoreboard.md (the SH2 numbers and gaps this wave comes from), docs/parallel-results/self-host-census.md, docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: #1221 and docs/parallel-results/self-host-element-borrows.md (G5, G6), the closure rules in docs/manuscript, the closure hunks of src/check.rs and lowering (`dyn_fn` desugaring: docs/parallel-results/self-host-check-dyn.md on #1231), packages/vesper-std/collections/**.

When to start: now, stacked on #1221.

Claim: GitHub issue #1242 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1242", or "Part of #1242" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1242. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Closure parameter modes and collection view callbacks.

Exclusive ownership:
- closure-parameter hunks of crates/vesper-syntax/src/parser.rs with their packages/vesper-front and packages/vesper-ast mirror
- closure-parameter hunks of src/check.rs, src/dyn_fn.rs and the lowering
- packages/vesper-std/collections/** (`with_ref`, `with_mut`, gap-filling `IdxMap`)
- manuscript hunks (flagged)
- tests/closure_modes*.rs

Implement:
1. Repros first: `|in v: State| v.n`, `|inout v: State| { v.n = 1; }`, a collection `with_ref(k, |in v| ..)`, and `IdxMap::put` at an index past the end with a constructor callback.
2. Syntax and typing of `in`/`inout` closure parameters (no `sink` unless it falls out), how such a closure is lent (it is second-class while it holds a borrow), with the manuscript change flagged.
3. Lowering on the evaluator, JIT and executable; inlining of the callback where the callee is small.
4. `with_ref`/`with_mut` on `FlatTable`, `IdxMap` and `CacheBTree`, and `IdxMap::put_with(i, |..| ..)` filling gaps.
5. Rewrite one checker port's lookup (for example `@vesper/check-fip` bindings) with the callbacks and measure it against the position-and-view form.

Acceptance:
- Repros pass; differential tests across evaluator, JIT and executable; leak tests clean; callbacks cost no more than the hand-written view match (callgrind).
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-closure-modes.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
