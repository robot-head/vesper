# Checker data package and differential harness

> Self-hosting wave SH2, lane 4. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh2/check-data`. Claim issue: #1176.

Every SH2 port needs the checker's data in Vesper and a harness that runs a Vesper module against the Rust one over the compiler's own inputs. The census built both locally (census gaps C7, C8 and C10): a hash-consed type arena (a type is a `u32` id) with `Display`, `Debug`, `substitute` and `default_type`; a names interner; `World` tables (`constructor`, `mode`, `field`, `vector_words`); a diagnostics table; and a Rust encoder that hands them across as little-endian words. That representation is why the census samples ran at 0.34x to 0.87x of Rust's time; keep it.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh2/README.md (the wave plan and the rules for every lane), docs/parallel-results/self-host-census.md (the census this wave comes from: its samples, numbers and gaps C1-C12), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: crates/vesper-core/src/ast.rs, the `World` type and its lookups in src/check.rs, crates/vesper-diagnostics, docs/agent-prompts/self-host-sh1/06-tree-abi.md, the census harness (census report, "How the samples were run").

The census samples are not in the repository. A copy is in the project files at /mnt/project-files/census/ (sample/src/*.ves, sample/lower/lower.ves, harness/*.rs) when your session can see that folder; if not, the census report describes their design (hash-consed type arena, interned names, flat tables, sorted rows, a state pool) and you rebuild it.

When to start: now, with the hand encoder. Switch the encoder to tree ABI v4 when SH1 lane 6 (#1149) merges. Lanes 5-14 build on this package: merge it as early as you can, then extend it in follow-up PRs. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1176 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1176", or "Part of #1176" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1176. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Checker data package and differential harness.

Exclusive ownership:
- new packages/vesper-check-data/** (@vesper/check-data)
- new tests/self_host_check_harness.rs and tests/self_host_check_data*.rs
- the shard 7 entry for the new package
- docs/parallel-results/self-host-check-data.md

Implement:
1. Commit the census type table, names, world and diagnostics tables as @vesper/check-data, with the accessors the census used and room for the fields SH2 lanes add (each lane adds its own fields in its own PR; this lane owns the package layout).
2. Answer the world queries the census had to precompute in Rust (whether a call is effectful, which operations are partial or overflow) in Vesper.
3. The shared differential harness: load the corpus (every checked single-file program under tests/fixtures and examples and every Loom package, 272 worlds at the census base), encode it, call any Vesper entry with one `Str` input, and compare a digest against a Rust function. Support mutants, a stride for callgrind and ABBA timing, as the census did.
4. Switch the encoder to ABI v4 once #1149 merges, and report the encode cost before and after.
5. Differential: type `Display`/`Debug` text, substitution and layout of every corpus type at 0 differences.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- The differential has 0 differences on all four platforms.
- The harness is documented in the package README for lanes 5-14.
- Report with numbers and a "Compiler gaps" section.
- Report: docs/parallel-results/self-host-check-data.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
