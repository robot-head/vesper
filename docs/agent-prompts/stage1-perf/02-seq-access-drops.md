# Sequence element access without Option, and native drops for local and by-value sequences and strings

> Stage 1 performance wave, lane 2. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `stage1-perf/seq-access-drops`. Claim issue: #932.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend performance gap found by port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 943d901 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/stage1-perf/README.md (the wave plan, why the work fits in Stage 1, and the rules for every lane), the port reports docs/parallel-results/port-p1-*.md (on main or on PRs #924-#930), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #932 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #932").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/stage1-perf/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs, `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. Measure ports; do not push to port branches.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #932. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the port ratios before and after, decisions and remaining gaps.

Assignment: Sequence element access without Option, and native drops for local and by-value sequences and strings.

Exclusive ownership:
- src/seq/native.rs, src/seq/native_runtime.rs, src/seq/aot_runtime.rs, src/seq/ir.rs
- src/strings/native.rs
- the drop elaboration in src/fip.rs (`elaborate_non_fip_drops` and its helpers), extended from `Box` to sequences and strings
- the drop hooks in src/native.rs that emit `box_drops_*` (generalize them; nothing else in the file)
- the hunk in src/native_export.rs that refuses passing a sequence or string by value (own commit; lane 4 owns the rest of the file, coordinate on #934)
- tests/native_seq_drop*.rs, tests/native_seq_access*.rs
- docs/parallel-results/stage1-perf-seq-access-drops.md
Dependencies: None. Lane 3 waits for this lane's src/seq/native.rs changes.

Implement:
1. Measure first: callgrind Quarry (#930, `planner_abba`) and Aura (#928, `abba`) and record the share of `malloc`/`free`/`realloc` and of `try_get` + Option matching.
2. Element reads: lower `get`/`try_get` immediately consumed by a `match`/`if let` (and indexing that raises `Fail[RuntimeError]`) to one inline compare-and-branch and a direct load, with no Option value built in memory. Hoist the header loads (`len`, `data`) out of loops where the handle cannot change (no `push`/grow on the same sequence in the loop body).
3. Drops: free function-local `ChunkSeq` and string buffers at scope end, on every exit edge (return, Fail, handler abandonment), unless moved, returned, stored into an owned aggregate or passed by value. A value passed by value is owned by the callee and freed there. Both the JIT runtime (which today frees everything at teardown) and the AOT runtime must agree, and the live-allocation count must return to its start.
4. Lift the export ABI's by-value refusal for sequences and strings, now that the callee frees what it drops (own commit, linked on #934).
5. Allocation: start sequences built by `push` loops with a capacity hint where the loop bound is known, and grow geometrically (check the current policy first).
6. Tests: leak tests with the live-allocation counter over every exit kind; double-free tests (move then drop, by-value then drop, aliasing slices of a dropped sequence must be refused by the checker already: prove it with a test); differential interpreter/JIT/AOT tests.

Acceptance:
- Leak and double-free tests pass on all four platforms, the Stage 1 guard holds, and the ports' existing leak tests (`port_live_allocations()`) still pass without the handle-field workarounds (show one port with the workaround removed locally).
- Quarry, Aura and Ignis are re-measured; the report gives each ratio before and after.
- Report: docs/parallel-results/stage1-perf-seq-access-drops.md, with what changed, the Stage 1 guard evidence, the port ratios before and after with the setup, and what is left.
~~~
