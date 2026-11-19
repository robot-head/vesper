# Bounds checks and slice headers

> P3b gaps wave, lane 3. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/bounds-facts`. Claim issue: #1088.

Bounds-check hoisting (#933, `src/bounds_hoist.rs`) versions counted loops whose body indexes a sequence with the loop variable itself. The P3 reports hit three cases it does not cover:

1. **Affine indices** (port-p3-bench.md gap 2): `for s in (0..=total).rev() { dist[s + rank] += dist[s] }` (`util.ves` `add_rank`) is 97-99% of the bench port's time; Rust's checks are removed by LLVM. Reverse ranges and `i + k`/`i - k` with loop-invariant `k` need the same guard (`hi + k <= len`).
2. **Constant ranges and dominated accesses** (port-p3b-resonance.md gap 2): `for v in 0..16` over 11 slice parameters checks every access against `len` even after an access at index 15.
3. **Header reloads** (resonance gap 2): each `inout` slice parameter is a pointer to a {ptr, offset, len} header, and after any `set` on one slice the header of every other is reloaded. Distinct `inout`/`in` slice parameters cannot alias (the place calculus refuses overlapping loans), so a store through one cannot change another's header. Repro: `fn k(inout a: Slice[u32], inout b: Slice[u32]) eff [Fail[RuntimeError]] { for v in 0..16 { a.set(v, b[v]); b.set(v, a[v]); } }`: two header loads and two compares per access today.
4. **Loops in functions with Box drops are skipped** (port-p3b-loop-owned-boxes.md, "Remaining gaps"), so the boxed-object loops #1076 enables lose hoisting.

Ports to re-measure: Bench (the `add_rank` loop is 97-99% of the port), Resonance, atomics workers, Prism H3, Kinetic.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3-bench.md (gap 2), port-p3b-resonance.md (gap 2), port-p3-atomics-workers.md, port-p3b-loop-owned-boxes.md, bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1088 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1088", or "Part of #1088" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1088. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Bounds checks and slice headers.

Exclusive ownership:
- src/bounds_hoist.rs, src/bounds_hoist/**
- the slice-header load hunk of src/native.rs (where a slice parameter's {ptr, offset, len} is read) and its alias facts
- tests/native_bounds_facts*.rs, tests/native_slice_headers*.rs
- docs/parallel-results/p3b-gaps-bounds-facts.md

Implement:
1. Extend the guard to reverse ranges, `..=`, and indices `i + k`, `i - k` and `k - i` with loop-invariant `k` (checked arithmetic in the guard itself, so an overflowing guard falls back to the checked loop).
2. Remove a check dominated by a check of a larger or equal index on the same unchanged sequence (constant indices and constant-trip loops).
3. Load each slice parameter's header once per function (or once per loop when the length can change) and do not reload it after stores through a different slice; keep reloads after calls that receive the slice `inout`.
4. Allow hoisting in functions that own boxes, once #1076 merges (merge `main` first).
5. No manuscript change is expected; this is a native optimize-path pass.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- The bench `add_rank` loop and the Resonance header repro are measured in instructions before and after; Bench, Resonance and Prism H3 are re-measured with their committed ABBA benchmarks. A short sequence still faults at the same iteration with the same prior effects (differential tests).
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-bounds-facts.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
