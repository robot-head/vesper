# Fail across stationary inout (keep-writes) and cheaper Fail lowering

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

When to start: now for the language change (steps 1-4); the codegen (step 6) after lane 4 (#934) and lane 1 (#931) merge. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

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
- the E0402 abort-frame check in src/check.rs (`check_abort_frame_for`, the `loans`/`Binding` depth bookkeeping it needs, `apply_args` sibling-argument loans, and the `ExprKind::Handle` fact havoc) and its tests
- the Tier-1 fallible lowering with `inout` in src/lean.rs (`restore_states` and its callers) ONLY: wave-nine lane 16 (#593) holds the Lean extractor's inout and multi-Fail contracts. Before editing, read #593 and its branch if any; if it has an open PR touching the same code, coordinate there instead of editing
- the manuscript hunks named in step 2 (§4.3/§4.5 PostCall, §5.2 frame condition scope, §8.2 Tier 1)
- the native `Fail` row lowering in src/native.rs (`fail_record_size`, `fail_exit`, call-site status handling) and the matching code in src/native_export.rs/glue after lane 4 merges
- tests/native_fail*.rs, tests/inout_abort*.rs
- docs/parallel-results/stage1-perf-fail-lowering.md
Dependencies: Steps 1-4 (language change) start now. Steps 5-6 (codegen) wait for lanes 1 and 4.

Owner decision (2026-11-17, from the owner's language design review): Vesper allows an abortable effect (`Fail`, `Cancel`, uniformly, by `res(epsilon) = affine`, never by effect name) to propagate out of a call while a *stationary* `inout` argument is live, with keep-writes semantics: the caller's place holds the last value the callee wrote before the abort (as Rust `&mut` does). Roll-back (copy-in/copy-out) is rejected: it cannot snapshot affine/linear/pinned places and costs a copy on every call. The owner's review says the evaluator (src/runtime.rs, shared `Slot`) and Cranelift (src/native.rs, pointer-passed `Pass::Inout`) already keep writes, and that E0402 mostly hid a mismatch with the Lean Tier-1 extraction, which models roll-back. Verify both claims with tests before relying on them.

Implement:
1. Checker (src/check.rs). Stop raising E0402 for an active `inout` loan or parameter when all of these hold, and keep it otherwise:
   - the parameter is stationary (`tau_in == tau_out`; no `~>` typestate transition). A non-stationary `inout` still needs `res(epsilon) = linear` unless the abort is caught by a lexical `handle` inside the callee;
   - the place has no open move-out holes in the PCG at the abort point (`moved` set empty);
   - refinements and `@respects` quotient invariants of the `inout` parameter hold at every abortable checkpoint reachable after it is mutated, not only at `return`;
   - an `out` destination is left uninitialized on abort; partially written copy/affine parts are dropped; writing a linear value into an `out` slot before an abortable checkpoint stays rejected.
   Keep `[T-Let]`/E0301 unchanged on owned bindings: `inout tx: WriteTxn` (linear) may cross a fallible call, and the caller that owns `tx` still must handle the abort before its frame unwinds. Sibling argument aborts (`foo(inout x, fallible()?)`) follow the same rule; E0410 still protects linear `sink` siblings.
   Fix the scoping gap the review found: `target` (the catching handler) filters only `state_guards` today, so a `Fail` raised and caught by a `handle` inside `fn f(inout x: T)` is wrongly rejected. Record lexical handler depth on bindings and loans and skip those at or outside the catching handler's depth.
   Havoc path-sensitive facts in the caller mentioning the borrowed place on both the normal-return edge and the `handle Fail` catch edge.
2. Manuscript: split PostCall into `PostCall_ok` and `PostCall_abort` in §4.3/§4.5 (abort: `sink` arguments stay consumed, stationary `inout` places keep the last write with facts havoced, `out` stays uninitialized, non-stationary `inout` needs a linear row or an internal handler); scope the §5.2 frame condition to owned obligations and handler states; in §8.2, Tier 1 for fallible functions with stationary `inout` emits `ExceptT E (StateM Sigma_io) (Ret × Out)`. Use the owner's wording from the decision on #935 where it fits, and flag the change in the PR body.
3. Lean extraction (src/lean.rs, see the ownership note on #593): switch fallible functions with `inout` parameters from `Except E (Ret × Inout)` with `restore_states` after bind (which models roll-back) to `ExceptT E (StateM Inout) Ret`, so `.error` keeps the state. Show with a test that the extracted model, the evaluator and native code agree on the post-abort value.
4. Tests: accepted and rejected cases for each condition above (stationary vs type-changing, moved-out hole, refinement broken at the abort point, out slot, linear `inout`, sibling arguments, local handler, Cancel); differential interpreter/JIT/AOT tests that the caller sees the callee's last write after an abort.
5. Measure: in BBR, Aura and Quarry, every mutating export returns `Result`/`Option` as a value because of E0402; record what the Result construction, the failure-record writes and the copies cost per call. With steps 1-4 merged, rewrite one hot export per port to raise `Fail` across `inout` in a local experiment (do not push to port branches).
6. Native lowering (after lanes 1 and 4): return the failure status in a register (0 = success, k = the k-th Fail of the row) and write the result and error payload in place through caller-provided storage, so a successful call writes nothing extra. Callers test the register. Keep the export ABI's failure-record contract at the boundary (lane 4's glue converts once). Return small copy `Result`/`Option` payloads in two registers where the Cranelift calling convention allows. Differential tests for every Fail row shape (one and several Fails, payload sizes, nested handlers, Fail inside loops).

The language change may land as its own PR before the codegen PR; both close parts of #935 (the last one closes it).

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The verified backend's own Fail lowering is unchanged.
- BBR, Aura and Quarry are re-measured; the report gives each ratio before and after, and lists the E0402 cases that changed and the ones that remain.
- Report: docs/parallel-results/stage1-perf-fail-lowering.md, with what changed, the Stage 1 guard evidence, the port ratios before and after with the setup, and what is left.
~~~
