# SH2 scoreboard and SH3 plan

> Self-hosting wave SH2, lane 15. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh2/scoreboard-sh3-plan`. Claim issue: #1187.

This lane closes SH2: it re-measures every swap against the Rust checker in one hybrid build, collects the gaps the lanes found, and plans SH3 (Place IR lowering, inliner, optimizer, bounds hoisting, e-graph rules) in the plan-PR pattern, as SH1 lane 15 did for SH2.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh2/README.md (the wave plan and the rules for every lane), docs/parallel-results/self-host-census.md (the census this wave comes from: its samples, numbers and gaps C1-C12), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: every SH2 report, src/ir.rs (read only), src/ir_inline.rs, src/ir_opt.rs (read only), src/bounds_hoist.rs, src/optimize.rs.

The census samples are not in the repository. A copy is in the project files at /mnt/project-files/census/ (sample/src/*.ves, sample/lower/lower.ves, harness/*.rs) when your session can see that folder; if not, the census report describes their design (hash-consed type arena, interned names, flat tables, sorted rows, a state pool) and you rebuild it.

When to start: when lanes 5-14 have each merged a first swap (their later parts can still be open). If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1187 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1187", or "Part of #1187" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh2/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Parser mirror rule: any change to crates/vesper-syntax/src/parser.rs or lexer.rs must be mirrored in packages/vesper-front in the same PR, or tests/self_host_front_v2.rs fails.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only, never a root `**`); new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. A new port joins Linux port shard 7.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1187. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: SH2 scoreboard and SH3 plan.

Exclusive ownership:
- docs/maintenance/self-hosting-plan.md (the SH2 results and SH3 sections, and the gap table)
- new docs/agent-prompts/self-host-sh3/**
- docs/parallel-results/self-host-sh2-scoreboard.md

Implement:
1. Build the hybrid compiler with every SH2 module swapped in; run the full suite and the corpus differential; record compile time against the Rust checker (ABBA, callgrind) per module and for the whole checker.
2. Translate one SH3 sample locally (for example `ir_inline.rs`'s cost model) the way the census did, to find SH3's gaps before planning it. Never edit the Stage 1 guarded files.
3. Write the SH3 plan (README with lanes, claims, exclusive boundaries and merge order, one prompt per lane), open the claim issues, update self-hosting-plan.md.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Scoreboard with function and speed per module; SH3 plan PR that claims its lanes.
- Report: docs/parallel-results/self-host-sh2-scoreboard.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
