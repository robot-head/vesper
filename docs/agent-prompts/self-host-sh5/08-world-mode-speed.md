# `World::mode` speed toward the 2x build target

> Self-hosting wave SH5, lane 8. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh5/world-mode-speed`. Claim issue: #1331. Stacks on: #1312.

SH4 lane 5 cut the optimizer from 54.6 s to 5.2 s on the combined checkers, but the whole `vesper build --lib` is 1.72x faster, not 2x: with the optimizer at zero it would still be about 1.87x. Most of what is left is the checker's `World::mode` recursion in `check_core` and `ir_check::validate` (gap 1 in docs/parallel-results/self-host-optimizer-budget.md).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1312) (`git fetch origin pull/1312/head`) in a NEW cloud checkout or a dedicated git worktree. Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh5/README.md (the wave plan, its CI rule, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh4/README.md, docs/agent-prompts/self-host-sh3/README.md and docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane" and the SH3 and SH4 additions, which all apply), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: #1312 and its report, src/check.rs and src/check/** (`World::mode` and its callers), src/ir_check.rs, the SH3 lane 7 capacity report docs/parallel-results/self-host-compiler-capacity.md.

When to start: now, stacked on #1312 (SH4 lane 5). If a PR this lane waits on is still open, work only on the parts that do not depend on it.

Claim: GitHub issue #1331 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all the open SH4 lanes, #1275-#1291, which still own their own remaining parts); if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1331", or "Part of #1331" for a partial PR).

Delivery and coordination:
- CI rule (owner, 2026-11-21, standing project rule): no GitHub Actions runs for a branch that cannot merge yet. While the PR you stack on is open, cancel every Actions run your push starts right after the push (list the branch's runs, then `gh api -X POST repos/robot-head/vesper/actions/runs/<id>/cancel` for each, or the GitHub MCP actions tools), including the bootstrap-stage1 workflow. Push rarely. Run targeted tests locally while stacked; let CI run (and dispatch bootstrap-stage1) only once your base has merged and your branch has merged origin/main, right before landing.
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh2/README.md and the additions in docs/agent-prompts/self-host-sh3/README.md, docs/agent-prompts/self-host-sh4/README.md and docs/agent-prompts/self-host-sh5/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**; never route the verified backend's pipeline through a swap point; and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch once it can merge).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths. Depend on unmerged work only through the PR named in "Stacks on" or the lane named in "When to start".
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave. Port lowering emits through @vesper/cranelift, never by reimplementing Cranelift.
- Parser mirror rule: any change to crates/vesper-syntax/src/parser.rs or lexer.rs must be mirrored in packages/vesper-front (and packages/vesper-ast where it lowers the construct) in the same PR, or tests/self_host_front_v2.rs and tests/self_host_ast.rs fail.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo. Serialize merges that touch root Cargo.toml (no merge queue on this account).
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only, never a root `**`); new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. A new port joins Linux port shard 7.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1331. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: `World::mode` speed.

Exclusive ownership:
- the `World::mode` hunks of `src/check.rs`/`src/check/**` and the matching hunks of `src/ir_check.rs`
- `tests/world_mode_speed*.rs` and a bench
- report docs/parallel-results/self-host-world-mode-speed.md

Implement:
1. Profile the combined-checker build (callgrind and wall) and find why `World::mode` recurses: repeated work per call site, per field or per instance.
2. Memoize or precompute modes per type in the world, with the same answers; the SH2 check-data differential and the checker's own tests must not change.
3. If the Vesper checker ports (`@vesper/check-core-typing`) mirror the slow shape, report it with the measurement instead of editing them.
4. Re-measure `vesper build --lib` on the combined program with #1312's policy on.

Acceptance:
- The combined-checker `vesper build --lib` is at least 2x faster than before #1312, or the report shows what is left and where.
- Diagnostics and outputs are unchanged over the corpus.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-world-mode-speed.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
