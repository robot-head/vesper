# Cheaper Fail lowering on the native path

> Stage 1 performance wave, lane 5. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `stage1-perf/fail-lowering`. Claim issue: #935.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend performance gap found by port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 943d901 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/stage1-perf/README.md (the wave plan, why the work fits in Stage 1, and the rules for every lane), the port reports docs/parallel-results/port-p1-*.md (on main or on PRs #924-#930), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: after lane 4 (#934) and lane 1 (#931) merge; the measurement can start now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #935 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #935").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/stage1-perf/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs, `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. Measure ports; do not push to port branches.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #935. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the port ratios before and after, decisions and remaining gaps.

Assignment: Cheaper Fail lowering on the native path.

Exclusive ownership:
- the native `Fail` row lowering in src/native.rs (`fail_record_size`, `fail_exit`, call-site status handling) and the matching code in src/native_export.rs/glue after lane 4 merges
- tests/native_fail*.rs
- docs/parallel-results/stage1-perf-fail-lowering.md
- only if the owner approves relaxing E0402: the E0402 check for Fail across an active inout, its manuscript section, and its tests
Dependencies: Lanes 1 and 4.

Implement:
1. Measure first: in BBR, Aura and Quarry, every mutating export returns `Result`/`Option` as a value because `Fail` cannot cross an active `inout` (E0402); record what the Result construction, the failure-record writes and the copies cost per call.
2. Native lowering: return the failure status in a register (0 = success, k = the k-th Fail of the row) and write the result and error payload in place through caller-provided storage, so a successful call writes nothing extra. Callers test the register. Keep the export ABI's failure-record contract at the boundary (lane 4's glue converts once).
3. `Result`/`Option` returns of small copy payloads: return them in two registers instead of through memory where the Cranelift calling convention allows.
4. E0402 is a language rule (what an `inout` argument holds when the callee fails). Do not change it unless the owner approves on #935. If approved: write the manuscript change (the argument keeps the callee's writes up to the failure, as Rust `&mut` does), relax the checker, and add the tests; otherwise leave E0402 as is and say so in the report.
5. Tests: differential interpreter/JIT/AOT tests for every Fail row shape (one and several Fails, payload sizes, nested handlers, Fail inside loops).

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The verified backend's own Fail lowering is unchanged.
- BBR, Aura and Quarry are re-measured; the report gives each ratio before and after, and states whether E0402 changed.
- Report: docs/parallel-results/stage1-perf-fail-lowering.md, with what changed, the Stage 1 guard evidence, the port ratios before and after with the setup, and what is left.
~~~
