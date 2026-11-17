# Inline small Vesper functions on the native path and run Cranelift at opt_level=speed

> Stage 1 performance wave, lane 1. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `stage1-perf/inlining`. Claim issue: #931.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend performance gap found by port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 943d901 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/stage1-perf/README.md (the wave plan, why the work fits in Stage 1, and the rules for every lane), the port reports docs/parallel-results/port-p1-*.md (on main or on PRs #924-#930), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #931 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #931").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/stage1-perf/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs, `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. Measure ports; do not push to port branches.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #931. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the port ratios before and after, decisions and remaining gaps.

Assignment: Inline small Vesper functions on the native path and run Cranelift at opt_level=speed.

Exclusive ownership:
- NEW src/ir_inline.rs (and src/ir_inline/** if you split it)
- the `if optimize { .. }` branches that call `ir_opt::optimize` in src/native.rs (`compile_entry`), src/native_export.rs (the library path ports use), src/native_session.rs and src/native_batch.rs: add one call to the inliner after `ir_opt::optimize`
- the Cranelift `settings::builder()` sites in src/native.rs, src/native_export.rs, src/native_session.rs, src/native_batch.rs, src/native_handlers.rs and src/bench_commands.rs: `opt_level=speed` when `optimize` is on, `none` otherwise (keeps the Stage 1 corpus objects unchanged)
- the `runtime.optimize = false` default in src/bench_commands.rs if measurement shows it hides the optimizer
- tests/native_inline*.rs
- docs/parallel-results/stage1-perf-inlining.md
Dependencies: None. Lanes 2 and 5 also edit src/native.rs; keep to your hunks and merge main before merging.

Implement:
1. Measure first: profile two ports (BBR on main with `ack_path_abba`, and Quarry or warp-wheel from its PR branch) and record the call counts of small Vesper helpers on the hot path, so the report can show what inlining removed.
2. Write a Place IR inliner (`ir_inline::inline(world, &mut program, &Budget)`): inline direct calls to non-recursive callees (no cycle in the call graph) whose body is at most a fixed instruction budget, plus single-call-site private functions, bottom-up, with a total growth cap per caller and per program. Keep effects exact: a callee with a `Fail` row inlines only if its failure edges map onto the caller's handlers or row as the call did; handlers, regions, linear state and `inout` loans keep their scopes (rename values and blocks, never merge scopes). Recursion, closures and indirect calls are not inlined.
3. Run it only on the native path and only when `optimize` is on, after `ir_opt::optimize` and before `fip::elaborate_non_fip_drops`. Do not call it from `ir_opt::optimize`, which the verified backend runs.
4. Set Cranelift `opt_level=speed` for optimized builds at every native settings site, and leave unoptimized builds at their current setting so the Stage 1 corpus (built with optimize off) is byte-identical.
5. Tests: differential tests (interpreter, JIT, AOT, optimize on and off) over programs with nested calls, Fail rows, inout, handlers and generic instances; a test that recursion and over-budget callees stay calls; a test that the Stage 1 corpus objects do not change.

Acceptance:
- The differential tests pass on all four platforms, and the Stage 1 guard holds (verified digests equal; Cranelift corpus digests also equal, since the corpus is built with optimize off).
- BBR and at least two more ports are re-measured; the report gives each ratio before and after, and the share of the change from inlining versus `opt_level=speed`.
- Report: docs/parallel-results/stage1-perf-inlining.md, with what changed, the Stage 1 guard evidence, the port ratios before and after with the setup, and what is left.
~~~
