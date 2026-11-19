# Adopt the wave in the P3 ports and re-score

> P3b gaps wave, lane 12. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/rescore`. Claim issue: #1097.

Lanes 1-11 measure their repros and the named ports with local rewrites. This lane commits those rewrites where the port's own lane is done, re-measures every P3a and P3b port against one `main` with the setup of `docs/parallel-results/port-gaps-rescore.md`, and writes the scoreboard rows the owner uses to decide which ports reach parity (and which Rust code can then be deleted).

Ports to re-measure: Resonance, Kinetic, Warp, Prism H3, Chorus, Flux, eBPF, silicon, Pact, Bench, Loom audit, Aegis.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: every docs/parallel-results/port-p3-*.md and port-p3b-*.md, port-gaps-rescore.md, bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: after lanes 1-11 merge and after the port speed re-score (#1028). If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1097 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1097", or "Part of #1097" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths. This lane starts only after lanes 1-11 and #1028 have merged.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code; recommend defaults in the report. Commit port rewrites only in ports whose port-lane PRs have merged.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1097. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Adopt the wave in the P3 ports and re-score.

Exclusive ownership:
- crates/vesper-<port>/** of the P3 ports whose port-lane PRs have merged (coordinate with any open port-lane PR instead of editing its files)
- docs/parallel-results/port-parity.md (the P3 section)
- docs/parallel-results/p3b-gaps-rescore.md

Implement:
1. Remove the workarounds the wave made unnecessary (shift helpers, `0..u(3)`, hoisted argument locals, banks instead of handle sequences, private sorts, scalar float loops where `Vec[f32, N]` applies) in ports whose port-lane PRs have merged; for ports with an open port-lane PR, hand the rewrite to that lane on its issue instead.
2. Re-measure every P3a and P3b port (ABBA wall clock and callgrind instructions) and update the scoreboard.
3. Rust stays the default build of every port except those the owner has already moved; recommend, do not change, defaults.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. Every port's own tests and divergence checks pass after its rewrites.
- `docs/parallel-results/p3b-gaps-rescore.md` gives each port's ratio before and after the wave and a remaining-gaps table with repros, which seeds the next compiler wave.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-rescore.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
