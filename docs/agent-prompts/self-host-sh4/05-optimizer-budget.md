# Optimizer time on large programs

> Self-hosting wave SH4, lane 5. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh4/optimizer-budget`. Claim issue: #1279.

Compiling the twelve SH2 checker packages as one program takes 170.8 s, and 72% of that is `ir_opt::optimize`, mostly egglog's `run_program` (SH3 lane 7, docs/parallel-results/self-host-compiler-capacity.md, gap 1). A self-hosted compiler several times that size would take many minutes per build. `ir_opt::optimize` is Stage 1 guarded, so how to fix it is the owner's call. Options: (A) a native-path policy around the unchanged pass (per-function runs, a content-hash cache, skipping saturation for functions it cannot improve) through the native call sites, so `bootstrap/stage1.lock` stays as it is; (B) budgets inside `ir_opt::optimize`, which edits guarded code and needs the owner to re-lock Stage 1; (C) no Rust change, and the speed comes from SH3 lane 9's Vesper `ir_opt` on `@vesper/egraph`. The owner chose A (2026-11-21).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh4/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh3/README.md and docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane" and SH3's additions, which all apply), docs/parallel-results/self-host-cranelift-shim.md, docs/parallel-results/self-host-census.md (the lowering sample), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-compiler-capacity.md, docs/parallel-results/self-host-large-functions.md, src/ir_opt.rs (read only), #1259 (src/ir_swap.rs, the `opt` hook), SH3 lane 9's prompt (docs/agent-prompts/self-host-sh3/09-ir-opt.md).

When to start: now, from main (the owner chose option A on 2026-11-21). If a PR this lane waits on is still open, work only on the parts that do not depend on it.

Claim: GitHub issue #1279 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1279", or "Part of #1279" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh2/README.md and the additions in docs/agent-prompts/self-host-sh3/README.md and docs/agent-prompts/self-host-sh4/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**; never route the verified backend's pipeline through a swap point; and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths. Depend on unmerged work only through the PR named in "Stacks on" or the lane named in "When to start".
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave. Port lowering emits through @vesper/cranelift, never by reimplementing Cranelift.
- Parser mirror rule: any change to crates/vesper-syntax/src/parser.rs or lexer.rs must be mirrored in packages/vesper-front (and packages/vesper-ast where it lowers the construct) in the same PR, or tests/self_host_front_v2.rs and tests/self_host_ast.rs fail.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo. Serialize merges that touch root Cargo.toml (no merge queue on this account).
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only, never a root `**`); new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. A new port joins Linux port shard 7.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1279. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: native-path optimizer policy (option A).

Exclusive ownership:
- new src/ir_opt_policy.rs and its call hunk in src/ir_swap.rs's default `opt` path (native path only)
- tests/optimizer_budget*.rs
- report docs/parallel-results/self-host-optimizer-budget.md

Implement:
1. Profile `ir_opt::optimize` on the combined checker program: time per function, rewrites found per function, and which functions never change.
2. On the native path only, call the unchanged `ir_opt::optimize` per function (or per group), skip functions whose result is cached by content hash, and skip functions the profile shows it cannot improve. The verified backend keeps calling `ir_opt::optimize` directly.
3. Run-time cost: the port harness and the corpus with the policy on and off. Any program that gets slower at run time is listed.

Acceptance:
- The combined checker build at least 2x faster, with run-time Ir of the ports within 1% (or each difference listed for the owner).
- bootstrap/stage1.lock unchanged.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-optimizer-budget.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
