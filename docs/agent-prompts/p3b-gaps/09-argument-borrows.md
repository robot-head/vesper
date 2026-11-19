# Two-phase inout arguments and bound views

> P3b gaps wave, lane 9. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/argument-borrows`. Claim issue: #1094.

#986 (PR #1004) reserved receiver loans: `s.push(s.len())` checks. The manuscript says "Only receivers are reserved: an `inout` argument of a plain call `f(inout s, s.len())` excludes reads of its place in the other arguments as before." Every P3 port with a state handle hit this:

- port-p3b-chorus.md gap 1: `f(inout c, g(in c, x))` and `c.out.push(ga(in c, n))` are refused; about 140 arguments are hoisted into `let`s. Repro: `fn g(in c: C) -> u64 { c.x } fn f(inout c: C, v: u64) { c.x = v; } fn h(inout c: C) { f(inout c, g(in c)); }`.
- port-p3-pact.md gap 1 (about 50 places), port-p3-ebpf.md gap 7, port-p3-loom-audit.md (`f(inout s, s.field)`).
- port-p3b-warp.md: the nested `w8(inout out, w8(inout out, 0, 1), 2)` is refused; with reservation the inner call is evaluated before the outer loan activates.
- port-p3b-chorus.md gap 2: `let b = s.as_bytes();` on an `in Str` is E1531.
- port-p3-aegis.md gap 6: `b[a..c].find_not_in(..)` is E1521 (slice views as method receivers).

**Spec call (flagged):** this extends the manuscript's two-phase rule from receivers to every `inout` argument. Arguments are already evaluated left to right before the call on every backend, so the evaluator, native code and the Lean extraction agree; the self-hosted Stage 1 checker stays stricter, as it already is for receivers.

Ports to re-measure: Chorus, Pact, eBPF verifier, Loom audit, Aegis, Warp.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3b-chorus.md (gaps 1-2), port-p3-pact.md (gap 1), port-p3-ebpf.md (gap 7), port-p3-loom-audit.md, port-p3-aegis.md (gap 6), port-p3b-warp.md (gap 1, side note), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1094 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1094", or "Part of #1094" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1094. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Two-phase inout arguments and bound views.

Exclusive ownership:
- the argument-loan reservation hunk of src/check.rs (next to reserved_receivers)
- the matching Place IR check hunk of src/ir_check.rs
- the E1531 hunk of src/strings/check.rs and the E1521 hunk of src/seq/check.rs
- tests/two_phase_arguments*.rs, tests/bound_views*.rs
- the two-phase loan paragraph of the manuscript (§5, "Two-phase receiver loans")
- docs/parallel-results/p3b-gaps-argument-borrows.md

Implement:
1. Reserve every `inout` argument loan while the call's other arguments are evaluated, with the same rules as receivers: reads (`in`, copies) of the place are allowed and see the value from before the call; writes, moves and `inout` loans of an overlapping place are still refused, including in nested calls that are still live when the outer call is entered.
2. Allow `let v = s.as_bytes()` (and other view-returning methods on `in` values) when the view does not outlive its source.
3. Allow slice views (`b[a..c]`) as method receivers for the `Slice` methods.
4. Update the manuscript paragraph and flag the change.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- Each repro above checks and runs on the evaluator, JIT and objects with identical results; a local rewrite removes the hoisted locals from one Chorus and one Pact module with identical output; the refused cases (write in an argument, overlapping `inout`) still give E0301/E0410.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-argument-borrows.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
