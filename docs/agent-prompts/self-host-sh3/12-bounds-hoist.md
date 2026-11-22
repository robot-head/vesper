# Port bounds-check hoisting to Vesper

> Self-hosting wave SH3, lane 12. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh3/bounds-hoist`. Claim issue: #1251. Stacks on: #1232.

`bounds_hoist` versions counted loops so their sequence accesses run unchecked behind one guard, drops dominated checks, and hoists stable views and headers (#933, #1088, #1146). Its facts and decisions only read the IR, so they port first on #1232's IR decoder; the rewrite needs lane 1's writer.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1232) in a NEW cloud checkout or a dedicated git worktree (`git fetch origin pull/<n>/head`). Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh3/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane", which all apply), docs/parallel-results/self-host-sh2-scoreboard.md (the SH2 numbers and gaps this wave comes from), docs/parallel-results/self-host-census.md, docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: src/bounds_hoist.rs, src/bounds_hoist/facts.rs, src/bounds_hoist/native.rs, docs/parallel-results/ on byte loops and bounds facts (stage1-byte-loops, p3b-gaps-bounds-facts, self-host-byte-loops), #1232's IR encoder and decoder, lane 1's prompt.

When to start: now, stacked on #1232, for the facts and which loops and accesses hoist; the rewrite after lane 1's first PR.

Claim: GitHub issue #1251 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1251", or "Part of #1251" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1251. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: `@vesper/bounds-hoist`.

Exclusive ownership:
- new packages/vesper-bounds-hoist/**
- the hybrid module `bounds_hoist` and its `hoist` hook install
- tests/self_host_bounds_hoist*.rs
- one row of the SH3 scoreboard

Implement:
1. Port the facts and the analyses (`stable_views`, `header_hoists`, which loops version, which checks are dominated); differential per function over the corpus.
2. After lane 1: port the rewrite (loop versioning, `SeqOp::Hoisted`, dropped checks) and compare programs and run results through lane 1's harness, including the short-sequence fault cases (same iteration, same prior effects).
3. Swap through lane 1's `hoist` hook; full suite on the hybrid build.
4. Scoreboard row: the pass alone and whole corpus, ABBA and callgrind.

Acceptance:
- 0 differences in analyses; after lane 1, 0 differences in rewritten programs and run results.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-bounds-hoist.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
