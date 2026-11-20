# Swap layout and the IR check into the hybrid build

> Self-hosting wave SH2, lane 5. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh2/layout-ir-check-swap`. Claim issue: #1177.

The census translated `src/layout.rs` and `src/ir_check.rs` to Vesper with 0 differences on 272 worlds (the IR check also on five mutants of each: 1,597,638 instructions and 190,875 diagnostics byte-identical) at 0.34x and 0.57x of Rust's time. They are the first checker-side modules ready to swap.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh2/README.md (the wave plan and the rules for every lane), docs/parallel-results/self-host-census.md (the census this wave comes from: its samples, numbers and gaps C1-C12), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-hybrid-build.md, docs/agent-prompts/self-host-sh1/12-parser-swap.md, crates/vesper-hybrid/**, src/layout.rs, src/ir_check.rs.

The census samples are not in the repository. A copy is in the project files at /mnt/project-files/census/ (sample/src/*.ves, sample/lower/lower.ves, harness/*.rs) when your session can see that folder; if not, the census report describes their design (hash-consed type arena, interned names, flat tables, sorted rows, a state pool) and you rebuild it.

When to start: after SH2 lane 4 and SH1 lanes 6 (#1149) and 12 (#1155, the first swap and its pattern). Until then, commit the modules and their differential tests on the hand encoder. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1177 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1177", or "Part of #1177" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1177. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Swap layout and the IR check into the hybrid build.

Exclusive ownership:
- new packages/vesper-check-layout/** and packages/vesper-ir-check/** (or modules in @vesper/check-data if lane 4 agrees; say which in the PR)
- crates/vesper-hybrid/** new modules/layout and modules/ir_check only
- new tests/hybrid_layout*.rs, tests/hybrid_ir_check*.rs
- two scoreboard rows in docs/maintenance/self-hosting-plan.md
- docs/parallel-results/self-host-layout-ir-check-swap.md

Implement:
1. Commit the census layout and IR-check translations on @vesper/check-data.
2. Swap them into the hybrid build as non-default modules; the hybrid compiler passes the full suite with them.
3. Differential over the full suite and the corpus with mutants; ABBA and callgrind against Rust inside the hybrid build (including the ABI cost the census did not see).
4. Record the scoreboard rows.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- The hybrid build with both modules passes the full suite on all four platforms.
- Scoreboard rows and report with a "Compiler gaps" section.
- Report: docs/parallel-results/self-host-layout-ir-check-swap.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
