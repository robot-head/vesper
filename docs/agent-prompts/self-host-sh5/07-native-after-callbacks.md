# Native code: Option results, header hoisting, impure-call homes

> Self-hosting wave SH5, lane 7. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh5/native-after-callbacks`. Claim issue: #1330. Stacks on: #1317.

SH4 lane 4 met the 0.5% bar for `IdxMap` only and left three native gaps with repros: `Option[R]` results of inlined calls are built in memory and read back (16 Ir per lookup; needs enum scalar replacement in `native_aggregates`); receiver headers are re-read on every inlined call, because `ir_inline` refuses `eff [e]` callees and header hoisting skips loops with a closure or a `::` call (about 7 Ir per lookup); dropping the home rule for the `in` arguments of every impure call gave Grammar -7.5% to -13% and Aegis -4.7% to -8.2%, but two ports regressed slightly, so it needs a cost rule.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1317) (`git fetch origin pull/1317/head`) in a NEW cloud checkout or a dedicated git worktree. Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh5/README.md (the wave plan, its CI rule, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh4/README.md, docs/agent-prompts/self-host-sh3/README.md and docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane" and the SH3 and SH4 additions, which all apply), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: #1317 and docs/parallel-results/self-host-callback-registers.md on it, src/native_aggregates.rs, src/native.rs `register_locals`, src/bounds_hoist.rs, src/ir_inline.rs.

When to start: now, stacked on #1317 (SH4 lane 4). If a PR this lane waits on is still open, work only on the parts that do not depend on it.

Claim: GitHub issue #1330 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all the open SH4 lanes, #1275-#1291, which still own their own remaining parts); if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1330", or "Part of #1330" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1330. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: the three callback follow-ups.

Exclusive ownership:
- the enum scalar-replacement hunks of `src/native_aggregates.rs`
- the `register_locals` home-rule hunk of `src/native.rs` (after lane 4's)
- the closure/`::`-call hunk of header hoisting in `src/bounds_hoist.rs` (native path only, never the verified path)
- `tests/native_after_callbacks*.rs`
- report docs/parallel-results/self-host-native-after-callbacks.md

Implement:
1. Scalar-replace small `Option`/`Result` values of inlined calls so they live in registers.
2. Hoist receiver headers out of loops that contain lent closures or `::` calls when the loop provably does not write the receiver.
3. A cost rule for dropping `in` homes on impure calls that keeps the Grammar and Aegis wins and removes the Aegis `policy` and Silicon `dtb` regressions.
4. Re-measure every port (callgrind Ir) and the `flat_callbacks` bench; list each port that moves.

Acceptance:
- All three `flat_callbacks` parts within 0.5% of the view form.
- No port regresses by more than 0.2% Ir; the Stage 1 guard holds (the verified backend never sees these passes).
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-native-after-callbacks.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
