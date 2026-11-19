# Inline helpers that take an inout slice

> P3b gaps wave, lane 4. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/inline-inout-slices`. Claim issue: #1089.

`src/ir_inline.rs` refuses any callee with a slice parameter that is `inout` or `sink` (`refuse(.., "inout or sink slice")`); an `in` slice is a view value and inlines. Every byte writer in Warp (`w_vi`, `vi_put`, the `w_*` writers) and warp-tcp, and the Resonance and Kinetic kernels that write lent buffers, stay calls where Rust inlines all of them. Repro from port-p3b-warp.md gap 1 (`vesper build --lib . --lib-name repro`): `fn w8(inout out: Slice[u8], at: u64, v: u8) -> u64 { out.try_set(at, v); wrapping_add(at, 1) }` and `pub fn put3(inout out: Slice[u8]) -> u64 { let a = w8(inout out, 0, 1); let b = w8(inout out, a, 2); w8(inout out, b, 3) }` keeps three calls, while the `in` twin `get3` has none. A `Fail` row on `w8` makes no difference.

Ports to re-measure: Warp, warp-tcp, Resonance, Prism H3, Kinetic.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3b-warp.md (gap 1), port-p3b-resonance.md, bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1089 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1089", or "Part of #1089" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1089. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Inline helpers that take an inout slice.

Exclusive ownership:
- src/ir_inline.rs, src/ir_inline/**
- tests/native_inline_inout_slice*.rs
- docs/parallel-results/p3b-gaps-inline-inout-slices.md

Implement:
1. Inline callees with `inout` slice parameters when the argument is a place rooted at a caller local or a caller slice parameter: the callee's slice parameter becomes the caller's slice place (writes go through to the same header and buffer), as `inout` aggregates already do.
2. `sink` slices may stay refused; say so in the report if they do.
3. Keep the capability-verifier rule (callers with `ConstTime`/`Realtime`/`DetFloat` rows are left alone) and the growth budgets; record new refusals in `Report::refused`.
4. No manuscript change is expected.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- The `put3` repro has no calls after the change (object disassembly), and Warp's frame encoder and warp-tcp's codec are re-measured with their committed ABBA benchmarks.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-inline-inout-slices.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
