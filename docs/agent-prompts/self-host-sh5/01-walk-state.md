# Lent walk state and re-entry into Rust

> Self-hosting wave SH5, lane 1. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh5/walk-state`. Claim issue: #1324. Stacks on: #1309.

SH4's family hooks can emit only leaf instructions. Scalar `Read`/`Bind`/`Write` (lane 6, 10,307 + 3,649 + 295 sites left to Rust), the raising arms, call result stores, `inout` spills, the Fail record (lane 7), and closure and clause bodies, `perform`, `fail_exit`/`reraise_*`, `dyn` and session parks (lane 10) all need the walk's own state: `vars`, register and home maps, split locals, edge infos through `Codegen::jump`, and a way to ask Rust to emit a sub-expression and hand back its values.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1309) (`git fetch origin pull/1309/head`) in a NEW cloud checkout or a dedicated git worktree. Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh5/README.md (the wave plan, its CI rule, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh4/README.md, docs/agent-prompts/self-host-sh3/README.md and docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane" and the SH3 and SH4 additions, which all apply), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: src/codegen_swap.rs and src/native.rs on #1309 (`Codegen`, `ir_graph_at`, `expr_at`, `jump`, `def_var`, `use_var`, `flush`, the `swap_*` helpers), the reports of SH4 lanes 6, 7 and 10 (docs/parallel-results/self-host-codegen-{scalar,calls,control}.md on #1302, #1306 and #1298), lane 4's #1317.

When to start: now, stacked on #1309 (SH4 lane 1). Merge #1317 (lane 4) in once it lands, because both edit `register_locals`. If a PR this lane waits on is still open, work only on the parts that do not depend on it.

Claim: GitHub issue #1324 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all the open SH4 lanes, #1275-#1291, which still own their own remaining parts); if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1324", or "Part of #1324" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1324. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: the lent walk state.

Exclusive ownership:
- new `src/codegen_swap/walk.rs` (a `codegen_swap` submodule) and its request constants appended to `src/codegen_swap.rs`
- the walk-state hunks in `src/native.rs` that expose `vars`, homes, registers, split locals and edge infos to a request, and the re-entry entry point
- `@vesper/codegen-base` additions for the walk-state handles (new files only)
- `crates/vesper-codegen-swap-test/tests/self_host_walk_state*.rs`
- report docs/parallel-results/self-host-walk-state.md

Implement:
1. Lend the walk state as handles: look up and define a place's variable, its home slot or register, a split local's words, and record an edge info for a jump. Each is valid for one request.
2. Re-entry: a hook can ask Rust to emit one IR operand or sub-expression and get its values back, so a family hook can own a construct whose operands it cannot lower yet.
3. Prove it on `Read`, `Bind` and `Write` in a demonstration hook in the test crate (not in lane 6's package): 0 CLIF, object and run differences over the corpus, optimize on and off.
4. Write the protocol into the `codegen_swap` module docs so SH4 lanes 6, 7 and 10 can adopt it in their own packages. Behaviour-free with no hook installed: default objects byte-identical to main's.

Acceptance:
- The walk-state and re-entry requests are documented, and the demonstration hook answers every `Read`/`Bind`/`Write` with 0 CLIF, object and run differences.
- With no hook installed the default compiler's objects are byte-identical to the base.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-walk-state.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
