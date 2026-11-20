# Disjoint borrows: element indices, field paths through elements, fields of a lent receiver

> Self-hosting wave SH3, lane 2. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh3/disjoint-borrows`. Claim issue: #1241. Stacks on: #1221.

SH2 lane 1 (#1221) gave sequences element views, but the loan is still the whole sequence, a field path cannot go through an element, and a method on a struct cannot lend one of the struct's fields at the same time. Every checker port worked around these.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1221) in a NEW cloud checkout or a dedicated git worktree (`git fetch origin pull/<n>/head`). Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh3/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane", which all apply), docs/parallel-results/self-host-sh2-scoreboard.md (the SH2 numbers and gaps this wave comes from), docs/parallel-results/self-host-census.md, docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: #1221 and its report docs/parallel-results/self-host-element-borrows.md (gaps G1-G4, G9), docs/parallel-results/self-host-check-tensor-types.md on #1230 (gap 4), src/seq/check.rs, src/seq/ir.rs, the two-phase argument rules in src/check.rs (#1094, docs/parallel-results/ on argument borrows), src/ir_inline.rs (the refusal of borrowed matches on non-copy values).

When to start: now, stacked on #1221.

Claim: GitHub issue #1241 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1241", or "Part of #1241" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1241. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Disjoint element and field borrows.

Exclusive ownership:
- element-path and disjointness hunks of src/seq/check.rs and src/seq/ir.rs and their lowering
- the two-phase and borrow hunks of src/check.rs for lent receivers
- the non-copy-view refusal hunk of src/ir_inline.rs (merge main against SH3 lane 11's swap point)
- `swap(i, j)` on `ChunkSeq` in packages/vesper-std/core (or where #1221 put the element ops)
- manuscript hunks (flagged)
- tests/disjoint_borrows*.rs

Implement:
1. Commit each repro as a failing test first: `xs[0].n = 1`; `f(inout xs[0].slots)`; `swap(inout xs[0], inout xs[1])`; `f(inout xs[i], in xs[j])` with `i != j` not provable (must still refuse or check at run time, your call, flagged); `f(inout xs[i])` with a copy element; `put_name(in st, inout st.err, x)`; a raising `stamp(inout s[0])` with a live linear value (G4).
2. Field paths through an element: assignment and `inout` lending of `xs[i].f.g`, lowered as one bounds check and an address.
3. `xs.swap(i, j)` and two element views of one sequence in one call when the indices are distinct constants, or with a run-time distinctness check that raises like an out-of-bounds index (manuscript change, flagged).
4. A field of a lent receiver: `f(in st, inout st.err)` where the `in` borrow does not reach `err` (the callee's reads are of other fields); or refuse with a clear diagnostic if the rule cannot be made sound. State which in the report.
5. Inline functions that hold non-copy element views (G9): drop elaboration must not treat a borrowed view as owned.
6. Re-measure #1221's state pool (`abba_against_vec_state`) and one checker port's hot loop rewritten with the new forms.

Acceptance:
- Every repro passes or has a sound, documented refusal; evaluator, JIT (optimize on and off) and executable agree; leak tests clean.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-disjoint-borrows.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
